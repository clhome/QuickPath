#![allow(dead_code)]

use std::path::Path;
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{IShellWindows, ShellWindows};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindowVisible};

#[derive(Debug, Clone)]
pub struct ExplorerFolder {
    pub path: String,
    pub title: String,
    pub is_active: bool,
    pub hwnd: HWND,
}

/// 枚举所有 Windows 资源管理器打开的路径（完美兼容 Win10 经典窗口与 Win11 多标签页）
pub fn get_explorer_folders() -> Vec<ExplorerFolder> {
    let mut results = Vec::new();

    unsafe {
        // 保证当前线程已初始化 COM
        let _ = CoInitializeEx(
            None,
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        );

        let fg_hwnd = GetForegroundWindow();

        let shell_windows: Result<IShellWindows, _> = CoCreateInstance(
            &ShellWindows,
            None,
            CLSCTX_ALL,
        );

        if let Ok(shell_windows) = shell_windows {
            if let Ok(count) = shell_windows.Count() {
                for i in 0..count {
                    let index = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&index) {
                        if let Ok(browser) = disp.cast::<windows::Win32::UI::Shell::IWebBrowser2>() {
                            if let Ok(url_bstr) = browser.LocationURL() {
                                let url = url_bstr.to_string();
                                if let Some(path) = parse_file_url(&url) {
                                    if Path::new(&path).is_dir() {
                                        let title = browser
                                            .LocationName()
                                            .map(|n| n.to_string())
                                            .unwrap_or_else(|_| {
                                                Path::new(&path)
                                                    .file_name()
                                                    .map(|f| f.to_string_lossy().to_string())
                                                    .unwrap_or_else(|| path.clone())
                                            });

                                        let hwnd = browser
                                            .HWND()
                                            .map(|h| HWND(h.0 as *mut _))
                                            .unwrap_or_default();

                                        let mut is_active = false;
                                        if let Ok(visible) = browser.Visible() {
                                            is_active = visible.into();
                                        }
                                        if !hwnd.0.is_null() {
                                            if hwnd == fg_hwnd {
                                                is_active = true;
                                            } else if is_active && !IsWindowVisible(hwnd).as_bool() {
                                                is_active = false;
                                            }
                                        }

                                        // 若当前是第一个，默认也赋予较高优先级
                                        if results.is_empty() {
                                            is_active = true;
                                        }

                                        results.push(ExplorerFolder {
                                            path,
                                            title,
                                            is_active,
                                            hwnd,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    results
}

/// 解析 file:/// 形式的本地路径或网络共享路径
fn parse_file_url(url: &str) -> Option<String> {
    if !url.starts_with("file:///") {
        return None;
    }

    let raw = &url[8..]; // 移除 "file:///"
    let decoded = url_decode(raw);
    let path_str = decoded.replace('/', "\\");

    if path_str.len() >= 2 && &path_str[1..2] == ":" {
        Some(path_str)
    } else if path_str.starts_with('\\') {
        Some(format!("\\{}", path_str))
    } else {
        Some(path_str)
    }
}

/// 快速轻量 URL 百分号解码
fn url_decode(input: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = input.bytes();

    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                if let Ok(val) = u8::from_str_radix(
                    &format!("{}{}", h1 as char, h2 as char),
                    16,
                ) {
                    bytes.push(val);
                    continue;
                }
            }
        }
        bytes.push(b);
    }

    String::from_utf8_lossy(&bytes).to_string()
}
