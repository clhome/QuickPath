#![allow(dead_code)]

use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SendMessageW, WM_COMMAND};

const CM_COPY_SRC_PATH_TO_CLIP: usize = 2029;
const CM_COPY_TRG_PATH_TO_CLIP: usize = 2030;

#[derive(Debug, Clone)]
pub struct TotalCmdFolder {
    pub path: String,
    pub title: String,
    pub is_active: bool,
    pub hwnd: HWND,
}

/// 扫描系统中所有运行的 Total Commander 窗口及其打开的左右路径
pub fn get_totalcmd_folders() -> Vec<TotalCmdFolder> {
    let mut results = Vec::new();

    let class_name: Vec<u16> = "TTOTAL_CMD".encode_utf16().chain(Some(0)).collect();
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
            // 1. 获取源/当前活动面板路径
            let _ = SendMessageW(
                hwnd,
                WM_COMMAND,
                Some(WPARAM(CM_COPY_SRC_PATH_TO_CLIP)),
                Some(LPARAM(0)),
            );
            if let Some(src_path) = crate::win32::clipboard::get_clipboard_text() {
                let clean_path = src_path.trim().trim_end_matches('\\').to_string();
                if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                    results.push(TotalCmdFolder {
                        path: clean_path,
                        title: "Total Commander (活动窗格)".to_string(),
                        is_active: true,
                        hwnd,
                    });
                }
            }

            // 2. 获取目标/副面板路径
            let _ = SendMessageW(
                hwnd,
                WM_COMMAND,
                Some(WPARAM(CM_COPY_TRG_PATH_TO_CLIP)),
                Some(LPARAM(0)),
            );
            if let Some(trg_path) = crate::win32::clipboard::get_clipboard_text() {
                let clean_path = trg_path.trim().trim_end_matches('\\').to_string();
                if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                    results.push(TotalCmdFolder {
                        path: clean_path,
                        title: "Total Commander (副窗格)".to_string(),
                        is_active: false,
                        hwnd,
                    });
                }
            }

            hwnd = FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null()).unwrap_or_default();
        }

        // 扫描完成，立即恢复用户原始剪贴板（恢复失败仅记录，不影响扫描结果）
        if let Err(err) = guard.restore_now() {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_FAILED(totalcmd): {:?}",
                err
            ));
        }
    }

    results
}
