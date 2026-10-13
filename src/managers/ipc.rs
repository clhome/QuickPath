#![allow(dead_code)]

//! 第三方文件管理器的有界 IPC 基础设施（审计报告 P-03）。
//!
//! 原实现的三个问题：
//! 1. `SendMessageW` 没有超时：对端（Total Commander / XYplorer）挂起会把调用线程一起拖死；
//! 2. `Command::status()` 没有超时：`dopusrt.exe` 卡住就无限等；
//! 3. 只按窗口类名判定目标，同类名伪装窗口也会收到协议消息。
//!
//! 本模块给出四条约束，供各管理器复用：
//! - 一律 `SendMessageTimeoutW` + `SMTO_ABORTIFHUNG`，超时即视为“本次无结果”；
//! - 外部工具一律 `spawn + try_wait + kill` 有界等待；
//! - 失败/超时按管理器进入冷却窗口，避免每次前台切换都白等一个超时；
//! - 发送协议消息前核对窗口进程镜像。

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
};

/// 子进程创建时隐藏控制台窗口
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 单次跨进程消息的等待上限（必须显著小于用户可感知卡顿阈值）
pub const MESSAGE_TIMEOUT_MS: u32 = 500;

/// 外部工具子进程的等待上限
pub const CHILD_TIMEOUT: Duration = Duration::from_millis(800);

/// 失败/超时后的冷却窗口：窗口期内不再重复尝试该管理器
pub const FAILURE_COOLDOWN: Duration = Duration::from_secs(10);

/// 管理器标识（冷却表键）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Manager {
    Dopus,
    TotalCmd,
    XYplorer,
}

fn cooldown_table() -> &'static Mutex<HashMap<Manager, Instant>> {
    static TABLE: OnceLock<Mutex<HashMap<Manager, Instant>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_table() -> std::sync::MutexGuard<'static, HashMap<Manager, Instant>> {
    cooldown_table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 是否允许本次探测（冷却窗口内返回 false，调用方应直接放弃并给出空结果）
pub fn try_enter(manager: Manager) -> bool {
    let table = lock_table();
    match table.get(&manager) {
        Some(until) => Instant::now() >= *until,
        None => true,
    }
}

/// 记录一次失败/超时，进入冷却窗口
pub fn report_failure(manager: Manager) {
    let mut table = lock_table();
    table.insert(manager, Instant::now() + FAILURE_COOLDOWN);
}

/// 记录一次成功，清除冷却
pub fn report_success(manager: Manager) {
    let mut table = lock_table();
    if table.remove(&manager).is_some() {
        crate::dialog::detector::log_debug("IPC: 管理器探测恢复正常，已解除冷却");
    }
}

/// 有界消息投递。返回 `None` 表示超时/对端无响应，调用方必须按“本次无结果”处理。
pub fn send_message_timeout(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<usize> {
    if hwnd.0.is_null() {
        return None;
    }
    unsafe {
        let mut result = 0usize;
        let ret = SendMessageTimeoutW(
            hwnd,
            msg,
            wparam,
            lparam,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            MESSAGE_TIMEOUT_MS,
            Some(&mut result),
        );
        (ret.0 != 0).then_some(result)
    }
}

/// 有界执行外部工具：超过 `CHILD_TIMEOUT` 直接 kill，绝不无限等待。
/// 返回 true 表示进程在超时前自行退出（不校验退出码，由调用方校验产物）。
pub fn run_bounded(exe: &Path, args: &[String]) -> bool {
    let Ok(mut child) = Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    else {
        return false;
    };

    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// 核对窗口所属进程镜像是否为预期程序（大小写不敏感，只比文件名）。
pub fn window_process_matches(hwnd: HWND, expected_exe_names: &[&str]) -> bool {
    let (_pid, name) = crate::dialog::detector::get_window_process_info(hwnd);
    process_name_matches(&name, expected_exe_names)
}

fn process_name_matches(actual: &str, expected_exe_names: &[&str]) -> bool {
    if actual.is_empty() {
        return false;
    }
    expected_exe_names
        .iter()
        .any(|expected| actual.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_name_matching_is_case_insensitive_and_filename_only() {
        let expected = ["totalcmd.exe", "totalcmd64.exe"];
        assert!(process_name_matches("TOTALCMD64.EXE", &expected));
        assert!(process_name_matches("totalcmd.exe", &expected));
        assert!(!process_name_matches("totalcmd64.exe.bak", &expected));
        assert!(!process_name_matches("", &expected));
        assert!(!process_name_matches("evil.exe", &expected));
    }

    #[test]
    fn test_failure_cooldown_blocks_then_success_clears() {
        let manager = Manager::XYplorer;
        // 先确保干净状态
        report_success(manager);
        assert!(try_enter(manager), "无故障时应允许探测");

        report_failure(manager);
        assert!(!try_enter(manager), "冷却窗口内必须拒绝重复探测");

        report_success(manager);
        assert!(try_enter(manager), "成功后必须立即解除冷却");
    }

    #[test]
    fn test_run_bounded_kills_long_running_child() {
        // 用系统自带的 ping 模拟“卡住的外部工具”：等待上限之外的进程必须被 kill
        let Some(ping) = crate::win32::trusted::system32_tool("ping.exe") else {
            return; // 极端环境缺少 ping 时跳过（不影响被测语义）
        };
        let start = Instant::now();
        let ok = run_bounded(&ping, &["-n".to_string(), "30".to_string(), "127.0.0.1".to_string()]);
        assert!(!ok, "超时子进程必须返回失败");
        assert!(
            start.elapsed() < CHILD_TIMEOUT + Duration::from_secs(2),
            "必须在等待上限附近返回，实际 {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn test_run_bounded_returns_true_for_fast_child() {
        let Some(cmd) = crate::win32::trusted::system32_tool("cmd.exe") else {
            return;
        };
        let args = vec!["/c".to_string(), "exit".to_string(), "0".to_string()];
        assert!(run_bounded(&cmd, &args), "快速退出的子进程应返回成功");
    }
}
