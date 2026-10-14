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
        // 槽位被占用意味着前一个钩子尚未卸载：必须显式失败，而不是静默复用同一个槽
        if !HOOK_SLOT.install(&coordinator) {
            return Err("资源管理器 WinEventHook 槽位已被占用（前一个钩子尚未卸载）".to_string());
        }

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
                // 注册失败：立即归还刚安装的静态引用，绝不泄漏（报告 P2-01）
                Self::release_slot();
                return Err("注册资源管理器多标签页 WinEventHook 失败".to_string());
            }

            Ok(Self {
                hook,
                _coordinator: coordinator,
            })
        }
    }

    /// 归还静态槽位：先摘指针再等在途回调归零（超时宁可泄漏也不 UAF）
    fn release_slot() {
        if !HOOK_SLOT.release(UNLOAD_DRAIN_TIMEOUT) {
            crate::dialog::detector::log_debug(
                "TAB_HOOK: 卸载时有回调仍在执行，已按安全优先故意保留引用",
            );
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
        Self::release_slot();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tab::ShellComManager;

    /// P2-01：槽位已被占用时必须显式失败（而不是静默复用），
    /// 且失败的构造绝不能破坏占用者，释放后仍可重新安装。
    #[test]
    fn test_hook_guard_rejects_occupied_slot_without_disturbing_owner() {
        let coordinator = Arc::new(TabCoordinator::new(Arc::new(ShellComManager::new())));

        assert!(HOOK_SLOT.install(&coordinator), "首次安装必须成功");
        let rejected = TabHookGuard::new(coordinator.clone());
        assert!(rejected.is_err(), "槽位被占用时构造必须返回 Err");
        // 占用者完好：借还计数归零，仍可正常释放
        assert_eq!(HOOK_SLOT.in_flight(), 0);
        assert!(HOOK_SLOT.release(UNLOAD_DRAIN_TIMEOUT));

        // 释放后重新安装/释放必须成功（证明上一次失败未污染槽位）
        assert!(HOOK_SLOT.install(&coordinator));
        assert!(HOOK_SLOT.release(UNLOAD_DRAIN_TIMEOUT));
    }
}
