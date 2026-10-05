#![allow(dead_code)]

use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData, COPYDATASTRUCT,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, SendMessageW, WM_COPYDATA,
};

const CF_UNICODETEXT: u32 = 13;
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
        while !hwnd.0.is_null() {
            let backup_clip = get_clipboard_text();

            // 1. 获取活动窗格路径
            send_xyplorer_script(hwnd, "::copytext get('path', a);");
            if let Some(path) = get_clipboard_text() {
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
            if let Some(path) = get_clipboard_text() {
                let clean_path = path.trim().trim_end_matches('\\').to_string();
                if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                    // 避免重复
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

            // 还原剪贴板
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
