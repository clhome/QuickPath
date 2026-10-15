#![allow(dead_code)]

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::WM_COPYDATA;

use super::clipboard_probe::{log_probe_abort, log_restore_outcome, probe_panel_path};
use super::explicit_probe::{ExplicitProbeOutcome, TargetWindow, verify_target};
use super::ipc::{self, Manager};
use crate::win32::clipboard::ClipboardTransaction;

const XYPLORER_COPYDATA_FLAG: usize = 4194305;

/// XYplorer 主窗口类名（单一定义，供扫描/显式探测共用）
pub const WINDOW_CLASS: &str = "ThunderRT6FormDC";

/// ThunderRT6FormDC 窗口必须属于以下进程镜像之一，否则视为同类名伪装窗口，不发送协议消息
pub const EXPECTED_PROCESS_NAMES: [&str; 1] = ["xyplorer.exe"];

/// 对一个**已由显式触发解析并通过身份核验**的目标窗口做一次事务化探测（审计报告 U-01）。
///
/// 与旧的后台枚举版本的关键差异同 `totalcmd::probe_explicit_target`：
/// 只投递给用户指定的那一个窗口、投递前后复核身份、只读取活动窗格路径，
/// 剪贴板事务的全部 fail-closed 检查保持不变。
pub fn probe_explicit_target(target: &TargetWindow) -> ExplicitProbeOutcome {
    if !verify_target(target.manager, target.hwnd_raw, target.pid) {
        return ExplicitProbeOutcome::NoForegroundTarget;
    }

    let mut tx = match ClipboardTransaction::begin() {
        Ok(tx) => tx,
        Err(err) => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_PROBE_UNAVAILABLE(xyplorer): 无法开启剪贴板事务({:?})，本次显式探测未执行",
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
        || send_path_copy_request(target, "::copytext get('path', a);"),
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
            log_probe_abort("xyplorer", &err);
        }
    }

    let outcome = tx.restore_if_still_owned();
    log_restore_outcome("xyplorer", &outcome);

    if timed_out {
        crate::dialog::detector::log_debug("IPC: XYplorer 响应超时，进入冷却");
        ipc::report_failure(Manager::XYplorer);
    } else if delivered {
        ipc::report_success(Manager::XYplorer);
    }

    match path {
        Some(path) => ExplicitProbeOutcome::Path(path),
        None => ExplicitProbeOutcome::NoPath,
    }
}

/// 投递前复核目标身份：任一不一致即取消（绝不向易主/已关闭的窗口发脚本）。
fn send_path_copy_request(target: &TargetWindow, script: &str) -> Option<usize> {
    if !verify_target(target.manager, target.hwnd_raw, target.pid) {
        crate::dialog::detector::log_debug(
            "CLIPBOARD_PROBE_CANCELED(xyplorer): 目标窗口身份已变化，取消本次脚本投递",
        );
        return None;
    }
    unsafe { send_xyplorer_script(target.hwnd(), script) }
}

/// 有界投递 XYplorer 脚本：使用 `SendMessageTimeoutW(SMTO_BLOCK)`，
/// 保证 `COPYDATASTRUCT` 缓冲区在目标读取期间始终有效，同时对端挂起时能及时脱身。
unsafe fn send_xyplorer_script(hwnd: HWND, script: &str) -> Option<usize> {
    let wide_script: Vec<u16> = script.encode_utf16().chain(Some(0)).collect();
    let cb_data = (wide_script.len() * 2) as u32;

    let copy_data = COPYDATASTRUCT {
        dwData: XYPLORER_COPYDATA_FLAG,
        cbData: cb_data,
        lpData: wide_script.as_ptr() as *mut _,
    };

    ipc::send_message_timeout(
        hwnd,
        WM_COPYDATA,
        WPARAM(0),
        LPARAM(&copy_data as *const _ as isize),
    )
}
