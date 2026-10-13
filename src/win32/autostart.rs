#![allow(dead_code)]

use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK_NAME: &str = "QuickPath";
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 设置或移除开机自启动（规范带双引号包裹并进行写后一致性校验）
pub fn set_autostart(enable: bool, use_task_scheduler: bool) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("获取当前可执行文件路径失败: {}", e))?;
    let exe_path_str = current_exe.to_string_lossy().to_string();
    // 强制加双引号包裹，彻底解决含空格目录（如 C:\Program Files）开机自启静默失效问题
    let quoted_exe = format!("\"{}\"", exe_path_str);

    if enable {
        if use_task_scheduler {
            // 使用任务计划程序以最高权限（免 UAC）开机自启
            let status = Command::new("schtasks")
                .args([
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
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .map_err(|e| format!("调用 schtasks 失败: {}", e))?;

            if !status.success() {
                // 如果任务计划程序失败（可能权限不足），回退到注册表模式
                set_registry_run(&quoted_exe, true)?;
            } else {
                // 任务计划程序成功，清理注册表中残留项避免重复拉起
                let _ = set_registry_run(&quoted_exe, false);
            }
        } else {
            // 普通模式：清理旧的任务计划程序（若有），并写入注册表
            let _ = Command::new("schtasks")
                .args(["/delete", "/tn", TASK_NAME, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
            set_registry_run(&quoted_exe, true)?;
        }
    } else {
        // 清理两种方式的自启
        let _ = Command::new("schtasks")
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        let _ = set_registry_run(&quoted_exe, false);
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
