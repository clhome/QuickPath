#![allow(unsafe_op_in_unsafe_fn)]

use windows::core::w;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

#[derive(Debug, Clone)]
pub struct DiskMetrics {
    /// 物理磁盘活动时间百分比 (0.0 ~ 100.0)
    pub activity_percent: f32,
    /// 实时读取带宽 (MB/s)
    pub read_mb_s: f64,
    /// 实时写入带宽 (MB/s)
    pub write_mb_s: f64,
    /// 格式化用于常驻显示的文本，如 "D:  3%"
    pub display: String,
    /// 格式化用于 Tooltip 显示的明细，如 "读取: 2.1 MB/s, 写入: 10.2 MB/s"
    pub tooltip_display: String,
}

impl Default for DiskMetrics {
    fn default() -> Self {
        Self {
            activity_percent: 0.0,
            read_mb_s: 0.0,
            write_mb_s: 0.0,
            display: "D:  0%".to_string(),
            tooltip_display: "读取: 0.0 MB/s, 写入: 0.0 MB/s".to_string(),
        }
    }
}

pub struct DiskCollector {
    query: Option<PDH_HQUERY>,
    counter_time: Option<PDH_HCOUNTER>,
    counter_read: Option<PDH_HCOUNTER>,
    counter_write: Option<PDH_HCOUNTER>,
    fail_count: u32,
    has_first_collect: bool,
}

impl DiskCollector {
    pub fn new() -> Self {
        let mut collector = Self {
            query: None,
            counter_time: None,
            counter_read: None,
            counter_write: None,
            fail_count: 0,
            has_first_collect: false,
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

            let mut c_time = PDH_HCOUNTER::default();
            let mut c_read = PDH_HCOUNTER::default();
            let mut c_write = PDH_HCOUNTER::default();

            let res_time = PdhAddEnglishCounterW(
                q,
                w!("\\PhysicalDisk(_Total)\\% Disk Time"),
                0,
                &mut c_time,
            );
            let res_read = PdhAddEnglishCounterW(
                q,
                w!("\\PhysicalDisk(_Total)\\Disk Read Bytes/sec"),
                0,
                &mut c_read,
            );
            let res_write = PdhAddEnglishCounterW(
                q,
                w!("\\PhysicalDisk(_Total)\\Disk Write Bytes/sec"),
                0,
                &mut c_write,
            );

            if res_time == 0 {
                self.counter_time = Some(c_time);
            }
            if res_read == 0 {
                self.counter_read = Some(c_read);
            }
            if res_write == 0 {
                self.counter_write = Some(c_write);
            }

            let _ = PdhCollectQueryData(q);
            self.query = Some(q);
            self.fail_count = 0;
            self.has_first_collect = false;
        }
    }

    fn cleanup(&mut self) {
        if let Some(q) = self.query.take() {
            unsafe {
                let _ = PdhCloseQuery(q);
            }
        }
        self.counter_time = None;
        self.counter_read = None;
        self.counter_write = None;
        self.has_first_collect = false;
    }

    pub fn sample(&mut self) -> DiskMetrics {
        if self.query.is_none() {
            self.init_pdh();
            if self.query.is_none() {
                return DiskMetrics::default();
            }
        }

        let q = self.query.unwrap();
        unsafe {
            if PdhCollectQueryData(q) != 0 {
                self.fail_count += 1;
                if self.fail_count >= 2 {
                    self.init_pdh();
                }
                return DiskMetrics::default();
            }

            self.fail_count = 0;

            if !self.has_first_collect {
                self.has_first_collect = true;
                return DiskMetrics::default();
            }

            let mut activity = 0.0f32;
            let mut read_bytes_s = 0.0f64;
            let mut write_bytes_s = 0.0f64;

            if let Some(c) = self.counter_time {
                let mut val = PDH_FMT_COUNTERVALUE::default();
                if PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut val) == 0 {
                    activity = (val.Anonymous.doubleValue as f32).clamp(0.0, 100.0);
                }
            }

            if let Some(c) = self.counter_read {
                let mut val = PDH_FMT_COUNTERVALUE::default();
                if PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut val) == 0 {
                    read_bytes_s = val.Anonymous.doubleValue.max(0.0);
                }
            }

            if let Some(c) = self.counter_write {
                let mut val = PDH_FMT_COUNTERVALUE::default();
                if PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut val) == 0 {
                    write_bytes_s = val.Anonymous.doubleValue.max(0.0);
                }
            }

            let read_mb_s = read_bytes_s / (1024.0 * 1024.0);
            let write_mb_s = write_bytes_s / (1024.0 * 1024.0);

            DiskMetrics {
                activity_percent: activity,
                read_mb_s,
                write_mb_s,
                display: format!("D: {:2.0}%", activity.round()),
                tooltip_display: format!("读取: {:.1} MB/s, 写入: {:.1} MB/s", read_mb_s, write_mb_s),
            }
        }
    }

    pub fn reset_baseline(&mut self) {
        self.has_first_collect = false;
    }
}

impl Drop for DiskCollector {
    fn drop(&mut self) {
        self.cleanup();
    }
}
