#![allow(dead_code)]

use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationSelectionItemPattern,
    TreeScope, TreeScope_Children, TreeScope_Descendants, UIA_ControlTypePropertyId,
    UIA_ListItemControlTypeId, UIA_NamePropertyId, UIA_SelectionItemPatternId,
    UIA_TabControlTypeId, UIA_TabItemControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{BSTR, Interface};

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
        let uia: Result<IUIAutomation, _> =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
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
                if !h.0.is_null() { h } else { host_hwnd }
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
        let var_type = VARIANT::from(UIA_ListItemControlTypeId.0);
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
        if let Ok(item) = root.FindFirst(TreeScope_Descendants, &cond_and)
            && let Ok(pattern) = item.GetCurrentPattern(UIA_SelectionItemPatternId)
            && let Ok(sel) = pattern.cast::<IUIAutomationSelectionItemPattern>()
        {
            let _ = sel.Select();
            return true;
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
        let var_tab_ctrl = VARIANT::from(UIA_TabControlTypeId.0);
        if let Ok(cond) = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_tab_ctrl)
            && let Ok(container) = root.FindFirst(TreeScope_Descendants, &cond)
        {
            return (container, TreeScope_Children);
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
        let var_type = VARIANT::from(UIA_TabItemControlTypeId.0);
        if let Ok(cond) = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_type)
            && let Ok(tabs) = search_root.FindAll(scope, &cond)
        {
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
        items
    }
}

/// 选择定位策略（纯逻辑，便于单测）。
///
/// 标签标题（末级目录名）不是身份：同一窗口中可以有多个 `src`。因此：
/// 1. 存在唯一精确同名项 → 选它（最常见、最准）；
/// 2. 否则（同名多个 / 无精确匹配）→ 回退到 Shell COM 枚举得到的索引；
/// 3. 目标为空或回退索引越界时返回 None（宁可放弃，也不猜）。
///
/// 真正的身份确认不在这里，而由调用方在选中后回查 Shell COM 的**完整路径**。
fn pick_tab_index_for_selection(names: &[String], target: &str, com_index: usize) -> Option<usize> {
    let lower = target.to_lowercase();
    if lower.is_empty() {
        return None;
    }

    let exact: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.is_empty() && **n == lower)
        .map(|(i, _)| i)
        .collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }

    // 回退到 COM 索引：仅接受名称非空的项（空名多为尚未加载完成的退化标签，选中无意义）
    if com_index < names.len() && !names[com_index].is_empty() {
        return Some(com_index);
    }
    None
}

/// 在 Explorer 宿主窗口中选择目标标签。
///
/// 本函数**只负责选择动作**，不做任何身份声称：
/// 选择结果必须由调用方回查 Shell COM 的完整路径后才能确认。
pub fn try_select_tab_item(host_hwnd: HWND, com_index: usize, leaf_name: &str) -> bool {
    if host_hwnd.0.is_null() {
        return false;
    }

    unsafe {
        if !IsWindow(Some(host_hwnd)).as_bool() {
            return false;
        }

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let uia: Result<IUIAutomation, _> =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        let Ok(uia) = uia else {
            return false;
        };

        let root: IUIAutomationElement = match uia.ElementFromHandle(host_hwnd) {
            Ok(r) => r,
            Err(_) => return false,
        };

        let items = collect_tab_items(&uia, &root);
        let names: Vec<String> = items.iter().map(|(_, n)| n.clone()).collect();
        let Some(idx) = pick_tab_index_for_selection(&names, leaf_name, com_index) else {
            return false;
        };
        let Some((tab, _)) = items.get(idx) else {
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
    fn test_pick_tab_index_prefers_unique_exact_name() {
        let tabs = names(&["src", "target", "release"]);
        assert_eq!(pick_tab_index_for_selection(&tabs, "release", 0), Some(2));
        assert_eq!(pick_tab_index_for_selection(&tabs, "RELEASE", 0), Some(2));
        assert_eq!(pick_tab_index_for_selection(&tabs, "target", 2), Some(1));
    }

    #[test]
    fn test_pick_tab_index_same_leaf_uses_com_index_and_needs_path_proof() {
        // 同名叶子目录（C:/ProjectA/src 与 D:/ProjectB/src）在标签标题上无法区分：
        // 必须回退到 COM 枚举索引定位，再由调用方回查完整路径核验身份。
        let tabs = names(&["src", "src"]);
        assert_eq!(pick_tab_index_for_selection(&tabs, "src", 1), Some(1));
        assert_eq!(pick_tab_index_for_selection(&tabs, "src", 0), Some(0));
        // 索引越界且无法唯一定位时宁可放弃，绝不猜测
        assert_eq!(pick_tab_index_for_selection(&tabs, "src", 9), None);

        // 唯一精确同名项优先于 COM 索引（避免索引与视觉顺序不一致时选错）
        let unique = names(&["build", "src"]);
        assert_eq!(pick_tab_index_for_selection(&unique, "src", 0), Some(1));

        // 无精确同名（标题带后缀等）时同样交由 COM 索引定位
        let fuzzy = names(&["src-backup", "other"]);
        assert_eq!(pick_tab_index_for_selection(&fuzzy, "src", 1), Some(1));
        assert_eq!(pick_tab_index_for_selection(&fuzzy, "src", 2), None);
    }

    #[test]
    fn test_pick_tab_index_ignores_empty_names_and_targets() {
        let tabs = names(&["", "release"]);
        assert_eq!(pick_tab_index_for_selection(&tabs, "release", 0), Some(1));
        // 空目标与全空名称均不得命中
        assert_eq!(pick_tab_index_for_selection(&tabs, "", 0), None);
        assert_eq!(
            pick_tab_index_for_selection(&names(&["", ""]), "src", 0),
            None
        );
    }

    /// P-02 架构不变式守卫：STA 线程（`tab/com.rs`）与 STA 链路（`managers/explorer.rs`）
    /// 上**不得**出现 UIA 调用 —— UIA 一旦挂起会永久堵死整个 Shell COM 队列。
    /// UIA 只能出现在各自的专用线程里（本文件 / `tab/uia.rs` / `win32/uia.rs`）。
    #[test]
    fn test_sta_worker_source_never_calls_uia() {
        let sources = [
            ("tab/com.rs", include_str!("com.rs")),
            (
                "managers/explorer.rs",
                include_str!("../managers/explorer.rs"),
            ),
        ];

        for (name, source) in sources {
            // 先剔除注释行，避免架构说明文字本身触发误报
            let code = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");

            for forbidden in ["IUIAutomation", "CUIAutomation", "Accessibility", "UIA_"] {
                assert!(
                    !code.contains(forbidden),
                    "{} 属于 STA 链路，禁止使用 UIA（命中 `{}`）",
                    name,
                    forbidden
                );
            }
        }
    }
}
