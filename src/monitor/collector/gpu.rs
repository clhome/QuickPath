#![allow(unsafe_op_in_unsafe_fn)]

use windows::core::w;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};
use windows::Win32::System::ProcessStatus::K32GetProcessImageFileNameW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

const PDH_MORE_DATA: u32 = 0x800007D2;

#[derive(Debug, Clone)]
pub struct GpuMetrics {
    /// GPU 综合利用率百分比 (0.0 ~ 100.0)，若设备不支持则为 None
    pub usage_percent: Option<f32>,
    /// 格式化用于常驻显示的文本，如 "G: 32%" 或 "G: --%"
    pub display: String,
    /// 占用 GPU 最高进程名称，如 "dwm.exe" 或 "--"
    pub top_process_display: String,
}

impl Default for GpuMetrics {
    fn default() -> Self {
        Self {
            usage_percent: None,
            display: "G: --%".to_string(),
            top_process_display: "--".to_string(),
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
            let mut pid_usages: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
            for item in items {
                let val = item.FmtValue.Anonymous.doubleValue;
                if val > 0.0 {
                    total_usage += val;
                    if let Some(pid) = parse_gpu_pid(item.szName.0) {
                        *pid_usages.entry(pid).or_insert(0.0) += val;
                    }
                }
            }

            let mut top_process_display = "--".to_string();
            if let Some((&top_pid, &max_val)) = pid_usages
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            {
                if max_val >= 1.0 {
                    if let Some(name) = get_process_name_by_pid(top_pid) {
                        top_process_display = name;
                    }
                }
            }

            let clamped = (total_usage as f32).clamp(0.0, 100.0);
            GpuMetrics {
                usage_percent: Some(clamped),
                display: format!("G: {:2.0}%", clamped.round()),
                top_process_display,
            }
        }
    }

    pub fn reset_baseline(&mut self) {
        self.has_first_collect = false;
    }
}

/// 快速从 GPU Engine 实例名中提取 PID（如 "pid_1234_luid_..."），零堆分配
unsafe fn parse_gpu_pid(mut ptr: *const u16) -> Option<u32> {
    if ptr.is_null() {
        return None;
    }
    let mut offset = 0;
    while *ptr.add(offset) != 0 && offset < 32 {
        if *ptr.add(offset) == b'p' as u16
            && *ptr.add(offset + 1) == b'i' as u16
            && *ptr.add(offset + 2) == b'd' as u16
            && *ptr.add(offset + 3) == b'_' as u16
        {
            ptr = ptr.add(offset + 4);
            let mut pid: u32 = 0;
            let mut digits = 0;
            while *ptr >= b'0' as u16 && *ptr <= b'9' as u16 {
                pid = pid * 10 + (*ptr - b'0' as u16) as u32;
                digits += 1;
                ptr = ptr.add(1);
            }
            if digits > 0 {
                return Some(pid);
            }
            return None;
        }
        offset += 1;
    }
    None
}

/// 根据 PID 极速获取可执行文件名（如 "dwm.exe"），仅对 Top 进程单次查询
unsafe fn get_process_name_by_pid(pid: u32) -> Option<String> {
    if pid == 0 || pid == 4 {
        return Some("System".to_string());
    }
    if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        let mut buf = [0u16; 260];
        let len = K32GetProcessImageFileNameW(handle, &mut buf);
        let _ = CloseHandle(handle);
        if len > 0 {
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            if let Some(idx) = path.rfind('\\') {
                return Some(path[idx + 1..].to_string());
            } else {
                return Some(path);
            }
        }
    }
    None
}

impl Drop for GpuCollector {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gpu_pid() {
        let sample = "pid_22764_luid_0x00000000_0x0000D0EB_phys_0_eng_0_engtype_3D\0";
        let u16_sample: Vec<u16> = sample.encode_utf16().collect();
        let pid = unsafe { parse_gpu_pid(u16_sample.as_ptr()) };
        assert_eq!(pid, Some(22764));

        let invalid = "unknown_engine_instance_name\0";
        let u16_invalid: Vec<u16> = invalid.encode_utf16().collect();
        let pid_invalid = unsafe { parse_gpu_pid(u16_invalid.as_ptr()) };
        assert_eq!(pid_invalid, None);
    }
}
