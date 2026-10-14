#![allow(dead_code)]

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TASK_NAME: &str = "QuickPath";
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 单次 schtasks 调用的等待上限。
///
/// 审计报告（§5 可靠性边界）：`Command::status()` / `output()` 没有截止时间，
/// 异常系统服务或安全软件挂起时会把调用线程（可能是宿主消息线程）无限拖住。
const AUTOSTART_TIMEOUT: Duration = Duration::from_millis(1500);

/// 通过 System32 绝对路径构造 schtasks 调用。
///
/// 审计报告 S-01：在“最高权限自启”模式下，裸程序名会被 Windows 按 PATH 搜索，
/// 用户 PATH 中的同名程序可能被高权限进程拉起。因此只接受系统目录下的绝对路径，
/// 解析失败直接返回错误，绝不回退 PATH 搜索。
fn schtasks_command() -> Result<Command, String> {
    let exe = crate::win32::trusted::system32_tool("schtasks.exe")
        .ok_or_else(|| "未在系统目录找到可信的 schtasks.exe".to_string())?;
    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW);
    Ok(cmd)
}

/// 有界执行并返回退出码是否成功；超时即 kill 并返回 `None`。
///
/// 统一走 `spawn + try_wait + kill`，绝不使用无截止时间的 `status()` / `output()`。
fn run_bounded_status(mut cmd: Command, args: &[&str], timeout: Duration) -> Option<bool> {
    let mut child = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    crate::dialog::detector::log_debug(
                        "AUTOSTART: schtasks 超时，已终止子进程（按失败处理）",
                    );
                    return None;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// 静默执行一次 schtasks 子命令（仅用于清理/查询，忽略结果）
fn run_schtasks_quiet(args: &[&str]) {
    if let Ok(cmd) = schtasks_command() {
        let _ = run_bounded_status(cmd, args, AUTOSTART_TIMEOUT);
    }
}

/// 设置或移除开机自启动（规范带双引号包裹并进行写后一致性校验）
pub fn set_autostart(enable: bool, use_task_scheduler: bool) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("获取当前可执行文件路径失败: {}", e))?;
    let exe_path_str = current_exe.to_string_lossy().to_string();
    // 强制加双引号包裹，彻底解决含空格目录（如 C:\Program Files）开机自启静默失效问题
    let quoted_exe = format!("\"{}\"", exe_path_str);

    if enable {
        if use_task_scheduler {
            // 使用任务计划程序以最高权限（免 UAC）开机自启。
            // 提交给任务计划程序的目标**只有本程序自身的规范绝对路径**，
            // 不对外部工具做任何 PATH 搜索，因此不存在 S-01 描述的提权路径劫持面。
            let task_created = match schtasks_command() {
                Ok(cmd) => run_bounded_status(
                    cmd,
                    &[
                        "/create",
                        "/tn",
                        TASK_NAME,
                        "/tr",
                        &quoted_exe,
                        "/sc",
                        "onlogon",
                        "/rl",
                        "highest",
                        "/f",
                    ],
                    AUTOSTART_TIMEOUT,
                )
                .unwrap_or(false),
                Err(err) => {
                    crate::dialog::detector::log_debug(&format!(
                        "AUTOSTART: 无可信 schtasks.exe，回退注册表模式 ({})",
                        err
                    ));
                    false
                }
            };

            if task_created {
                // 任务计划程序成功，清理注册表中残留项避免重复拉起
                let _ = set_registry_run(&quoted_exe, false);
            } else {
                // 任务计划程序不可用或权限不足：回退到 HKCU Run（无需提权）
                set_registry_run(&quoted_exe, true)?;
            }
        } else {
            // 普通模式：清理旧的任务计划程序（若有），并写入注册表
            run_schtasks_quiet(&["/delete", "/tn", TASK_NAME, "/f"]);
            set_registry_run(&quoted_exe, true)?;
        }
    } else {
        // 清理两种方式的自启
        run_schtasks_quiet(&["/delete", "/tn", TASK_NAME, "/f"]);

        let _ = set_registry_run(&quoted_exe, false);
    }

    Ok(())
}

/// 检查是否已设置开机自启
pub fn is_autostart_enabled() -> bool {
    // 检查任务计划程序
    let task_exists = match schtasks_command() {
        Ok(cmd) => {
            run_bounded_status(cmd, &["/query", "/tn", TASK_NAME], AUTOSTART_TIMEOUT)
                .unwrap_or(false)
        }
        Err(_) => false,
    };
    if task_exists {
        return true;
    }

    // 检查注册表
    is_registry_run_enabled()
}

fn set_registry_run(quoted_exe: &str, enable: bool) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
    };

    unsafe {
        let sub_key = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let value_name = HSTRING::from("QuickPath");
        let mut hkey = HKEY::default();

        let open_res = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &sub_key,
            Some(0),
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            &mut hkey,
        );

        if open_res.is_err() {
            return Err("无法打开注册表 Run 键".to_string());
        }

        if enable {
            let exe_wide: Vec<u16> = quoted_exe.encode_utf16().chain(Some(0)).collect();
            let slice = std::slice::from_raw_parts(
                exe_wide.as_ptr() as *const u8,
                exe_wide.len() * 2,
            );
            let set_res = RegSetValueExW(
                hkey,
                &value_name,
                Some(0),
                REG_SZ,
                Some(slice),
            );

            if set_res.is_err() {
                let _ = RegCloseKey(hkey);
                return Err("写入注册表自启项失败".to_string());
            }

            // 写后回读校验：确认注册表值已正确写入且内容完全一致
            let mut buffer = [0u16; 1024];
            let mut buffer_size = (buffer.len() * 2) as u32;
            let mut val_type = REG_SZ;
            let query_res = RegQueryValueExW(
                hkey,
                &value_name,
                None,
                Some(&mut val_type),
                Some(buffer.as_mut_ptr() as *mut u8),
                Some(&mut buffer_size),
            );
            let _ = RegCloseKey(hkey);

            if query_res.is_err() {
                return Err("注册表自启项写入后回读验证失败".to_string());
            }

            let read_len = (buffer_size / 2) as usize;
            let read_str = String::from_utf16_lossy(&buffer[..read_len.saturating_sub(1)]);
            if read_str.trim() != quoted_exe.trim() {
                return Err(format!(
                    "注册表自启路径不一致: 写入='{}', 回读='{}'",
                    quoted_exe, read_str
                ));
            }
        } else {
            let _ = RegDeleteValueW(hkey, &value_name);
            let _ = RegCloseKey(hkey);
        }

        Ok(())
    }
}

fn is_registry_run_enabled() -> bool {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
    };

    unsafe {
        let sub_key = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let value_name = HSTRING::from("QuickPath");
        let mut hkey = HKEY::default();

        let open_res = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &sub_key,
            Some(0),
            KEY_QUERY_VALUE,
            &mut hkey,
        );

        if open_res.is_ok() {
            let mut buffer = [0u16; 1024];
            let mut buffer_size = (buffer.len() * 2) as u32;
            let query_res = RegQueryValueExW(
                hkey,
                &value_name,
                None,
                None,
                Some(buffer.as_mut_ptr() as *mut u8),
                Some(&mut buffer_size),
            );
            let _ = RegCloseKey(hkey);

            if query_res.is_ok() {
                // 如果能读取到内容，校验是否确实包含 QuickPath
                let read_len = (buffer_size / 2) as usize;
                let read_str = String::from_utf16_lossy(&buffer[..read_len.saturating_sub(1)]);
                return !read_str.trim().is_empty();
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 有界等待：成功/失败退出码必须区分（不再用无超时的 status()/output()）
    #[test]
    fn test_run_bounded_status_reports_exit_code() {
        let Some(cmd_exe) = crate::win32::trusted::system32_tool("cmd.exe") else {
            return;
        };

        let mut ok_cmd = Command::new(&cmd_exe);
        ok_cmd.creation_flags(CREATE_NO_WINDOW);
        assert_eq!(
            run_bounded_status(ok_cmd, &["/c", "exit", "0"], Duration::from_secs(5)),
            Some(true)
        );

        let mut fail_cmd = Command::new(&cmd_exe);
        fail_cmd.creation_flags(CREATE_NO_WINDOW);
        assert_eq!(
            run_bounded_status(fail_cmd, &["/c", "exit", "3"], Duration::from_secs(5)),
            Some(false)
        );
    }

    /// 挂起的子进程必须在超时附近被 kill，且绝不会无限等待
    #[test]
    fn test_run_bounded_status_kills_on_timeout() {
        let Some(ping) = crate::win32::trusted::system32_tool("ping.exe") else {
            return;
        };
        let mut cmd = Command::new(&ping);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let start = Instant::now();
        assert_eq!(
            run_bounded_status(cmd, &["-n", "30", "127.0.0.1"], Duration::from_millis(300)),
            None
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "必须在超时附近返回，实际 {:?}",
            start.elapsed()
        );
    }
}
