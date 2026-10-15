#![allow(dead_code)]

//! 显式探测的**进程级非阻塞独占 lease**（审计报告 P0-06）。
//!
//! 背景：`ClipboardTransaction` 的原子检查只覆盖「单次 Win32 写入」，**不覆盖**
//! 「快照 → 清空 → 请求管理器 → 读回 → 恢复」这一整个探测生命周期。两次并发探测会
//! 各自快照（第二次快照到的已经是第一次写下的空文本），随后双双因事件史不可证明而
//! fail-closed 放弃恢复 —— 用户原始剪贴板内容被静默丢掉。
//!
//! 因此显式探测在**开启任何剪贴板事务之前**必须先取得本 lease：
//! - **非阻塞**：拿不到立即返回「已有探测在跑」，调用方不得读取 / 清空 / 写入剪贴板；
//! - **全局唯一**：Total Commander 与 XYplorer 共用同一把 lease，交叉快速点击同样互斥；
//! - **RAII**：guard 持有到事务销毁（含恢复动作）为止，任何提前返回都会自动释放。
//!
//! 注意：`ipc::try_enter()` 的失败冷却表**不是**互斥 —— 它只记录「上次失败之后多久
//! 才可以重试」，成功请求不进入冷却，因此同一管理器的双击可以同时通过。

use std::sync::atomic::{AtomicBool, Ordering};

/// 进程级「正在显式探测」标志。`false → true` 的 CAS 即 lease 获取。
static PROBE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// 独占 lease。持有期间任何新的显式探测都必须被拒绝。
#[derive(Debug)]
pub struct ProbeLease {
    /// 私有字段：禁止构造，也禁止 `Copy`（复制会让两个持有者各自释放）
    _private: (),
}

impl ProbeLease {
    /// 非阻塞获取：已被占用时返回 `None`，**不做任何等待**，也不触碰剪贴板。
    pub fn try_acquire() -> Option<Self> {
        PROBE_IN_FLIGHT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self { _private: () })
    }
}

impl Drop for ProbeLease {
    fn drop(&mut self) {
        PROBE_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// 是否已有一次显式探测正在进行（供 UI 置灰两个入口 / 显示忙碌态）。
pub fn is_in_flight() -> bool {
    PROBE_IN_FLIGHT.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lease_is_exclusive_and_non_blocking() {
        let first = ProbeLease::try_acquire().expect("空闲时必须能拿到 lease");
        assert!(is_in_flight());
        assert!(
            ProbeLease::try_acquire().is_none(),
            "已有探测在跑时第二次获取必须立即失败（非阻塞，不得等待）"
        );
        drop(first);
        assert!(!is_in_flight());
        let second = ProbeLease::try_acquire().expect("释放后必须能再次获取");
        drop(second);
        assert!(!is_in_flight());
    }

    #[test]
    fn test_lease_releases_on_panic_unwind() {
        let caught = std::panic::catch_unwind(|| {
            let _lease = ProbeLease::try_acquire().expect("空闲时必须能拿到 lease");
            panic!("模拟探测线程 panic");
        });
        assert!(caught.is_err());
        assert!(
            !is_in_flight(),
            "探测线程 panic 展开时 RAII guard 必须释放 lease，否则后续探测被永久锁死"
        );
    }

    /// 架构守卫：两个管理器的探测入口都必须经过「先 lease、后事务」。
    #[test]
    fn test_lease_is_acquired_before_any_clipboard_transaction() {
        const SOURCE: &str = include_str!("explicit_probe.rs");
        let production = &SOURCE[..SOURCE.find("\nmod tests {").expect("必须存在测试模块")];
        let lease_at = production
            .find("ProbeLease::try_acquire()")
            .expect("显式探测入口必须先获取独占 lease");
        let begin_at = production
            .find("probe_explicit_target")
            .expect("显式探测入口必须调用管理器事务");
        assert!(
            lease_at < begin_at,
            "lease 获取必须早于任何管理器事务（否则并发探测仍可同时触碰剪贴板）"
        );
        assert!(
            production.contains("AlreadyRunning"),
            "拿不到 lease 时必须返回 AlreadyRunning，而不是继续探测"
        );
    }
}
