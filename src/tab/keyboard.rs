#![allow(dead_code)]

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use super::coordinator::TabCoordinator;

/// 自定义消息：Win+E 已被钩子拦截，请宿主窗口消息线程接管
///（新建标签页；失败时补偿打开原生资源管理器窗口）
pub const WM_QUICKPATH_WINE_TO_TAB: u32 = 0x0400 + 104;

static COORDINATOR_KEYBOARD_PTR: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
/// 宿主导出窗口句柄：钩子只向它投递消息，自身不做任何重活
static NOTIFY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub struct TabKeyboardGuard {
    hook: HHOOK,
    _coordinator: Arc<TabCoordinator>,
}

impl TabKeyboardGuard {
    pub fn new(coordinator: Arc<TabCoordinator>, notify_hwnd: HWND) -> Result<Self, String> {
        let raw_ptr = Arc::into_raw(coordinator.clone()) as *mut core::ffi::c_void;
        COORDINATOR_KEYBOARD_PTR.store(raw_ptr, Ordering::SeqCst);
        NOTIFY_HWND.store(notify_hwnd.0 as *mut _, Ordering::SeqCst);

        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_ll_proc), None, 0) };

        match hook {
            Ok(installed) if !installed.is_invalid() => Ok(Self {
                hook: installed,
                _coordinator: coordinator,
            }),
            Ok(_) => {
                Self::release_static_refs();
                Err("注册多标签页 Win+E 键盘钩子失败: 句柄无效".to_string())
            }
            Err(e) => {
                Self::release_static_refs();
                Err(format!("注册多标签页 Win+E 键盘钩子失败: {}", e))
            }
        }
    }

    /// 安装失败时归还静态引用，避免句柄未建立却泄漏一个强引用
    fn release_static_refs() {
        let ptr = COORDINATOR_KEYBOARD_PTR.swap(std::ptr::null_mut(), Ordering::SeqCst);
        let _ = NOTIFY_HWND.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let _ = Arc::from_raw(ptr as *const TabCoordinator);
            }
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
        Self::release_static_refs();
    }
}

/// 低级键盘钩子回调。约束：必须立即返回。
///
/// 这里**只做**三件事：按键识别（Win+E，且无 Ctrl/Alt/Shift 复合修饰）、
/// 与配置/宿主窗口相关的廉价判定（纯 Win32，无 COM/UIA）、以及向宿主窗口投递自定义消息。
/// COM 枚举与 UIA 调用全部移出回调，交由宿主消息线程处理，
/// 避免慢回调造成系统级键盘卡顿，甚至被 Windows 静默摘除钩子。
unsafe extern "system" fn keyboard_ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0
        && (wparam.0 == WM_KEYDOWN as usize || wparam.0 == WM_SYSKEYDOWN as usize)
        && unsafe { keyboard_ll_should_swallow(lparam) }
    {
        let raw_ptr = COORDINATOR_KEYBOARD_PTR.load(Ordering::SeqCst);
        if !raw_ptr.is_null() {
            // 受控引用计数：只借用一份强引用（绝不接管静态指针的所有权），
            // 离开作用域即归还，避免在钩子回调里做任何所有权交接。
            unsafe { Arc::increment_strong_count(raw_ptr as *const TabCoordinator) };
            let coordinator = unsafe { Arc::from_raw(raw_ptr as *const TabCoordinator) };
            let cfg = coordinator.get_config();

            let notify_raw = NOTIFY_HWND.load(Ordering::SeqCst);
            if cfg.enabled
                && cfg.wine_to_tab
                && !notify_raw.is_null()
                && let Some(target) = TabCoordinator::quick_wine_target()
            {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(notify_raw as *mut _)),
                        WM_QUICKPATH_WINE_TO_TAB,
                        WPARAM(target.0 as usize),
                        LPARAM(0),
                    );
                }
                // 仅在“配置允许接管 + 确实存在宿主 + 消息已投递”时才吞掉系统 Win+E
                return LRESULT(1);
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 判定本次按键是否为待接管的 Win+E（纯逻辑，便于单测审视判定条件）
unsafe fn keyboard_ll_should_swallow(lparam: LPARAM) -> bool {
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if info.vkCode != 0x45 {
        // VK_E = 0x45 ('E')
        return false;
    }

    let pressed = |vk: i32| -> bool {
        (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
    };

    let win = pressed(VK_LWIN.0 as i32) || pressed(VK_RWIN.0 as i32);
    if !win {
        return false;
    }
    // 排除 Ctrl / Alt / Shift 复合修饰键干扰
    !pressed(VK_CONTROL.0 as i32) && !pressed(VK_MENU.0 as i32) && !pressed(VK_SHIFT.0 as i32)
}
