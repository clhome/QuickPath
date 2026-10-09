use crate::monitor::collector::{
    CpuMetrics, DetailMetrics, DiskMetrics, GpuMetrics, NetworkSpeed,
};
use std::collections::VecDeque;

/// 历史波形环形缓冲区（15~20 秒采样历史）
#[derive(Debug, Clone)]
pub struct WaveformBuffer {
    capacity: usize,
    /// 存放 0.0 ~ 100.0 的百分比数值
    points: VecDeque<f32>,
}

impl WaveformBuffer {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.clamp(10, 30);
        let mut points = VecDeque::with_capacity(cap);
        for _ in 0..cap {
            points.push_back(0.0);
        }
        Self {
            capacity: cap,
            points,
        }
    }

    pub fn push(&mut self, val: f32) {
        if self.points.len() >= self.capacity {
            self.points.pop_front();
        }
        self.points.push_back(val.clamp(0.0, 100.0));
    }

    pub fn points(&self) -> &VecDeque<f32> {
        &self.points
    }
}

/// 某次采样周期生成的完整不可变硬件快照
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub network: NetworkSpeed,
    pub cpu: CpuMetrics,
    pub memory: crate::monitor::collector::memory::MemoryMetrics,
    pub gpu: GpuMetrics,
    pub disk: DiskMetrics,
    pub details: DetailMetrics,
    /// 历史波形数据点（最近 15~20 秒）
    pub cpu_waveform: WaveformBuffer,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            network: NetworkSpeed::default(),
            cpu: CpuMetrics::default(),
            memory: crate::monitor::collector::memory::MemoryMetrics::default(),
            gpu: GpuMetrics::default(),
            disk: DiskMetrics::default(),
            details: DetailMetrics::default(),
            cpu_waveform: WaveformBuffer::new(18),
        }
    }
}
