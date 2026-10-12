#![allow(dead_code)]

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use super::coordinator::TabCoordinator;

static COORDINATOR_PTR: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub struct TabHookGuard {
    hook: HWINEVENTHOOK,
    _coordinator: Arc<TabCoordinator>,
}

impl TabHookGuard {
    pub fn new(coordinator: Arc<TabCoordinator>) -> Result<Self, String> {
        let raw_ptr = Arc::into_raw(coordinator.clone()) as *mut core::ffi::c_void;
        COORDINATOR_PTR.store(raw_ptr, Ordering::SeqCst);

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
        let ptr = COORDINATOR_PTR.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let _ = Arc::from_raw(ptr as *const TabCoordinator);
            }
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

    let raw_ptr = COORDINATOR_PTR.load(Ordering::SeqCst);
    if raw_ptr.is_null() {
        return;
    }

    let coordinator = unsafe { &*(raw_ptr as *const TabCoordinator) };

    match event {
        EVENT_OBJECT_CREATE | EVENT_OBJECT_SHOW => {
            if TabCoordinator::is_explorer_window(hwnd) {
                coordinator.handle_new_explorer_window(hwnd);
            }
        }
        _ => {}
    }
}
