#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use super::coordinator::TabCoordinator;
use super::owner::CoordinatorSlot;

static HOOK_SLOT: CoordinatorSlot = CoordinatorSlot::new();

/// 卸载时等待在途回调退出的上限（超时则宁可泄漏也不 UAF）
const UNLOAD_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

pub struct TabHookGuard {
    hook: HWINEVENTHOOK,
    _coordinator: Arc<TabCoordinator>,
}

impl TabHookGuard {
    pub fn new(coordinator: Arc<TabCoordinator>) -> Result<Self, String> {
        HOOK_SLOT.install(&coordinator);

        unsafe {
            // 同时监听 EVENT_OBJECT_CREATE 至 EVENT_OBJECT_SHOW，确保外部新窗口 100% 捕获
            let hook = SetWinEventHook(
                EVENT_OBJECT_CREATE,
                EVENT_OBJECT_SHOW,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );

            if hook.is_invalid() {
                return Err("注册资源管理器多标签页 WinEventHook 失败".to_string());
            }

            Ok(Self {
                hook,
                _coordinator: coordinator,
            })
        }
    }
}

impl Drop for TabHookGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWinEvent(self.hook);
            }
        }
        // 先摘指针再等在途回调归零，最后才释放唯一那份所有权（报告 R-04）
        if !HOOK_SLOT.release(UNLOAD_DRAIN_TIMEOUT) {
            crate::dialog::detector::log_debug(
                "TAB_HOOK: 卸载时有回调仍在执行，已按安全优先故意保留引用",
            );
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if hwnd.0.is_null() || id_object != 0 {
        return;
    }

    // 受控借用（报告 R-04）：槽位已卸载则直接返回，绝不触碰已释放的指针
    let Some(coordinator) = HOOK_SLOT.borrow() else {
        return;
    };

    match event {
        EVENT_OBJECT_CREATE | EVENT_OBJECT_SHOW => {
            if TabCoordinator::is_explorer_window(hwnd) {
                coordinator.handle_new_explorer_window(hwnd);
            }
        }
        _ => {}
    }
}
