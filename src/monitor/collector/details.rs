#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
    IDXGIAdapter3, IDXGIFactory4,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Debug, Clone)]
pub struct DetailMetrics {
    /// 显存专有显存详情 (如 "3.8 GB / 8.0 GB")
    pub vram_display: Option<String>,
    /// 供电状态 (如 "98% (接通电源)")，台式机无电池为 None
    pub power_display: Option<String>,
    /// 系统连续运行时间 (如 "3天 14小时 32分")
    pub uptime_display: String,
    /// 资源消耗最高的进程 (如 "chrome.exe (18%)")
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

pub struct DetailsCollector {
    last_process_times: HashMap<u32, (u64, String)>,
    last_total_sys_time: u64,
}

impl DetailsCollector {
    pub fn new() -> Self {
        Self {
            last_process_times: HashMap::new(),
            last_total_sys_time: 0,
        }
    }

    pub fn sample(&mut self) -> DetailMetrics {
        let vram_display = query_vram_info();
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
                            if !h_proc.is_invalid() {
                                let mut ct = FILETIME::default();
                                let mut et = FILETIME::default();
                                let mut kt = FILETIME::default();
                                let mut ut = FILETIME::default();
                                if GetProcessTimes(h_proc, &mut ct, &mut et, &mut kt, &mut ut).is_ok() {
                                    let proc_time = filetime_to_u64(&kt) + filetime_to_u64(&ut);
                                    current_map.insert(pid, (proc_time, name));
                                }
                                let _ = CloseHandle(h_proc);
                            }
                        }
                    }

                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);

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
                // 粗略百分比换算（按单核或总核比例）
                format!("{}", top_name)
            } else {
                "--".to_string()
            }
        }
    }
}

/// 查询 DXGI 专有显存分配
fn query_vram_info() -> Option<String> {
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1().ok()?;
        let adapter1 = factory.EnumAdapters1(0).ok()?;
        let adapter3: IDXGIAdapter3 = adapter1.cast().ok()?;

        let mut mem_info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        if adapter3
            .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut mem_info)
            .is_ok()
        {
            let gb = 1024.0 * 1024.0 * 1024.0;
            let used_gb = mem_info.CurrentUsage as f64 / gb;
            let budget_gb = mem_info.Budget as f64 / gb;
            if budget_gb > 0.05 {
                return Some(format!("{:.1} GB / {:.1} GB", used_gb, budget_gb));
            }
        }
        None
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
