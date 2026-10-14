#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HHOOK, KBDLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use super::coordinator::TabCoordinator;
use super::owner::CoordinatorSlot;

/// 自定义消息：Win+E 已被钩子拦截，请宿主窗口消息线程接管
///（新建标签页；失败时补偿打开原生资源管理器窗口）
pub const WM_QUICKPATH_WINE_TO_TAB: u32 = 0x0400 + 104;

static COORDINATOR_SLOT: CoordinatorSlot = CoordinatorSlot::new();
/// 宿主导出窗口句柄：钩子只向它投递消息，自身不做任何重活
static NOTIFY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// 卸载时等待在途回调退出的上限（超时则宁可泄漏也不 UAF，见 `CoordinatorSlot::release`）
const UNLOAD_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

pub struct TabKeyboardGuard {
    hook: HHOOK,
    _coordinator: Arc<TabCoordinator>,
}

impl TabKeyboardGuard {
    pub fn new(coordinator: Arc<TabCoordinator>, notify_hwnd: HWND) -> Result<Self, String> {
        // 槽位被占用意味着前一个钩子尚未卸载：必须显式失败，而不是静默复用
        if !COORDINATOR_SLOT.install(&coordinator) {
            return Err("Win+E 键盘钩子槽位已被占用（前一个钩子尚未卸载）".to_string());
        }
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
        let _ = NOTIFY_HWND.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !COORDINATOR_SLOT.release(UNLOAD_DRAIN_TIMEOUT) {
            crate::dialog::detector::log_debug(
                "TAB_KEYBOARD: 卸载时有回调仍在执行，已按安全优先故意保留引用",
            );
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
        // 受控借用：只借一份强引用，绝不接管静态指针的所有权（报告 R-04）
        if let Some(coordinator) = COORDINATOR_SLOT.borrow() {
            let cfg = coordinator.get_config();

            let notify_raw = NOTIFY_HWND.load(Ordering::SeqCst);
            if cfg.enabled
                && cfg.wine_to_tab
                && !notify_raw.is_null()
                && let Some(target) = TabCoordinator::quick_wine_target()
            {
                // 只有 PostMessageW 确认投递成功才吞键；投递失败必须放行给系统原生 Win+E，
                // 否则会变成“按键被吞且没有任何结果”（报告 P1-01）
                let posted = unsafe {
                    PostMessageW(
                        Some(HWND(notify_raw as *mut _)),
                        WM_QUICKPATH_WINE_TO_TAB,
                        WPARAM(target.0 as usize),
                        LPARAM(0),
                    )
                };
                if posted.is_ok() {
                    return LRESULT(1);
                }
                crate::dialog::detector::log_debug(
                    "TAB_KEYBOARD: 宿主消息投递失败，放行系统原生 Win+E",
                );
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

    let pressed = |vk: i32| -> bool { (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0 };

    let win = pressed(VK_LWIN.0 as i32) || pressed(VK_RWIN.0 as i32);
    if !win {
        return false;
    }
    // 排除 Ctrl / Alt / Shift 复合修饰键干扰
    !pressed(VK_CONTROL.0 as i32) && !pressed(VK_MENU.0 as i32) && !pressed(VK_SHIFT.0 as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tab::ShellComManager;

    /// P2-01：键盘钩子槽位被占时构造必须失败，且不得改写 NOTIFY_HWND；
    /// 失败路径与卸载路径共用的 `release_static_refs()` 必须摘清槽位。
    /// （本文件内两个断言合并在一个用例里，因为它们共享同一份静态槽位，
    ///  拆成两个并发用例会互相扰动。）
    #[test]
    fn test_keyboard_guard_slot_ownership_and_release() {
        let coordinator = Arc::new(TabCoordinator::new(Arc::new(ShellComManager::new())));

        assert!(COORDINATOR_SLOT.install(&coordinator), "首次安装必须成功");
        let rejected = TabKeyboardGuard::new(coordinator.clone(), HWND(std::ptr::null_mut()));
        assert!(rejected.is_err(), "槽位被占用时构造必须返回 Err");
        assert!(
            NOTIFY_HWND.load(Ordering::SeqCst).is_null(),
            "被拒的构造不得改写 NOTIFY_HWND"
        );

        // 失败/卸载共用路径：必须同时摘清 NOTIFY_HWND 与槽位
        NOTIFY_HWND.store(0x1234 as *mut _, Ordering::SeqCst);
        TabKeyboardGuard::release_static_refs();
        assert!(NOTIFY_HWND.load(Ordering::SeqCst).is_null());
        assert!(COORDINATOR_SLOT.borrow().is_none(), "释放后借用必须失败");

        // 释放后重新安装/释放必须成功
        assert!(COORDINATOR_SLOT.install(&coordinator));
        assert!(COORDINATOR_SLOT.release(UNLOAD_DRAIN_TIMEOUT));
    }
}
