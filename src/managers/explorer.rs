#![allow(dead_code)]

use std::path::Path;
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, IServiceProvider,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{IShellBrowser, IShellWindows, IWebBrowser2, ShellWindows};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetClassNameW, GetForegroundWindow, GetWindow, IsWindow, IsWindowVisible, GW_HWNDNEXT,
};

#[derive(Debug, Clone)]
pub struct ExplorerFolder {
    pub path: String,
    pub title: String,
    pub is_active: bool,
    pub hwnd: HWND,
}

/// 枚举所有 Windows 资源管理器打开的路径（完美兼容 Win10 经典窗口与 Win11 多标签页原生识别）
pub fn get_explorer_folders() -> Vec<ExplorerFolder> {
    let mut results = Vec::new();

    unsafe {
        // 保证当前线程已初始化 COM
        let _ = CoInitializeEx(
            None,
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        );

        let top_explorer = get_top_visible_explorer_hwnd();
        let top_active_tab = top_explorer.and_then(get_active_tab_hwnd);

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
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser
                                .HWND()
                                .map(|h| HWND(h.0 as *mut _))
                                .unwrap_or_default();

                            // 严格过滤无效窗口与不可见窗口
                            if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() || !IsWindowVisible(hwnd).as_bool() {
                                continue;
                            }

                            if let Ok(url_bstr) = browser.LocationURL() {
                                let url = url_bstr.to_string();
                                if let Some(path) = parse_file_url(&url) {
                                    if (path.len() >= 2 && &path[1..2] == ":") || path.starts_with("\\\\") {
                                        let title = browser
                                            .LocationName()
                                            .map(|n| n.to_string())
                                            .unwrap_or_else(|_| {
                                                Path::new(&path)
                                                    .file_name()
                                                    .map(|f| f.to_string_lossy().to_string())
                                                    .unwrap_or_else(|| path.clone())
                                            });

                                        // 核心：基于 ShellTabWindowClass 精准识别 Win11 多标签页活动状态
                                        let mut is_active = false;
                                        let tab_sub_hwnd = get_browser_tab_hwnd(&browser);

                                        if let Some(top_exp) = top_explorer {
                                            if hwnd == top_exp {
                                                if let (Some(my_tab), Some(active_tab)) = (tab_sub_hwnd, top_active_tab) {
                                                    // 多标签页窗口：严格匹配 Z-Order 顶层活跃 Tab
                                                    if my_tab == active_tab {
                                                        is_active = true;
                                                    }
                                                } else if top_active_tab.is_none() {
                                                    // 经典单窗口或无标签页模式：顶级窗口匹配即为 active
                                                    is_active = true;
                                                }
                                            }
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

    // 排序保障：当前处于激活状态的 Tab 绝对置顶
    results.sort_by(|a, b| b.is_active.cmp(&a.is_active));

    results
}

/// 尝试从 IWebBrowser2 提取其对应的 ShellTabWindowClass 句柄
pub fn get_browser_tab_hwnd(browser: &IWebBrowser2) -> Option<HWND> {
    unsafe {
        if let Ok(sp) = browser.cast::<IServiceProvider>() {
            if let Ok(sb) = sp.QueryService::<IShellBrowser>(&IShellBrowser::IID) {
                if let Ok(tab_hwnd) = sb.GetWindow() {
                    if !tab_hwnd.0.is_null() && IsWindow(Some(tab_hwnd)).as_bool() {
                        return Some(tab_hwnd);
                    }
                }
            }
        }
        None
    }
}

/// 获取指定资源管理器顶级窗口当前激活的 Tab 句柄 (Z-Order 最顶层的 ShellTabWindowClass)
pub fn get_active_tab_hwnd(top_hwnd: HWND) -> Option<HWND> {
    unsafe {
        if top_hwnd.0.is_null() || !IsWindow(Some(top_hwnd)).as_bool() {
            return None;
        }
        FindWindowExW(Some(top_hwnd), None, windows::core::w!("ShellTabWindowClass"), None)
            .ok()
            .filter(|h| !h.0.is_null() && IsWindow(Some(*h)).as_bool())
    }
}

/// 检查指定的窗口句柄是否为资源管理器顶级窗口 (CabinetWClass)
pub fn is_cabinet_window(hwnd: HWND) -> bool {
    unsafe {
        if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        let mut buf = [0u16; 32];
        let len = GetClassNameW(hwnd, &mut buf);
        if len > 0 {
            let cls = String::from_utf16_lossy(&buf[..len as usize]);
            return cls == "CabinetWClass";
        }
        false
    }
}

/// 寻找当前或 Z-Order 最靠近顶层的可见资源管理器窗口 (CabinetWClass)
pub fn get_top_visible_explorer_hwnd() -> Option<HWND> {
    unsafe {
        let fg = GetForegroundWindow();
        if is_cabinet_window(fg) && IsWindowVisible(fg).as_bool() {
            return Some(fg);
        }
        let mut curr = fg;
        for _ in 0..40 {
            if let Ok(next) = GetWindow(curr, GW_HWNDNEXT) {
                if next.0.is_null() || next == curr {
                    break;
                }
                if is_cabinet_window(next) && IsWindowVisible(next).as_bool() {
                    return Some(next);
                }
                curr = next;
            } else {
                break;
            }
        }
        None
    }
}

/// 极速定向提取当前最顶层可见资源管理器窗口的当前活动路径（短路加速，纳秒/微秒级）
pub fn get_active_explorer_folder_fast() -> Option<String> {
    unsafe {
        let top_explorer = get_top_visible_explorer_hwnd()?;
        let top_active_tab = get_active_tab_hwnd(top_explorer);

        let _ = CoInitializeEx(
            None,
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        );

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
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser
                                .HWND()
                                .map(|h| HWND(h.0 as *mut _))
                                .unwrap_or_default();

                            if hwnd == top_explorer {
                                let is_match = if let (Some(active_tab), Some(my_tab)) = (top_active_tab, get_browser_tab_hwnd(&browser)) {
                                    my_tab == active_tab
                                } else {
                                    // 经典单窗口或无标签页模式，顶级窗口匹配即为目标
                                    true
                                };

                                if is_match {
                                    if let Ok(url_bstr) = browser.LocationURL() {
                                        let url = url_bstr.to_string();
                                        if let Some(path) = parse_file_url(&url) {
                                            if (path.len() >= 2 && &path[1..2] == ":") || path.starts_with("\\\\") {
                                                return Some(path);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        None
    }
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
