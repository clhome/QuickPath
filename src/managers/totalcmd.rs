#![allow(dead_code)]

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::WM_COMMAND;

use super::clipboard_probe::{log_probe_abort, log_restore_outcome, probe_panel_path};
use super::explicit_probe::{ExplicitProbeOutcome, TargetWindow, verify_target};
use super::ipc::{self, Manager};
use crate::win32::clipboard::ClipboardTransaction;

const CM_COPY_SRC_PATH_TO_CLIP: usize = 2029;

/// Total Commander 主窗口类名（单一定义，供扫描/显式探测共用）
pub const WINDOW_CLASS: &str = "TTOTAL_CMD";

/// TTOTAL_CMD 窗口必须属于以下进程镜像之一，否则视为同类名伪装窗口，不发送协议消息
pub const EXPECTED_PROCESS_NAMES: [&str; 3] =
    ["totalcmd.exe", "totalcmd64.exe", "totalcmd_unicode.exe"];

/// 对一个**已由显式触发解析并通过身份核验**的目标窗口做一次事务化探测（审计报告 U-01）。
///
/// 与旧的后台枚举版本的关键差异：
/// - 只对用户指定的**那一个**窗口投递协议消息（不再枚举系统内所有同类窗口）；
/// - 投递前与投递时都复核窗口身份（句柄 + PID + 类名 + 进程镜像 + 可见），
///   窗口被关闭 / HWND 被复用 / 进程重启 / 焦点变化都会在写下任何剪贴板内容前取消；
/// - 只读取**活动窗格**路径（用户要的是「当前路径」，少一次剪贴板往返就少一分风险）；
/// - 剪贴板事务的全部 fail-closed 检查保持不变（唯一事件 / sequence / owner / 事件环）。
pub fn probe_explicit_target(target: &TargetWindow) -> ExplicitProbeOutcome {
    if !verify_target(target.manager, target.hwnd_raw, target.pid) {
        return ExplicitProbeOutcome::NoForegroundTarget;
    }

    // 安全守卫：探测前开启剪贴板事务（完整快照，含文件列表 CF_HDROP、截图位图等）。
    // 若剪贴板被占用或关键格式无法保真备份，宁可放弃扫描，绝不破坏用户剪贴板！
    let mut tx = match ClipboardTransaction::begin() {
        Ok(tx) => tx,
        Err(err) => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_PROBE_UNAVAILABLE(totalcmd): 无法开启剪贴板事务({:?})，本次显式探测未执行",
                err
            ));
            return ExplicitProbeOutcome::NoPath;
        }
    };

    let mut delivered = false;
    let mut timed_out = false;
    let mut path = None;

    match probe_panel_path(
        &mut tx,
        |pid| pid != 0 && pid == target.pid,
        || send_path_copy_request(target, CM_COPY_SRC_PATH_TO_CLIP),
    ) {
        Ok(panel) => {
            if panel.delivered {
                delivered = true;
            } else {
                timed_out = true;
            }
            path = panel.path;
        }
        Err(err) => {
            // 检出外部改写：立即中止，绝不覆盖用户新复制的内容
            log_probe_abort("totalcmd", &err);
        }
    }

    let outcome = tx.restore_if_still_owned();
    log_restore_outcome("totalcmd", &outcome);

    if timed_out {
        crate::dialog::detector::log_debug("IPC: Total Commander 响应超时，进入冷却");
        ipc::report_failure(Manager::TotalCmd);
    } else if delivered {
        ipc::report_success(Manager::TotalCmd);
    }

    match path {
        Some(path) => ExplicitProbeOutcome::Path(path),
        None => ExplicitProbeOutcome::NoPath,
    }
}

/// 投递前复核目标身份：任一不一致（窗口关闭 / 易主 / 进程重启）即返回 `None`，
/// 让事务把本轮判定为「未投递」而不是「已投递但无结果」。
fn send_path_copy_request(target: &TargetWindow, command: usize) -> Option<usize> {
    if !verify_target(target.manager, target.hwnd_raw, target.pid) {
        crate::dialog::detector::log_debug(
            "CLIPBOARD_PROBE_CANCELED(totalcmd): 目标窗口身份已变化，取消本次协议消息投递",
        );
        return None;
    }
    ipc::send_message_timeout(target.hwnd(), WM_COMMAND, WPARAM(command), LPARAM(0))
}
