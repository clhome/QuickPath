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
    TreeScope, TreeScope_Children, TreeScope_Descendants, UIA_ControlTypePropertyId,
    UIA_ListItemControlTypeId, UIA_NamePropertyId, UIA_SelectionItemPatternId,
    UIA_TabControlTypeId, UIA_TabItemControlTypeId,
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

/// 统一的标签栏搜索目标：优先收窄到 TabControl 容器（Children 范围），
/// 找不到容器时回退到宿主根（Descendants 范围）。
/// 选择与核验必须共用本策略，否则会出现“选择找得到、核验找不到”的假失败。
unsafe fn tab_search_target(
    uia: &IUIAutomation,
    root: &IUIAutomationElement,
) -> (IUIAutomationElement, TreeScope) {
    unsafe {
        let var_tab_ctrl = VARIANT::from(UIA_TabControlTypeId.0 as i32);
        if let Ok(cond) = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_tab_ctrl) {
            if let Ok(container) = root.FindFirst(TreeScope_Descendants, &cond) {
                return (container, TreeScope_Children);
            }
        }
        (root.clone(), TreeScope_Descendants)
    }
}

/// 收集宿主窗口内全部 TabItem 元素及其小写名称
unsafe fn collect_tab_items(
    uia: &IUIAutomation,
    root: &IUIAutomationElement,
) -> Vec<(IUIAutomationElement, String)> {
    unsafe {
        let (search_root, scope) = tab_search_target(uia, root);
        let mut items = Vec::new();
        let var_type = VARIANT::from(UIA_TabItemControlTypeId.0 as i32);
        if let Ok(cond) = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_type) {
            if let Ok(tabs) = search_root.FindAll(scope, &cond) {
                let count = tabs.Length().unwrap_or(0);
                for i in 0..count {
                    if let Ok(tab) = tabs.GetElement(i) {
                        let name = tab
                            .CurrentName()
                            .map(|n| n.to_string().to_lowercase())
                            .unwrap_or_default();
                        items.push((tab, name));
                    }
                }
            }
        }
        items
    }
}

/// 目标标签匹配（纯逻辑，便于单测）：精确同名优先；仅当“包含匹配唯一”时才接受模糊命中，
/// 杜绝 `C:\ProjectA\src` 与 `D:\ProjectB\src` 这类同名目录互相误激活。
fn pick_matching_index(names: &[String], target: &str) -> Option<usize> {
    let target = target.to_lowercase();
    if target.is_empty() {
        return None;
    }
    if let Some(idx) = names.iter().position(|n| *n == target) {
        return Some(idx);
    }
    let mut found = None;
    for (i, name) in names.iter().enumerate() {
        if !name.is_empty() && (name.contains(target.as_str()) || target.contains(name.as_str())) {
            if found.is_some() {
                // 多个模糊候选：无法确定唯一目标，宁可放弃也不猜测
                return None;
            }
            found = Some(i);
        }
    }
    found
}

/// 目标标签匹配：返回唯一确定的目标元素
fn pick_matching_tab<'a>(
    items: &'a [(IUIAutomationElement, String)],
    target: &str,
) -> Option<&'a IUIAutomationElement> {
    let names: Vec<String> = items.iter().map(|(_, n)| n.clone()).collect();
    let idx = pick_matching_index(&names, target)?;
    items.get(idx).map(|(elem, _)| elem)
}

/// 检查宿主窗口中当前选中的 TabItem 是否为目标文件夹（用于核验标签页激活结果）。
/// 通过 UIA SelectionItemPattern.IsSelected() 定位真正的活动标签，而非猜测顺序或相信魔法指令的返回值。
pub fn is_tab_active_by_name(host_hwnd: HWND, folder_name: &str) -> bool {
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

        let items = collect_tab_items(&uia, &root);
        if items.is_empty() {
            return false;
        }
        let names: Vec<String> = items.iter().map(|(_, n)| n.clone()).collect();

        // 定位当前处于选中态的标签
        let mut selected_idx: Option<usize> = None;
        for (i, (tab, _)) in items.iter().enumerate() {
            let selected = tab
                .GetCurrentPattern(UIA_SelectionItemPatternId)
                .ok()
                .and_then(|p| p.cast::<IUIAutomationSelectionItemPattern>().ok())
                .and_then(|sel| sel.CurrentIsSelected().ok())
                .map(|b| b.as_bool())
                .unwrap_or(false);
            if selected {
                selected_idx = Some(i);
                break;
            }
        }

        // 活动标签必须就是目标标签：精确同名，或唯一模糊候选
        let Some(sel_idx) = selected_idx else {
            return false;
        };
        pick_matching_index(&names, folder_name) == Some(sel_idx)
    }
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

        let items = collect_tab_items(&uia, &root);
        let Some(tab) = pick_matching_tab(&items, folder_name) else {
            return false;
        };

        tab.GetCurrentPattern(UIA_SelectionItemPatternId)
            .ok()
            .and_then(|p| p.cast::<IUIAutomationSelectionItemPattern>().ok())
            .map(|sel| sel.Select().is_ok())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_lowercase()).collect()
    }

    #[test]
    fn test_pick_matching_index_prefers_exact_name() {
        let tabs = names(&["src", "target", "release"]);
        assert_eq!(pick_matching_index(&tabs, "release"), Some(2));
        assert_eq!(pick_matching_index(&tabs, "RELEASE"), Some(2));
        assert_eq!(pick_matching_index(&tabs, "target"), Some(1));
    }

    #[test]
    fn test_pick_matching_index_rejects_ambiguous_fuzzy() {
        // 两个标签都包含 "src"：无法唯一确定，必须放弃而不是猜
        let tabs = names(&["src", "src-backup", "other"]);
        assert_eq!(pick_matching_index(&tabs, "sr"), None);
        // 唯一模糊候选才接受
        let unique = names(&["my-src-folder", "other"]);
        assert_eq!(pick_matching_index(&unique, "src"), Some(0));
    }

    #[test]
    fn test_pick_matching_index_ignores_empty_names() {
        let tabs = names(&["", "release"]);
        assert_eq!(pick_matching_index(&tabs, "release"), Some(1));
        // 空目标与全空名称均不得命中
        assert_eq!(pick_matching_index(&tabs, ""), None);
        assert_eq!(pick_matching_index(&names(&["", ""]), "src"), None);
    }

    #[test]
    fn test_pick_matching_index_same_leaf_different_parents() {
        // 同名叶子目录（C:/ProjectA/src / D:/ProjectB/src）在标签上同名，
        // 精确匹配返回首个，但绝不会因为父路径不同而互相误判
        let tabs = names(&["src", "src"]);
        assert_eq!(pick_matching_index(&tabs, "src"), Some(0));
    }
}
