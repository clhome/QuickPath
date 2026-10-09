#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::GetSystemTimes;

#[derive(Debug, Clone)]
pub struct CpuMetrics {
    /// CPU 总利用率百分比 (0.0 ~ 100.0)
    pub usage_percent: f32,
    /// 格式化用于常驻显示的文本，如 "C: 19%"
    pub display: String,
}

impl Default for CpuMetrics {
    fn default() -> Self {
        Self {
            usage_percent: 0.0,
            display: "C:  0%".to_string(),
        }
    }
}

pub struct CpuCollector {
    last_idle: u64,
    last_kernel: u64,
    last_user: u64,
    has_baseline: bool,
}

impl CpuCollector {
    pub fn new() -> Self {
        let mut collector = Self {
            last_idle: 0,
            last_kernel: 0,
            last_user: 0,
            has_baseline: false,
        };
        // 初始采样一次建立基准
        let _ = collector.sample();
        collector
    }

    pub fn sample(&mut self) -> CpuMetrics {
        unsafe {
            let mut idle = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();

            if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_err() {
                return CpuMetrics::default();
            }

            let cur_idle = filetime_to_u64(&idle);
            let cur_kernel = filetime_to_u64(&kernel);
            let cur_user = filetime_to_u64(&user);

            if !self.has_baseline {
                self.last_idle = cur_idle;
                self.last_kernel = cur_kernel;
                self.last_user = cur_user;
                self.has_baseline = true;
                return CpuMetrics::default();
            }

            let delta_idle = cur_idle.saturating_sub(self.last_idle);
            let delta_kernel = cur_kernel.saturating_sub(self.last_kernel);
            let delta_user = cur_user.saturating_sub(self.last_user);

            self.last_idle = cur_idle;
            self.last_kernel = cur_kernel;
            self.last_user = cur_user;

            // 在 Windows 中，KernelTime 已包含 IdleTime，因此总时间为 delta_kernel + delta_user
            let total_system = delta_kernel.saturating_add(delta_user);
            let usage_percent = if total_system > 0 && delta_idle <= total_system {
                let idle_ratio = (delta_idle as f64) / (total_system as f64);
                let usage = (1.0 - idle_ratio) * 100.0;
                (usage as f32).clamp(0.0, 100.0)
            } else {
                0.0
            };

            CpuMetrics {
                usage_percent,
                display: format!("C: {:2.0}%", usage_percent.round()),
            }
        }
    }

    pub fn reset_baseline(&mut self) {
        self.has_baseline = false;
    }
}

fn filetime_to_u64(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}
