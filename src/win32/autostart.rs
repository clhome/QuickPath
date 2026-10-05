#![allow(dead_code)]

use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK_NAME: &str = "QuickPath";
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 设置或移除开机自启动
pub fn set_autostart(enable: bool, use_task_scheduler: bool) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("获取当前可执行文件路径失败: {}", e))?;
    let exe_path_str = current_exe.to_string_lossy().to_string();

    if enable {
        if use_task_scheduler {
            // 使用任务计划程序以最高权限（免 UAC）开机自启
            let status = Command::new("schtasks")
                .args([
                    "/create",
                    "/tn",
                    TASK_NAME,
                    "/tr",
                    &format!("\"{}\"", exe_path_str),
                    "/sc",
                    "onlogon",
                    "/rl",
                    "highest",
                    "/f",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .map_err(|e| format!("调用 schtasks 失败: {}", e))?;

            if !status.success() {
                // 如果任务计划程序失败（可能权限不足），回退到注册表模式
                set_registry_run(&exe_path_str, true)?;
            }
        } else {
            set_registry_run(&exe_path_str, true)?;
        }
    } else {
        // 清理两种方式的自启
        let _ = Command::new("schtasks")
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        let _ = set_registry_run(&exe_path_str, false);
    }

    Ok(())
}

/// 检查是否已设置开机自启
pub fn is_autostart_enabled() -> bool {
    // 检查任务计划程序
    let task_check = Command::new("schtasks")
        .args(["/query", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    if let Ok(out) = task_check {
        if out.status.success() {
            return true;
        }
    }

    // 检查注册表
    is_registry_run_enabled()
}

fn set_registry_run(exe_path: &str, enable: bool) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_SZ,
    };

    unsafe {
        let sub_key = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let value_name = HSTRING::from("QuickPath");
        let mut hkey = HKEY::default();

        let open_res = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &sub_key,
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        );

        if open_res.is_err() {
            return Err("无法打开注册表 Run 键".to_string());
        }

        if enable {
            let exe_wide: Vec<u16> = exe_path.encode_utf16().chain(Some(0)).collect();
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
            let _ = RegCloseKey(hkey);
            if set_res.is_err() {
                return Err("写入注册表自启项失败".to_string());
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
            let query_res = RegQueryValueExW(hkey, &value_name, None, None, None, None);
            let _ = RegCloseKey(hkey);
            return query_res.is_ok();
        }

        false
    }
}
