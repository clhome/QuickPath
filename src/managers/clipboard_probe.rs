#![allow(dead_code)]

//! 第三方文件管理器路径探测的**事务化**共享实现（审计报告 P0-03）。
//!
//! Total Commander 与 XYplorer 的探测循环共用同一套流程：
//! 「写前归属校验 → 清空 → 请求管理器写入 → 读回登记」，全部通过
//! [`ClipboardTransaction`] 完成。任何管理器都不得再手工拼装剪贴板快照/守卫，
//! 否则用户新复制的内容可能被旧快照静默覆盖。

use std::path::Path;

use crate::win32::clipboard::{
    ClipboardTransaction, ClipboardTransactionError, ClipboardTransactionOutcome,
};

/// 单次面板探测结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelProbe {
    /// 对端是否在超时内应答（`false` = 超时 / 窗口已退出）
    pub delivered: bool,
    /// 读回、清洗并校验为真实目录的路径
    pub path: Option<String>,
}

/// 事务化探测单个面板路径。
///
/// 1. 写前归属校验：第二次及后续写入前，检出外部改写立即返回 `Err`，
///    调用方必须中止整轮扫描，绝不覆盖用户刚复制的新内容；
/// 2. `QuickPath` 自己写入空文本以清空剪贴板（登记序列号与精确文本）；
/// 3. 请求管理器把路径写入剪贴板；
/// 4. 记下请求开始标记，并在请求返回后**证明更新来源**才登记序列号：请求期间一旦
///    出现非 `is_expected_source` 认可的剪贴板更新，立即返回 `Err(NotOwned)`
///    （审计报告 P0-04 竞态 B，绝不无条件认领最终状态）。
///
/// `is_expected_source` 接收剪贴板 owner 的进程 id，返回 `true` 表示该进程就是本次
/// 请求的目标管理器。管理器实现应传入「目标窗口所属进程 id」的比对闭包。
pub fn probe_panel_path<A, F>(
    tx: &mut ClipboardTransaction,
    is_expected_source: A,
    request: F,
) -> Result<PanelProbe, ClipboardTransactionError>
where
    A: Fn(u32) -> bool,
    F: FnOnce() -> Option<usize>,
{
    tx.ensure_still_owned()?;
    tx.write_owned_text("")?;

    // 请求前记下事件标记：这之后登记的剪贴板更新都属于本次请求窗口
    let marker = tx.request_marker();

    if request().is_none() {
        // 管理器超时 / 窗口已退出：剪贴板仍是本事务的临时写入，归属未变，
        // 最终恢复会把用户原内容原样还回去 —— 这不构成「覆盖当前内容」，
        // 因此必须正常恢复（见报告 §2.4 失败时保守退出）。
        return Ok(PanelProbe {
            delivered: false,
            path: None,
        });
    }

    Ok(PanelProbe {
        delivered: true,
        path: tx
            .note_observed_owned_text_proven(marker, is_expected_source)?
            .and_then(|text| clean_dir_path(&text)),
    })
}

/// 在单个管理器窗口上依次探测两个面板。
///
/// 审计报告 P-05：首个面板未在超时内应答（`delivered == false`）时**立即短路**，
/// 不再发起第二个面板请求 —— 一次失败已足以判定该管理器挂起并进入冷却，
/// 继续请求只会让后台 worker 多等一个 `MESSAGE_TIMEOUT_MS`。
///
/// 返回值为 `(首个面板, 第二个面板)`；短路时第二个为 `None`。
pub fn probe_two_panels<A, F1, F2>(
    tx: &mut ClipboardTransaction,
    is_expected_source: A,
    first: F1,
    second: F2,
) -> Result<(PanelProbe, Option<PanelProbe>), ClipboardTransactionError>
where
    A: Fn(u32) -> bool,
    F1: FnOnce() -> Option<usize>,
    F2: FnOnce() -> Option<usize>,
{
    let first_panel = probe_panel_path(tx, &is_expected_source, first)?;
    if !first_panel.delivered {
        return Ok((first_panel, None));
    }
    let second_panel = probe_panel_path(tx, &is_expected_source, second)?;
    Ok((first_panel, Some(second_panel)))
}

/// 与管理器既有行为完全一致的路径清洗（去首尾空白 + 去掉尾部反斜杠）与目录校验。
fn clean_dir_path(raw: &str) -> Option<String> {
    let clean = raw.trim().trim_end_matches('\\').to_string();
    if clean.is_empty() || !Path::new(&clean).is_dir() {
        return None;
    }
    Some(clean)
}

/// 归属冲突导致探测中止时的统一日志（不记录真实剪贴板内容）。
pub fn log_probe_abort(manager: &str, err: &ClipboardTransactionError) {
    crate::dialog::detector::log_debug(&format!(
        "CLIPBOARD_PROBE_ABORTED({}): {:?}，已中止本轮扫描以保护用户新内容",
        manager, err
    ));
}

/// 剪贴板恢复结局的统一日志（绝不记录真实剪贴板内容，只记结局与原因）。
/// 保留既有 `CLIPBOARD_RESTORE_FAILED(<manager>)` 日志语义。
pub fn log_restore_outcome(manager: &str, outcome: &ClipboardTransactionOutcome) {
    match outcome {
        ClipboardTransactionOutcome::Restored => {}
        ClipboardTransactionOutcome::SkippedForeignChange => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_SKIPPED({}): 检出第三方改写剪贴板，保留用户新内容不覆盖",
                manager
            ));
        }
        ClipboardTransactionOutcome::ReadFailed => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_FAILED({}): 剪贴板归属不可判定（读回/序列号失败），已放弃恢复以避免覆盖",
                manager
            ));
        }
        ClipboardTransactionOutcome::RestoreFailed(err) => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_FAILED({}): {:?}",
                manager, err
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    /// P0-03 架构守卫：两个管理器不得再手工拼装剪贴板快照/守卫/清空/恢复，
    /// 必须复用共享的事务化探测函数（内部使用 `ClipboardTransaction`）。
    #[test]
    fn test_managers_delegate_clipboard_handling_to_transaction_probe() {
        let sources = [
            ("managers/totalcmd.rs", include_str!("totalcmd.rs")),
            ("managers/xyplorer.rs", include_str!("xyplorer.rs")),
        ];

        for (name, source) in sources {
            // 先剔除整行注释，避免架构说明文字本身触发误报
            let code = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");

            for forbidden in [
                "ClipboardSnapshot::capture",
                "ClipboardRestoreGuard::new",
                "clear_clipboard_text",
                "restore_now",
            ] {
                assert!(
                    !code.contains(forbidden),
                    "{} 禁止绕过事务 API 手工操作剪贴板（命中 `{}`）",
                    name,
                    forbidden
                );
            }

            for required in ["probe_two_panels", "ClipboardTransaction::begin"] {
                assert!(
                    code.contains(required),
                    "{} 必须复用统一事务化探测路径（缺少 `{}`）",
                    name,
                    required
                );
            }
        }
    }
}
