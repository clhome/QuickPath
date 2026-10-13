#![allow(dead_code)]

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx,
    HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use super::coordinator::TabCoordinator;

static COORDINATOR_KEYBOARD_PTR: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub struct TabKeyboardGuard {
    hook: HHOOK,
    _coordinator: Arc<TabCoordinator>,
}

impl TabKeyboardGuard {
    pub fn new(coordinator: Arc<TabCoordinator>) -> Result<Self, String> {
        let raw_ptr = Arc::into_raw(coordinator.clone()) as *mut core::ffi::c_void;
        COORDINATOR_KEYBOARD_PTR.store(raw_ptr, Ordering::SeqCst);

        unsafe {
            let hook = SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_ll_proc),
                None,
                0,
            ).map_err(|e| format!("注册多标签页 Win+E 键盘钩子失败: {}", e))?;

            if hook.is_invalid() {
                return Err("注册多标签页 Win+E 键盘钩子失败: 句柄无效".to_string());
            }

            Ok(Self {
                hook,
                _coordinator: coordinator,
            })
        }
    }
}

impl Drop for TabKeyboardGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWindowsHookEx(self.hook);
            }
        }
        let ptr = COORDINATOR_KEYBOARD_PTR.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let _ = Arc::from_raw(ptr as *const TabCoordinator);
            }
        }
    }
}

unsafe extern "system" fn keyboard_ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (wparam.0 == WM_KEYDOWN as usize || wparam.0 == WM_SYSKEYDOWN as usize) {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // VK_E = 0x45 ('E')
        if info.vkCode == 0x45 {
            // 检查 Win 键是否被按下
            let lwin = (unsafe { GetAsyncKeyState(VK_LWIN.0 as i32) } as u16 & 0x8000) != 0;
            let rwin = (unsafe { GetAsyncKeyState(VK_RWIN.0 as i32) } as u16 & 0x8000) != 0;
            if lwin || rwin {
                // 排除 Ctrl / Alt / Shift 复合修饰键干扰
                let ctrl = (unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0;
                let alt = (unsafe { GetAsyncKeyState(VK_MENU.0 as i32) } as u16 & 0x8000) != 0;
                let shift = (unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000) != 0;
                if !ctrl && !alt && !shift {
                    let raw_ptr = COORDINATOR_KEYBOARD_PTR.load(Ordering::SeqCst);
                    if !raw_ptr.is_null() {
                        let coordinator = unsafe { Arc::from_raw(raw_ptr as *const TabCoordinator) };
                        let coordinator_clone = coordinator.clone();
                        let _ = Arc::into_raw(coordinator);

                        if coordinator_clone.handle_wine_hotkey() {
                            return LRESULT(1); // 成功拦截 Win+E 并转为新建标签页，阻止系统弹出新窗口
                        }
                    }
                }
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
