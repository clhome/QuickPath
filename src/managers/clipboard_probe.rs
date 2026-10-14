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
/// 4. 读回文本并登记当时的序列号，作为下一次归属校验的基准。
pub fn probe_panel_path<F>(
    tx: &mut ClipboardTransaction,
    request: F,
) -> Result<PanelProbe, ClipboardTransactionError>
where
    F: FnOnce() -> Option<usize>,
{
    tx.ensure_still_owned()?;
    tx.write_owned_text("")?;

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
            .note_observed_owned_text()
            .and_then(|text| clean_dir_path(&text)),
    })
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

            for required in ["probe_panel_path", "ClipboardTransaction::begin"] {
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
