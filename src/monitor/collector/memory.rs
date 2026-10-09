#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

#[derive(Debug, Clone)]
pub struct MemoryMetrics {
    /// 物理内存利用率百分比 (0 ~ 100)
    pub usage_percent: u32,
    /// 已用物理内存 (GB)
    pub used_gb: f32,
    /// 总物理内存 (GB)
    pub total_gb: f32,
    /// 格式化用于常驻显示的文本，如 "M: 42%"
    pub display: String,
    /// 格式化用于 Tooltip 显示的明细，如 "已用 13.4 GB / 共 32.0 GB"
    pub tooltip_display: String,
}

impl Default for MemoryMetrics {
    fn default() -> Self {
        Self {
            usage_percent: 0,
            used_gb: 0.0,
            total_gb: 0.0,
            display: "M:  0%".to_string(),
            tooltip_display: "0.0 GB / 0.0 GB".to_string(),
        }
    }
}

pub struct MemoryCollector;

impl MemoryCollector {
    pub fn new() -> Self {
        Self
    }

    pub fn sample(&self) -> MemoryMetrics {
        unsafe {
            let mut mem_status = MEMORYSTATUSEX {
                dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
                ..Default::default()
            };

            if GlobalMemoryStatusEx(&mut mem_status).is_err() {
                return MemoryMetrics::default();
            }

            let usage_percent = mem_status.dwMemoryLoad;
            let total_bytes = mem_status.ullTotalPhys;
            let avail_bytes = mem_status.ullAvailPhys;
            let used_bytes = total_bytes.saturating_sub(avail_bytes);

            let bytes_to_gb = 1024.0 * 1024.0 * 1024.0;
            let total_gb = (total_bytes as f64 / bytes_to_gb) as f32;
            let used_gb = (used_bytes as f64 / bytes_to_gb) as f32;

            MemoryMetrics {
                usage_percent,
                used_gb,
                total_gb,
                display: format!("M: {:2}%", usage_percent),
                tooltip_display: format!("{:.1} GB / {:.1} GB", used_gb, total_gb),
            }
        }
    }
}
