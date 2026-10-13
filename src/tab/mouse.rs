#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::core::Interface;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationSelectionItemPattern, TreeScope_Children,
    TreeScope_Descendants, UIA_ControlTypePropertyId, UIA_SelectionItemPatternId,
    UIA_TabControlTypeId, UIA_TabItemControlTypeId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetAncestor, GetSystemMetrics, SetWindowsHookExW,
    UnhookWindowsHookEx, WindowFromPoint, GA_ROOT, HHOOK, MSLLHOOKSTRUCT, SM_CXDOUBLECLK,
    SM_CYDOUBLECLK, WH_MOUSE_LL, WM_LBUTTONDOWN,
};

use super::coordinator::TabCoordinator;
use super::owner::CoordinatorSlot;

static MOUSE_SLOT: CoordinatorSlot = CoordinatorSlot::new();
static LAST_LBUTTON_CLICK: Mutex<Option<(Instant, POINT)>> = Mutex::new(None);

/// 卸载时等待在途回调退出的上限（超时则宁可泄漏也不 UAF）
const UNLOAD_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

pub struct TabMouseGuard {
    hook: HHOOK,
    _coordinator: Arc<TabCoordinator>,
}

impl TabMouseGuard {
    pub fn new(coordinator: Arc<TabCoordinator>) -> Result<Self, String> {
        MOUSE_SLOT.install(&coordinator);

        unsafe {
            let hook = SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(mouse_ll_proc),
                None,
                0,
            ).map_err(|e| format!("注册多标签页鼠标双击 LowLevel 钩子失败: {}", e))?;

            if hook.is_invalid() {
                return Err("注册多标签页鼠标双击 LowLevel 钩子失败: 句柄无效".to_string());
            }

            Ok(Self {
                hook,
                _coordinator: coordinator,
            })
        }
    }
}

impl Drop for TabMouseGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWindowsHookEx(self.hook);
            }
        }
        // 先摘指针再等在途回调归零，最后才释放唯一那份所有权（报告 R-04）
        if !MOUSE_SLOT.release(UNLOAD_DRAIN_TIMEOUT) {
            crate::dialog::detector::log_debug(
                "TAB_MOUSE: 卸载时有回调仍在执行，已按安全优先故意保留引用",
            );
        }
    }
}

unsafe extern "system" fn mouse_ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 == WM_LBUTTONDOWN as usize {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let pt = info.pt;

        let now = Instant::now();
        let mut is_double_click = false;

        if let Ok(mut last) = LAST_LBUTTON_CLICK.lock() {
            if let Some((last_time, last_pt)) = *last {
                let elapsed = now.duration_since(last_time);
                let double_click_time = unsafe { GetDoubleClickTime() };
                let max_dx = unsafe { GetSystemMetrics(SM_CXDOUBLECLK) };
                let max_dy = unsafe { GetSystemMetrics(SM_CYDOUBLECLK) };

                let dx = (pt.x - last_pt.x).abs();
                let dy = (pt.y - last_pt.y).abs();

                if elapsed <= Duration::from_millis(double_click_time as u64)
                    && dx <= max_dx
                    && dy <= max_dy
                {
                    is_double_click = true;
                    *last = None; // 触发双击后复位，防止连击
                } else {
                    *last = Some((now, pt));
                }
            } else {
                *last = Some((now, pt));
            }
        }

        if is_double_click {
            let target_wnd = unsafe { WindowFromPoint(pt) };
            let root_wnd = unsafe { GetAncestor(target_wnd, GA_ROOT) };

            if TabCoordinator::is_explorer_window(root_wnd) {
                // 受控借用后提升为独立强引用：工作线程的存活不依赖静态指针
                if let Some(coordinator) = MOUSE_SLOT.borrow() {
                    let coordinator_clone = coordinator.to_owned();
                    let raw_root = root_wnd.0 as usize;
                    thread::spawn(move || {
                        let root_hwnd = HWND(raw_root as *mut _);
                        let cfg = coordinator_clone.get_config();
                        if !cfg.enabled || !cfg.double_click_to_duplicate {
                            return;
                        }

                        // 校验坐标是否落在 TabItem 主体区域，并提取目标 Tab 标题和序号
                        if let Some((tab_title, tab_index)) = find_tab_item_at_point(root_hwnd, pt) {
                            coordinator_clone.duplicate_tab(root_hwnd, Some(&tab_title), Some(tab_index));
                        }
                    });
                }
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 检查指定的屏幕坐标 (pt) 是否落在指定 Explorer 窗口内的某一个 TabItem 标签主体区域
/// 若命中，返回 (Tab 标题, Tab 索引序号) 并主动激活该 Tab
fn find_tab_item_at_point(root_hwnd: HWND, pt: POINT) -> Option<(String, usize)> {
    if root_hwnd.0.is_null() {
        return None;
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let uia: Result<IUIAutomation, _> = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        let uia = match uia {
            Ok(u) => u,
            Err(_) => return None,
        };

        // 1. 优先超速路径：通过 ElementFromPoint 纳秒级直接命中被点击的元素或其父 TabItem
        if let Ok(elem) = uia.ElementFromPoint(pt) {
            let mut curr = elem;
            for _ in 0..4 {
                if let Ok(ctrl_type) = curr.CurrentControlType() {
                    if ctrl_type == UIA_TabItemControlTypeId {
                        if let Ok(rect) = curr.CurrentBoundingRectangle() {
                            // 排除右侧关闭按钮区域（约 34px 宽度）
                            if pt.x <= (rect.right - 34) {
                                let title = curr.CurrentName().map(|n| n.to_string()).unwrap_or_default();
                                if let Ok(pattern) = curr.GetCurrentPattern(UIA_SelectionItemPatternId) {
                                    if let Ok(sel) = pattern.cast::<IUIAutomationSelectionItemPattern>() {
                                        let _ = sel.Select();
                                    }
                                }
                                return Some((title, 0));
                            }
                        }
                    }
                }
                if let Ok(walker) = uia.RawViewWalker() {
                    if let Ok(parent) = walker.GetParentElement(&curr) {
                        curr = parent;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }

        // 2. 兜底路径：优先限制在 TabControl 容器子树，避免从根窗口无节制遍历上万节点
        let root_elem = match uia.ElementFromHandle(root_hwnd) {
            Ok(e) => e,
            Err(_) => return None,
        };

        // 尝试快速定位标签栏容器控件 (TabControl)
        let var_tab_ctrl = VARIANT::from(UIA_TabControlTypeId.0 as i32);
        let cond_tab_ctrl = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_tab_ctrl).ok();

        let search_root = if let Some(ref cond) = cond_tab_ctrl {
            root_elem.FindFirst(TreeScope_Descendants, cond).unwrap_or_else(|_| root_elem.clone())
        } else {
            root_elem.clone()
        };

        let var_type = VARIANT::from(UIA_TabItemControlTypeId.0 as i32);
        let cond_tab = match uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_type) {
            Ok(c) => c,
            Err(_) => return None,
        };

        // 若成功锁定 TabControl，仅需直接枚举其子节点 (TreeScope_Children)；否则降级局部后代搜索
        let scope = if search_root == root_elem {
            TreeScope_Descendants
        } else {
            TreeScope_Children
        };

        if let Ok(tabs) = search_root.FindAll(scope, &cond_tab) {
            if let Ok(count) = tabs.Length() {
                for i in 0..count {
                    if let Ok(tab) = tabs.GetElement(i) {
                        if let Ok(rect) = tab.CurrentBoundingRectangle() {
                            if pt.x >= rect.left
                                && pt.x <= rect.right
                                && pt.y >= rect.top
                                && pt.y <= rect.bottom
                            {
                                if pt.x <= (rect.right - 34) {
                                    let title = tab.CurrentName().map(|n| n.to_string()).unwrap_or_default();
                                    if let Ok(pattern) = tab.GetCurrentPattern(UIA_SelectionItemPatternId) {
                                        if let Ok(sel) = pattern.cast::<IUIAutomationSelectionItemPattern>() {
                                            let _ = sel.Select();
                                        }
                                    }
                                    return Some((title, i as usize));
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
