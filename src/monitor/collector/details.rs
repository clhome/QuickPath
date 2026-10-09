#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory4};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Debug, Clone)]
pub struct DetailMetrics {
    /// 显存专有显存详情 (如 "0.4 GB / 2.0 GB")
    pub vram_display: Option<String>,
    /// 供电状态 (如 "98% (接通电源)")，台式机无电池为 None
    pub power_display: Option<String>,
    /// 系统连续运行时间 (如 "3天 14小时 32分")
    pub uptime_display: String,
    /// 资源消耗最高的进程 (如 "chrome.exe")
    pub top_process_display: String,
}

impl Default for DetailMetrics {
    fn default() -> Self {
        Self {
            vram_display: None,
            power_display: None,
            uptime_display: "0天 0小时 0分".to_string(),
            top_process_display: "--".to_string(),
        }
    }
}

/// 独立显卡专用显存 (Dedicated VRAM) 采集器
struct VramCollector {
    query: Option<PDH_HQUERY>,
    counter: Option<PDH_HCOUNTER>,
    buffer: Vec<u8>,
    dedicated_total_bytes: usize,
    target_luid_pattern: Option<String>,
}

impl VramCollector {
    fn new() -> Self {
        let (dedicated_total_bytes, target_luid_pattern) = Self::detect_discrete_gpu();
        let mut collector = Self {
            query: None,
            counter: None,
            buffer: vec![0u8; 4096],
            dedicated_total_bytes,
            target_luid_pattern,
        };
        collector.init_pdh();
        collector
    }

    /// 遍历 DXGI 所有物理适配器，选出专用物理显存 (DedicatedVideoMemory) 最大的独立显卡
    fn detect_discrete_gpu() -> (usize, Option<String>) {
        unsafe {
            let factory: IDXGIFactory4 = match CreateDXGIFactory1() {
                Ok(f) => f,
                Err(_) => return (0, None),
            };

            let mut best_dedicated = 0usize;
            let mut best_luid = None;
            let mut i = 0;
            while let Ok(adapter1) = factory.EnumAdapters1(i) {
                if let Ok(desc) = adapter1.GetDesc1() {
                    // 排除软件模拟驱动 (DXGI_ADAPTER_FLAG_SOFTWARE = 2)
                    if (desc.Flags & 2) == 0 && desc.DedicatedVideoMemory > best_dedicated {
                        best_dedicated = desc.DedicatedVideoMemory;
                        best_luid = Some(desc.AdapterLuid);
                    }
                }
                i += 1;
            }

            let luid_pat = best_luid.map(|l| format!("{:08x}_{:08x}", l.HighPart, l.LowPart).to_lowercase());
            (best_dedicated, luid_pat)
        }
    }

    fn init_pdh(&mut self) {
        self.cleanup();
        unsafe {
            let mut q = PDH_HQUERY::default();
            if PdhOpenQueryW(None, 0, &mut q) != 0 {
                return;
            }
            let mut c = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(
                q,
                w!("\\GPU Adapter Memory(*)\\Dedicated Usage"),
                0,
                &mut c,
            ) == 0 {
                let _ = PdhCollectQueryData(q);
                self.query = Some(q);
                self.counter = Some(c);
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
    }

    fn sample(&mut self) -> Option<String> {
        if self.dedicated_total_bytes == 0 {
            let (tot, pat) = Self::detect_discrete_gpu();
            if tot == 0 {
                return None;
            }
            self.dedicated_total_bytes = tot;
            self.target_luid_pattern = pat;
        }

        let gb = 1024.0 * 1024.0 * 1024.0;
        let total_gb = self.dedicated_total_bytes as f64 / gb;

        if self.query.is_none() {
            self.init_pdh();
        }

        let mut used_bytes: f64 = 0.0;
        if let (Some(q), Some(c)) = (self.query, self.counter) {
            unsafe {
                if PdhCollectQueryData(q) == 0 {
                    let mut buf_size: u32 = self.buffer.len() as u32;
                    let mut item_count: u32 = 0;
                    let mut res = PdhGetFormattedCounterArrayW(
                        c,
                        PDH_FMT_DOUBLE,
                        &mut buf_size,
                        &mut item_count,
                        Some(self.buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
                    );

                    if res == 0x800007D2 { // PDH_MORE_DATA
                        self.buffer.resize(buf_size as usize + 2048, 0);
                        let mut new_size = self.buffer.len() as u32;
                        res = PdhGetFormattedCounterArrayW(
                            c,
                            PDH_FMT_DOUBLE,
                            &mut new_size,
                            &mut item_count,
                            Some(self.buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
                        );
                    }

                    if res == 0 && item_count > 0 {
                        let items = std::slice::from_raw_parts(
                            self.buffer.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W,
                            item_count as usize,
                        );

                        let mut matched = false;
                        for item in items {
                            let val = item.FmtValue.Anonymous.doubleValue;
                            if let Some(target) = &self.target_luid_pattern {
                                let ptr = item.szName.0;
                                if !ptr.is_null() {
                                    let name_len = (0..).take_while(|&idx| *ptr.add(idx) != 0).count();
                                    let name_slice = std::slice::from_raw_parts(ptr, name_len);
                                    let name = String::from_utf16_lossy(name_slice).to_lowercase();
                                    if name.contains(target) {
                                        used_bytes = val;
                                        matched = true;
                                        break;
                                    }
                                }
                            }
                        }

                        // 若未精确匹配到目标 LUID，则取 Dedicated Usage 最大的物理适配器
                        if !matched {
                            for item in items {
                                let val = item.FmtValue.Anonymous.doubleValue;
                                if val > used_bytes {
                                    used_bytes = val;
                                }
                            }
                        }
                    }
                }
            }
        }

        let used_gb = used_bytes / gb;
        Some(format!("{:.1} GB / {:.1} GB", used_gb, total_gb))
    }
}

impl Drop for VramCollector {
    fn drop(&mut self) {
        self.cleanup();
    }
}

pub struct DetailsCollector {
    last_process_times: HashMap<u32, (u64, String)>,
    last_total_sys_time: u64,
    vram_collector: VramCollector,
}

impl DetailsCollector {
    pub fn new() -> Self {
        Self {
            last_process_times: HashMap::new(),
            last_total_sys_time: 0,
            vram_collector: VramCollector::new(),
        }
    }

    pub fn sample(&mut self) -> DetailMetrics {
        let vram_display = self.vram_collector.sample();
        let power_display = query_power_status();
        let uptime_display = query_system_uptime();
        let top_process_display = self.query_top_process();

        DetailMetrics {
            vram_display,
            power_display,
            uptime_display,
            top_process_display,
        }
    }

    fn query_top_process(&mut self) -> String {
        unsafe {
            let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
                Ok(h) if !h.is_invalid() => h,
                _ => return "--".to_string(),
            };

            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };

            let mut current_map: HashMap<u32, (u64, String)> = HashMap::new();

            if Process32FirstW(snap, &mut entry).is_ok() {
                loop {
                    let pid = entry.th32ProcessID;
                    if pid != 0 && pid != 4 {
                        let name_len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                        let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);

                        if let Ok(h_proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                            let mut creation = FILETIME::default();
                            let mut exit = FILETIME::default();
                            let mut kernel = FILETIME::default();
                            let mut user = FILETIME::default();

                            if GetProcessTimes(h_proc, &mut creation, &mut exit, &mut kernel, &mut user).is_ok() {
                                let total_time = filetime_to_u64(&kernel) + filetime_to_u64(&user);
                                current_map.insert(pid, (total_time, name));
                            }
                            let _ = CloseHandle(h_proc);
                        }
                    }

                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);

            // 比较上一轮时间戳找出 CPU 耗时增量最大的进程
            let mut max_delta = 0u64;
            let mut top_name = String::new();

            for (pid, (cur_time, name)) in &current_map {
                if let Some((prev_time, _)) = self.last_process_times.get(pid) {
                    let delta = cur_time.saturating_sub(*prev_time);
                    if delta > max_delta {
                        max_delta = delta;
                        top_name = name.clone();
                    }
                }
            }

            self.last_process_times = current_map;

            if max_delta > 0 && !top_name.is_empty() {
                format!("{}", top_name)
            } else {
                "--".to_string()
            }
        }
    }
}

/// 查询电池电量与供电状态
fn query_power_status() -> Option<String> {
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_ok() {
            // 检查是否有电池：BatteryFlag 128 代表 No system battery
            if status.BatteryFlag != 128 && status.BatteryLifePercent <= 100 {
                let percent = status.BatteryLifePercent;
                let state_str = if status.ACLineStatus == 1 {
                    "接通电源"
                } else {
                    "电池供电"
                };
                return Some(format!("{}% ({})", percent, state_str));
            }
        }
        None
    }
}

/// 查询系统连续运行总时长 (不受快速启动假关机影响)
fn query_system_uptime() -> String {
    let ms = unsafe { GetTickCount64() };
    let total_secs = ms / 1000;
    let days = total_secs / (3600 * 24);
    let hours = (total_secs / 3600) % 24;
    let minutes = (total_secs / 60) % 60;

    if days > 0 {
        format!("{}天 {}小时 {}分", days, hours, minutes)
    } else if hours > 0 {
        format!("{}小时 {}分", hours, minutes)
    } else {
        format!("{}分", minutes.max(1))
    }
}

fn filetime_to_u64(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vram_collector() {
        let mut collector = VramCollector::new();
        let vram = collector.sample();
        println!("VRAM Sample Result: {:?}", vram);
        if let Some(res) = vram {
            assert!(res.contains("GB / "));
            assert!(!res.contains("47.")); // 绝不可再包含 47.x GB 共享内存！
        }
    }
}
