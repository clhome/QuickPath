#![allow(dead_code)]

use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, SendMessageW, WM_COMMAND,
};

const CM_COPY_SRC_PATH_TO_CLIP: usize = 2029;
const CM_COPY_TRG_PATH_TO_CLIP: usize = 2030;
const CF_UNICODETEXT: u32 = 13;

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
        while !hwnd.0.is_null() {
            // 备份用户当前的剪贴板内容
            let backup_clip = get_clipboard_text();

            // 1. 获取源/当前活动面板路径
            let _ = SendMessageW(
                hwnd,
                WM_COMMAND,
                Some(WPARAM(CM_COPY_SRC_PATH_TO_CLIP)),
                Some(LPARAM(0)),
            );
            if let Some(src_path) = get_clipboard_text() {
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
            if let Some(trg_path) = get_clipboard_text() {
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

            // 还原用户的剪贴板
            if let Some(ref text) = backup_clip {
                set_clipboard_text(text);
            } else {
                let _ = OpenClipboard(None);
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }

            hwnd = FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null()).unwrap_or_default();
        }
    }

    results
}

fn get_clipboard_text() -> Option<String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }

        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            let _ = CloseClipboard();
            return None;
        }

        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_err() {
            let _ = CloseClipboard();
            return None;
        }

        let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.unwrap().0 as _));
        if ptr.is_null() {
            let _ = CloseClipboard();
            return None;
        }

        let wide_slice = std::slice::from_raw_parts(ptr as *const u16, 4096);
        let len = wide_slice.iter().position(|&c| c == 0).unwrap_or(0);
        let result = String::from_utf16_lossy(&wide_slice[..len]);

        let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(ptr as _));
        let _ = CloseClipboard();

        Some(result)
    }
}

fn set_clipboard_text(text: &str) {
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();

        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes_len = wide.len() * 2;

        if let Ok(hglobal) = GlobalAlloc(GHND, bytes_len) {
            let ptr = GlobalLock(hglobal);
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes_len);
                let _ = GlobalUnlock(hglobal);
                let _ = SetClipboardData(
                    CF_UNICODETEXT,
                    Some(windows::Win32::Foundation::HANDLE(hglobal.0 as _)),
                );
            }
        }

        let _ = CloseClipboard();
    }
}
