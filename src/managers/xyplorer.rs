#![allow(dead_code)]

use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SendMessageW, WM_COPYDATA};

const XYPLORER_COPYDATA_FLAG: usize = 4194305;

#[derive(Debug, Clone)]
pub struct XYplorerFolder {
    pub path: String,
    pub title: String,
    pub is_active: bool,
    pub hwnd: HWND,
}

/// 扫描系统中所有运行的 XYplorer 窗口及其打开的路径
pub fn get_xyplorer_folders() -> Vec<XYplorerFolder> {
    let mut results = Vec::new();

    let class_name: Vec<u16> = "ThunderRT6FormDC".encode_utf16().chain(Some(0)).collect();
    let class_pcwstr = PCWSTR(class_name.as_ptr());

    unsafe {
        let mut hwnd = FindWindowExW(None, None, class_pcwstr, PCWSTR::null()).unwrap_or_default();
        if hwnd.0.is_null() {
            return results;
        }

        // P0 安全守卫：在枚举前完整快照剪贴板（含文件列表 CF_HDROP、截图位图等）
        // 若剪贴板被占用或关键格式无法保真备份，宁可放弃扫描，绝不破坏用户剪贴板！
        let snapshot = match crate::win32::clipboard::ClipboardSnapshot::capture() {
            Ok(s) => s,
            Err(_) => return results,
        };
        let mut guard = crate::win32::clipboard::ClipboardRestoreGuard::new(snapshot);

        while !hwnd.0.is_null() {
            // 1. 获取活动窗格路径
            send_xyplorer_script(hwnd, "::copytext get('path', a);");
            if let Some(path) = crate::win32::clipboard::get_clipboard_text() {
                let clean_path = path.trim().trim_end_matches('\\').to_string();
                if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                    results.push(XYplorerFolder {
                        path: clean_path,
                        title: "XYplorer (活动窗格)".to_string(),
                        is_active: true,
                        hwnd,
                    });
                }
            }

            // 2. 获取非活动窗格路径
            send_xyplorer_script(hwnd, "::copytext get('path', i);");
            if let Some(path) = crate::win32::clipboard::get_clipboard_text() {
                let clean_path = path.trim().trim_end_matches('\\').to_string();
                if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                    if !results.iter().any(|r| r.path == clean_path) {
                        results.push(XYplorerFolder {
                            path: clean_path,
                            title: "XYplorer (副窗格)".to_string(),
                            is_active: false,
                            hwnd,
                        });
                    }
                }
            }

            hwnd = FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null()).unwrap_or_default();
        }

        // 扫描完成，立即恢复用户原始剪贴板
        guard.restore_now();
    }

    results
}

unsafe fn send_xyplorer_script(hwnd: HWND, script: &str) -> LRESULT {
    let wide_script: Vec<u16> = script.encode_utf16().chain(Some(0)).collect();
    let cb_data = (wide_script.len() * 2) as u32;

    let copy_data = COPYDATASTRUCT {
        dwData: XYPLORER_COPYDATA_FLAG,
        cbData: cb_data,
        lpData: wide_script.as_ptr() as *mut _,
    };

    unsafe {
        SendMessageW(
            hwnd,
            WM_COPYDATA,
            Some(WPARAM(0)),
            Some(LPARAM(&copy_data as *const _ as isize)),
        )
    }
}
