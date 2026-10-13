#![allow(dead_code)]

use std::sync::atomic::{AtomicPtr, Ordering};
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
    CUIAutomation, IUIAutomation, IUIAutomationSelectionItemPattern, TreeScope_Descendants,
    UIA_ControlTypePropertyId, UIA_SelectionItemPatternId, UIA_TabItemControlTypeId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetAncestor, GetSystemMetrics, SetWindowsHookExW,
    UnhookWindowsHookEx, WindowFromPoint, GA_ROOT, HHOOK, MSLLHOOKSTRUCT, SM_CXDOUBLECLK,
    SM_CYDOUBLECLK, WH_MOUSE_LL, WM_LBUTTONDOWN,
};

use super::coordinator::TabCoordinator;

static COORDINATOR_MOUSE_PTR: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static LAST_LBUTTON_CLICK: Mutex<Option<(Instant, POINT)>> = Mutex::new(None);

pub struct TabMouseGuard {
    hook: HHOOK,
    _coordinator: Arc<TabCoordinator>,
}

impl TabMouseGuard {
    pub fn new(coordinator: Arc<TabCoordinator>) -> Result<Self, String> {
        let raw_ptr = Arc::into_raw(coordinator.clone()) as *mut core::ffi::c_void;
        COORDINATOR_MOUSE_PTR.store(raw_ptr, Ordering::SeqCst);

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
        let ptr = COORDINATOR_MOUSE_PTR.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let _ = Arc::from_raw(ptr as *const TabCoordinator);
            }
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
                let raw_ptr = COORDINATOR_MOUSE_PTR.load(Ordering::SeqCst);
                if !raw_ptr.is_null() {
                    let coordinator = unsafe { Arc::from_raw(raw_ptr as *const TabCoordinator) };
                    let coordinator_clone = coordinator.clone();
                    let _ = Arc::into_raw(coordinator); // 保持指针不被 drop

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

        // 2. 兜底路径：浅层/全树匹配
        let root_elem = match uia.ElementFromHandle(root_hwnd) {
            Ok(e) => e,
            Err(_) => return None,
        };

        let var_type = VARIANT::from(UIA_TabItemControlTypeId.0 as i32);
        let cond_tab = match uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &var_type) {
            Ok(c) => c,
            Err(_) => return None,
        };

        if let Ok(tabs) = root_elem.FindAll(TreeScope_Descendants, &cond_tab) {
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
