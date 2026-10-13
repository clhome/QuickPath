#![allow(dead_code)]

use std::thread;
use std::time::Duration;
use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationSelectionItemPattern,
    TreeScope_Children, TreeScope_Descendants, UIA_ControlTypePropertyId, UIA_ListItemControlTypeId,
    UIA_NamePropertyId, UIA_SelectionItemPatternId,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

/// 在指定的 Explorer 宿主窗口内部，通过 UI Automation 尝试高亮选中指定文件名称的列表项
pub fn try_select_file_item(host_hwnd: HWND, file_name: &str) -> bool {
    if host_hwnd.0.is_null() || file_name.is_empty() {
        return false;
    }

    unsafe {
        if !IsWindow(Some(host_hwnd)).as_bool() {
            return false;
        }

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let uia: Result<IUIAutomation, _> = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        let uia = match uia {
            Ok(u) => u,
            Err(_) => return false,
        };

        // 优先在当前活跃的文件列表子视图窗口中搜索，彻底隔绝侧边栏导航树、Toolbar 和状态栏深搜卡顿
        let view_hwnd = {
            use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;
            let h_tab_view = FindWindowExW(
                Some(host_hwnd),
                None,
                windows::core::w!("ShellTabWindowClass"),
                None,
            );
            if let Ok(h) = h_tab_view {
                if !h.0.is_null() {
                    h
                } else {
                    host_hwnd
                }
            } else {
                host_hwnd
            }
        };

        let root: IUIAutomationElement = match uia.ElementFromHandle(view_hwnd) {
            Ok(r) => r,
            Err(_) => match uia.ElementFromHandle(host_hwnd) {
                Ok(r) => r,
                Err(_) => return false,
            },
        };

        // 构建匹配条件：ControlType == ListItem 且 Name == file_name
        let var_type = VARIANT::from(UIA_ListItemControlTypeId.0 as i32);
        let cond_type = match uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_type) {
            Ok(c) => c,
            Err(_) => return false,
        };

        let var_name = VARIANT::from(BSTR::from(file_name));
        let cond_name = match uia.CreatePropertyCondition(UIA_NamePropertyId, &var_name) {
            Ok(c) => c,
            Err(_) => return false,
        };

        let cond_and = match uia.CreateAndCondition(&cond_type, &cond_name) {
            Ok(c) => c,
            Err(_) => return false,
        };

        // 尝试查找匹配的文件项
        if let Ok(item) = root.FindFirst(TreeScope_Descendants, &cond_and) {
            if let Ok(pattern) = item.GetCurrentPattern(UIA_SelectionItemPatternId) {
                if let Ok(sel) = pattern.cast::<IUIAutomationSelectionItemPattern>() {
                    let _ = sel.Select();
                    return true;
                }
            }
        }
    }

    false
}

/// 异步延时重试选中（等待列表视图加载就绪）
pub fn async_select_file_item(host_hwnd: HWND, file_name: String) {
    let raw = host_hwnd.0 as usize;
    thread::spawn(move || {
        let hwnd = HWND(raw as *mut _);
        // 延时等待 Explorer 视图渲染文件列表（最多探测 8 次，间隔 60ms）
        for _ in 0..8 {
            thread::sleep(Duration::from_millis(60));
            if try_select_file_item(hwnd, &file_name) {
                break;
            }
        }
    });
}

/// 在 Explorer 宿主窗口内，通过 UI Automation 尝试激活指定标题/文件夹名称的 TabItem
pub fn try_select_tab_item_by_title(host_hwnd: HWND, folder_name: &str) -> bool {
    if host_hwnd.0.is_null() || folder_name.is_empty() {
        return false;
    }

    unsafe {
        if !IsWindow(Some(host_hwnd)).as_bool() {
            return false;
        }

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let uia: Result<IUIAutomation, _> = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        let uia = match uia {
            Ok(u) => u,
            Err(_) => return false,
        };

        let root: IUIAutomationElement = match uia.ElementFromHandle(host_hwnd) {
            Ok(r) => r,
            Err(_) => return false,
        };

        // 核心性能与可靠性加固：优先定位 TabControl 标签栏容器，避免在全窗口万级节点深度扫描
        let var_tab_ctrl = VARIANT::from(windows::Win32::UI::Accessibility::UIA_TabControlTypeId.0 as i32);
        let cond_tab_ctrl = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_tab_ctrl).ok();

        let search_root = if let Some(ref cond) = cond_tab_ctrl {
            root.FindFirst(TreeScope_Descendants, cond).unwrap_or_else(|_| root.clone())
        } else {
            root.clone()
        };

        let var_type = VARIANT::from(windows::Win32::UI::Accessibility::UIA_TabItemControlTypeId.0 as i32);
        let cond_tab = match uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_type) {
            Ok(c) => c,
            Err(_) => return false,
        };

        let scope = if search_root == root {
            TreeScope_Descendants
        } else {
            TreeScope_Children
        };

        if let Ok(tabs) = search_root.FindAll(scope, &cond_tab) {
            if let Ok(count) = tabs.Length() {
                let target_lower = folder_name.to_lowercase();
                for i in 0..count {
                    if let Ok(tab) = tabs.GetElement(i) {
                        if let Ok(name_bstr) = tab.CurrentName() {
                            let name_str = name_bstr.to_string().to_lowercase();
                            if name_str.contains(&target_lower) || target_lower.contains(&name_str) {
                                if let Ok(pattern) = tab.GetCurrentPattern(UIA_SelectionItemPatternId) {
                                    if let Ok(sel) = pattern.cast::<IUIAutomationSelectionItemPattern>() {
                                        let _ = sel.Select();
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    false
}
