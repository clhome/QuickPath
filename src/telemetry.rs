#![allow(dead_code)]

//! 共享遥测工具：有界 u64 微秒样本环 + 分位数计算。
//!
//! 审计报告 P-04 要求后台扫描遥测复用 `tab::com` 的同一套分位思路，因此把原先
//! 内联在 `tab::com` 的 `percentile_ms` 与耗时样本环抽到这里，供 COM STA 埋点
//! 与后台扫描埋点共用，避免复制算法后两处各自漂移。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// 样本环默认容量（沿用 `tab::com` 原 `LATENCY_SAMPLES = 256`）。
pub const DEFAULT_SAMPLE_CAPACITY: usize = 256;

/// 有界耗时样本环（u64 微秒）：满环后丢弃最旧样本，保证内存与分位计算开销有界。
pub struct SampleRing {
    capacity: usize,
    samples_us: Mutex<VecDeque<u64>>,
}

impl Default for SampleRing {
    fn default() -> Self {
        Self::new(DEFAULT_SAMPLE_CAPACITY)
    }
}

impl SampleRing {
    pub const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            samples_us: Mutex::new(VecDeque::new()),
        }
    }

    /// 记录一个耗时样本；环满时先丢弃最旧样本，保证样本数恒 `<= capacity`。
    pub fn record(&self, elapsed: Duration) {
        let mut samples = self
            .samples_us
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if samples.len() >= self.capacity {
            samples.pop_front();
        }
        samples.push_back(elapsed.as_micros() as u64);
    }

    /// 当前样本数（恒有界）。
    pub fn len(&self) -> usize {
        self.samples_us
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 返回升序排列的样本副本，供分位计算使用。
    pub fn sorted_samples(&self) -> Vec<u64> {
        let mut samples: Vec<u64> = self
            .samples_us
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .copied()
            .collect();
        samples.sort_unstable();
        samples
    }
}

/// 最近秩（nearest-rank）分位数：输入需已升序，单位微秒，返回毫秒。
///
/// 空样本返回 `0.0`（绝不返回 NaN）。与 `tab::com` 原实现逐字一致。
pub fn percentile_ms(sorted_us: &[u64], percentile: f64) -> f64 {
    if sorted_us.is_empty() {
        return 0.0;
    }
    let rank = ((percentile / 100.0) * (sorted_us.len() as f64 - 1.0)).round() as usize;
    sorted_us[rank.min(sorted_us.len() - 1)] as f64 / 1000.0
}
