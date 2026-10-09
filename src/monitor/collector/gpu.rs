#![allow(unsafe_op_in_unsafe_fn)]

use windows::core::w;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

const PDH_MORE_DATA: u32 = 0x800007D2;

#[derive(Debug, Clone)]
pub struct GpuMetrics {
    /// GPU 综合利用率百分比 (0.0 ~ 100.0)，若设备不支持则为 None
    pub usage_percent: Option<f32>,
    /// 格式化用于常驻显示的文本，如 "G: 32%" 或 "G: --%"
    pub display: String,
}

impl Default for GpuMetrics {
    fn default() -> Self {
        Self {
            usage_percent: None,
            display: "G: --%".to_string(),
        }
    }
}

pub struct GpuCollector {
    query: Option<PDH_HQUERY>,
    counter: Option<PDH_HCOUNTER>,
    fail_count: u32,
    has_first_collect: bool,
    buffer: Vec<u8>,
}

impl GpuCollector {
    pub fn new() -> Self {
        let mut collector = Self {
            query: None,
            counter: None,
            fail_count: 0,
            has_first_collect: false,
            buffer: vec![0u8; 8192],
        };
        collector.init_pdh();
        collector
    }

    fn init_pdh(&mut self) {
        self.cleanup();
        unsafe {
            let mut q = PDH_HQUERY::default();
            if PdhOpenQueryW(None, 0, &mut q) != 0 {
                return;
            }

            let mut c = PDH_HCOUNTER::default();
            let res = PdhAddEnglishCounterW(
                q,
                w!("\\GPU Engine(*)\\Utilization Percentage"),
                0,
                &mut c,
            );

            if res == 0 {
                self.counter = Some(c);
                let _ = PdhCollectQueryData(q);
                self.query = Some(q);
                self.fail_count = 0;
                self.has_first_collect = false;
            } else {
                let _ = PdhCloseQuery(q);
            }
        }
    }

    fn cleanup(&mut self) {
        if let Some(q) = self.query.take() {
            unsafe {
                let _ = PdhCloseQuery(q);
            }
        }
        self.counter = None;
        self.has_first_collect = false;
    }

    pub fn sample(&mut self) -> GpuMetrics {
        if self.query.is_none() {
            self.init_pdh();
            if self.query.is_none() {
                return GpuMetrics::default();
            }
        }

        let q = self.query.unwrap();
        let counter = match self.counter {
            Some(c) => c,
            None => return GpuMetrics::default(),
        };

        unsafe {
            if PdhCollectQueryData(q) != 0 {
                self.fail_count += 1;
                if self.fail_count >= 2 {
                    self.init_pdh();
                }
                return GpuMetrics::default();
            }

            self.fail_count = 0;

            if !self.has_first_collect {
                self.has_first_collect = true;
                return GpuMetrics::default();
            }

            let mut buf_size: u32 = self.buffer.len() as u32;
            let mut item_count: u32 = 0;

            let res = PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &mut buf_size,
                &mut item_count,
                Some(self.buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
            );

            // 缓冲区不足时扩容重试
            if res == PDH_MORE_DATA {
                self.buffer.resize(buf_size as usize + 2048, 0);
                let mut new_size = self.buffer.len() as u32;
                let _ = PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_DOUBLE,
                    &mut new_size,
                    &mut item_count,
                    Some(self.buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
                );
            }

            if item_count == 0 {
                return GpuMetrics::default();
            }

            let items = std::slice::from_raw_parts(
                self.buffer.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W,
                item_count as usize,
            );

            let mut total_usage = 0.0f64;
            for item in items {
                let val = item.FmtValue.Anonymous.doubleValue;
                if val > 0.0 {
                    total_usage += val;
                }
            }

            let clamped = (total_usage as f32).clamp(0.0, 100.0);
            GpuMetrics {
                usage_percent: Some(clamped),
                display: format!("G: {:2.0}%", clamped.round()),
            }
        }
    }

    pub fn reset_baseline(&mut self) {
        self.has_first_collect = false;
    }
}

impl Drop for GpuCollector {
    fn drop(&mut self) {
        self.cleanup();
    }
}
