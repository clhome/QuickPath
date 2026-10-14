#![allow(dead_code)]

//! 钩子回调与 `TabCoordinator` 之间的受控共享槽（审计报告 R-04）。
//!
//! 背景：原实现把 `Arc` 拆成裸指针存进静态变量，回调里用 `Arc::from_raw()` **临时接管**
//! 该指针的所有权，再 `clone` + `into_raw` 还回去。问题在于：卸载时先 `UnhookWindowsHookEx`
//! 再 `Arc::from_raw()` 释放，代码无法证明“最后一个在途回调已经退出”，
//! 于是存在引用计数所有权竞态与潜在 UAF。
//!
//! 本模块用两条不变式把这件事说清楚：
//! 1. **借用不接管所有权**：回调期间只 `Arc::increment_strong_count` + `from_raw` 借一份强引用，
//!    作用域结束即归还，静态指针始终只持有一份所有权；
//! 2. **卸载先摘指针再等在途归零**：`release()` 先把指针置空（新回调立刻失败），
//!    再等待在途计数归零，最后才释放那份所有权；超时则**故意泄漏**该 `Arc`
//!   （宁可泄漏一个协调器，也绝不 UAF），并返回 false 供上层记录。

use std::ops::Deref;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::coordinator::TabCoordinator;

pub struct CoordinatorSlot {
    owner: AtomicPtr<core::ffi::c_void>,
    in_flight: AtomicUsize,
}

/// 一次受控借用：持有一份临时强引用，并通过 Drop 归还在途计数
pub struct CoordinatorBorrow<'a> {
    slot: &'a CoordinatorSlot,
    coordinator: Arc<TabCoordinator>,
}

impl Default for CoordinatorSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl CoordinatorSlot {
    pub const fn new() -> Self {
        Self {
            owner: AtomicPtr::new(std::ptr::null_mut()),
            in_flight: AtomicUsize::new(0),
        }
    }

    /// 安装所有权（同一时刻只允许一份）。返回 false 表示槽位已被占用。
    pub fn install(&self, coordinator: &Arc<TabCoordinator>) -> bool {
        let raw = Arc::into_raw(coordinator.clone()) as *mut core::ffi::c_void;
        match self
            .owner
            .compare_exchange(
                std::ptr::null_mut(),
                raw,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
            Ok(_) => true,
            Err(_) => {
                // 槽位已被占用：归还本次多余的强引用，绝不泄漏
                unsafe {
                    drop(Arc::from_raw(raw as *const TabCoordinator));
                }
                false
            }
        }
    }

    /// 回调期间借用协调器；若槽位已卸载（指针为空）则返回 None。
    ///
    /// 顺序很关键：**先登记在途计数，再读指针**。
    /// 若 `release()` 已经把指针置空，本次借用会在读到空指针后立刻退出；
    /// 若本线程在置空之前完成了登记，`release()` 一定能观察到该计数并等到它归零。
    pub fn borrow(&self) -> Option<CoordinatorBorrow<'_>> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let raw = self.owner.load(Ordering::SeqCst);
        if raw.is_null() {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return None;
        }

        // 受控引用计数：只借一份强引用，Drop 时归还（所有权仍归静态指针）
        unsafe { Arc::increment_strong_count(raw as *const TabCoordinator) };
        let coordinator = unsafe { Arc::from_raw(raw as *const TabCoordinator) };
        Some(CoordinatorBorrow {
            slot: self,
            coordinator,
        })
    }

    /// 卸载：摘指针 → 等在途归零 → 释放所有权。
    /// 返回 false 表示等待超时（此时已故意泄漏该引用以避免 UAF）。
    pub fn release(&self, timeout: Duration) -> bool {
        let raw = self.owner.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if raw.is_null() {
            return true;
        }

        let deadline = Instant::now() + timeout;
        while self.in_flight.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                // 在途回调仍可能解引用：宁可泄漏，绝不释放
                std::mem::forget(unsafe { Arc::from_raw(raw as *const TabCoordinator) });
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        unsafe {
            drop(Arc::from_raw(raw as *const TabCoordinator));
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

impl CoordinatorBorrow<'_> {
    /// 把借用提升为独立强引用：供回调派生出的工作线程持有。
    /// 这样线程的存活完全不依赖静态指针，卸载流程也不会因为“线程还在跑”而不能释放。
    pub fn to_owned(&self) -> Arc<TabCoordinator> {
        self.coordinator.clone()
    }
}

impl Deref for CoordinatorBorrow<'_> {
    type Target = TabCoordinator;

    fn deref(&self) -> &Self::Target {
        &self.coordinator
    }
}

impl Drop for CoordinatorBorrow<'_> {
    fn drop(&mut self) {
        self.slot.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_borrow_after_release_fails_and_owner_is_released_once() {
        let slot = CoordinatorSlot::new();
        assert!(slot.borrow().is_none(), "未安装时应借用失败");
        assert!(slot.release(Duration::from_millis(50)), "空槽位释放应为成功");

        let coordinator = Arc::new(TabCoordinator::new(Arc::new(
            super::super::com::ShellComManager::new(),
        )));

        assert!(slot.install(&coordinator));

        // 卸载后借用必须失败（新的钩子回调不会再解引用任何东西）
        assert!(slot.release(Duration::from_millis(50)));
        assert!(slot.borrow().is_none());
        assert_eq!(slot.in_flight(), 0);
    }

    #[test]
    fn test_promote_to_owned_outlives_release() {
        let coordinator = Arc::new(TabCoordinator::new(Arc::new(
            super::super::com::ShellComManager::new(),
        )));
        let slot = CoordinatorSlot::new();
        assert!(slot.install(&coordinator));
        assert!(slot.release(Duration::from_millis(50)));

        // 卸载后，先前提升出来的强引用仍然安全可用（这正是回调派生线程的用法）
        let borrow = CoordinatorSlot::new();
        assert!(borrow.install(&coordinator));
        let owned = borrow.borrow().expect("已安装时可借用").to_owned();
        assert!(borrow.release(Duration::from_millis(50)));
        let _cfg = owned.get_config();
        assert!(Arc::strong_count(&owned) >= 1);
    }

    #[test]
    fn test_release_waits_for_in_flight_borrow() {
        let coordinator = Arc::new(TabCoordinator::new(Arc::new(
            super::super::com::ShellComManager::new(),
        )));
        let slot = CoordinatorSlot::new();
        assert!(slot.install(&coordinator));

        let held = slot.borrow().expect("应可借用");
        assert_eq!(slot.in_flight(), 1);

        // 在途借用未归还前，release 必须超时并选择“泄漏而不释放”
        let start = Instant::now();
        assert!(!slot.release(Duration::from_millis(30)));
        assert!(start.elapsed() >= Duration::from_millis(30));
        assert!(slot.borrow().is_none(), "摘指针后不得再借用");

        drop(held);
        assert_eq!(slot.in_flight(), 0);
    }

    #[test]
    fn test_double_install_is_rejected_without_leaking() {
        let coordinator = Arc::new(TabCoordinator::new(Arc::new(
            super::super::com::ShellComManager::new(),
        )));
        let slot = CoordinatorSlot::new();
        assert!(slot.install(&coordinator));
        assert!(!slot.install(&coordinator), "重复安装必须被拒绝");
        assert!(slot.release(Duration::from_millis(50)));
    }
}
