use std::sync::atomic::{AtomicPtr, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, PostMessageW, EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

pub const WM_QUICKPATH_FOREGROUND: u32 = 0x0400 + 101;

static MAIN_THREAD_WINDOW: AtomicPtr<core::ffi::c_void> =
    AtomicPtr::new(std::ptr::null_mut());

pub struct WinEventHookGuard {
    hook: HWINEVENTHOOK,
}

impl WinEventHookGuard {
    pub fn new(notify_window: HWND) -> Result<Self, String> {
        MAIN_THREAD_WINDOW.store(notify_window.0 as *mut _, Ordering::SeqCst);

        unsafe {
            let hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );

            if hook.is_invalid() {
                return Err("注册系统级 WinEventHook 失败".to_string());
            }

            Ok(Self { hook })
        }
    }
}

impl Drop for WinEventHookGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWinEvent(self.hook);
            }
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    unsafe {
        if event == EVENT_SYSTEM_FOREGROUND && !hwnd.0.is_null() {
            let target = MAIN_THREAD_WINDOW.load(Ordering::SeqCst);
            if !target.is_null() {
                let target_hwnd = HWND(target as *mut _);
                let _ = PostMessageW(
                    Some(target_hwnd),
                    WM_QUICKPATH_FOREGROUND,
                    WPARAM(hwnd.0 as usize),
                    LPARAM(0),
                );
            }
        }
    }
}

pub fn get_active_foreground_window() -> HWND {
    unsafe { GetForegroundWindow() }
}
