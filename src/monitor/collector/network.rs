#![allow(unsafe_op_in_unsafe_fn)]

use std::time::Instant;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
};

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_OPER_STATUS_UP: i32 = 1;

#[derive(Debug, Clone)]
pub struct NetworkSpeed {
    /// 实时上传速率 (Mbps)
    pub upload_mbps: f64,
    /// 实时下载速率 (Mbps)
    pub download_mbps: f64,
    /// 实时上传速率 (MB/s)
    pub upload_mb_s: f64,
    /// 实时下载速率 (MB/s)
    pub download_mb_s: f64,
    /// 格式化用于任务栏常驻显示的上传文本 (省略单位，如 "↑ 1.2" 或 "↑ 120")
    pub upload_display: String,
    /// 格式化用于任务栏常驻显示的下载文本 (省略单位，如 "↓ 3.5" 或 "↓ 350")
    pub download_display: String,
    /// 活跃物理网卡友好名称
    pub adapter_name: String,
    /// 会话期间累计已用流量 (字节)
    pub total_bytes_session: u64,
}

impl Default for NetworkSpeed {
    fn default() -> Self {
        Self {
            upload_mbps: 0.0,
            download_mbps: 0.0,
            upload_mb_s: 0.0,
            download_mb_s: 0.0,
            upload_display: "↑ 0.0".to_string(),
            download_display: "↓ 0.0".to_string(),
            adapter_name: "以太网 / Wi-Fi".to_string(),
            total_bytes_session: 0,
        }
    }
}

pub struct NetworkCollector {
    last_in_octets: u64,
    last_out_octets: u64,
    last_sample_time: Option<Instant>,
    initial_total_octets: Option<u64>,
    primary_adapter_name: String,
}

impl NetworkCollector {
    pub fn new() -> Self {
        let mut collector = Self {
            last_in_octets: 0,
            last_out_octets: 0,
            last_sample_time: None,
            initial_total_octets: None,
            primary_adapter_name: "以太网 / Wi-Fi".to_string(),
        };
        let _ = collector.sample();
        collector
    }

    /// 采集瞬时网络速率
    pub fn sample(&mut self) -> NetworkSpeed {
        let now = Instant::now();
        let (cur_in, cur_out, adapter_name) = match self.fetch_octets() {
            Some(data) => data,
            None => {
                return NetworkSpeed {
                    adapter_name: self.primary_adapter_name.clone(),
                    ..Default::default()
                };
            }
        };

        if !adapter_name.is_empty() {
            self.primary_adapter_name = adapter_name.clone();
        }

        let cur_total = cur_in.saturating_add(cur_out);
        if self.initial_total_octets.is_none() {
            self.initial_total_octets = Some(cur_total);
        }
        let total_session = cur_total.saturating_sub(self.initial_total_octets.unwrap_or(cur_total));

        let last_time = match self.last_sample_time {
            Some(t) => t,
            None => {
                self.last_in_octets = cur_in;
                self.last_out_octets = cur_out;
                self.last_sample_time = Some(now);
                return NetworkSpeed {
                    adapter_name: self.primary_adapter_name.clone(),
                    total_bytes_session: total_session,
                    ..Default::default()
                };
            }
        };

        let elapsed_sec = now.duration_since(last_time).as_secs_f64();
        // 异常时间差（休眠或过长）防御
        if elapsed_sec <= 0.05 || elapsed_sec > 10.0 {
            self.last_in_octets = cur_in;
            self.last_out_octets = cur_out;
            self.last_sample_time = Some(now);
            return NetworkSpeed {
                adapter_name: self.primary_adapter_name.clone(),
                total_bytes_session: total_session,
                ..Default::default()
            };
        }

        let delta_in = cur_in.saturating_sub(self.last_in_octets);
        let delta_out = cur_out.saturating_sub(self.last_out_octets);

        self.last_in_octets = cur_in;
        self.last_out_octets = cur_out;
        self.last_sample_time = Some(now);

        // 字节/秒
        let in_bytes_per_sec = (delta_in as f64) / elapsed_sec;
        let out_bytes_per_sec = (delta_out as f64) / elapsed_sec;

        // MB/s (1024 * 1024)
        let download_mb_s = in_bytes_per_sec / (1024.0 * 1024.0);
        let upload_mb_s = out_bytes_per_sec / (1024.0 * 1024.0);

        // Mbps (1,000,000 bits)
        let download_mbps = (in_bytes_per_sec * 8.0) / 1_000_000.0;
        let upload_mbps = (out_bytes_per_sec * 8.0) / 1_000_000.0;

        NetworkSpeed {
            upload_mbps,
            download_mbps,
            upload_mb_s,
            download_mb_s,
            upload_display: format_speed_compact('↑', upload_mbps),
            download_display: format_speed_compact('↓', download_mbps),
            adapter_name: self.primary_adapter_name.clone(),
            total_bytes_session: total_session,
        }
    }

    /// 重置基准（用于休眠唤醒）
    pub fn reset_baseline(&mut self) {
        self.last_sample_time = None;
    }

    /// 读取所有有效物理网卡当前的 In/Out Octets
    fn fetch_octets(&self) -> Option<(u64, u64, String)> {
        unsafe {
            let mut table_ptr: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
            if GetIfTable2(&mut table_ptr).is_err() || table_ptr.is_null() {
                return None;
            }

            let num_entries = (*table_ptr).NumEntries as usize;
            let rows_slice = std::slice::from_raw_parts((*table_ptr).Table.as_ptr(), num_entries);

            let mut total_in: u64 = 0;
            let mut total_out: u64 = 0;
            let mut detected_name = String::new();
            let mut max_traffic: u64 = 0;

            for row in rows_slice {
                if !is_physical_active_adapter(row) {
                    continue;
                }

                total_in = total_in.saturating_add(row.InOctets);
                total_out = total_out.saturating_add(row.OutOctets);

                let traffic = row.InOctets.saturating_add(row.OutOctets);
                if traffic >= max_traffic {
                    max_traffic = traffic;
                    let desc = parse_utf16_desc(&row.Description);
                    if !desc.is_empty() {
                        detected_name = desc;
                    }
                }
            }

            FreeMibTable(table_ptr as *const _);
            Some((total_in, total_out, detected_name))
        }
    }
}

/// 判定是否为有效的物理活跃网卡 (PRD 2.2 过滤策略)
fn is_physical_active_adapter(row: &MIB_IF_ROW2) -> bool {
    // 1. 过滤回环
    if row.Type == IF_TYPE_SOFTWARE_LOOPBACK {
        return false;
    }

    // 2. 过滤非连接状态 (非 IfOperStatusUp)
    if row.OperStatus.0 != IF_OPER_STATUS_UP {
        return false;
    }

    // 3. 检查网卡描述，过滤各类虚拟、容器、VPN 适配器
    let desc = parse_utf16_desc(&row.Description);
    let desc_lower = desc.to_lowercase();

    const VIRTUAL_KEYWORDS: &[&str] = &[
        "virtual",
        "hyper-v",
        "vmware",
        "virtualbox",
        "loopback",
        "tap",
        "tun",
        "tailscale",
        "zerotier",
        "wsl",
        "wireguard",
        "clash",
        "veth",
        "docker",
        "bluetooth",
        "npcap",
    ];

    for kw in VIRTUAL_KEYWORDS {
        if desc_lower.contains(kw) {
            return false;
        }
    }

    true
}

/// 解析 MIB_IF_ROW2.Description 宽字符
fn parse_utf16_desc(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim().to_string()
}

/// PRD 3.1 紧凑网速排版规格
pub fn format_speed_compact(prefix: char, mbps: f64) -> String {
    let clamped = mbps.max(0.0);
    if clamped < 100.0 {
        format!("{} {:4.1}", prefix, clamped)
    } else if clamped < 9999.0 {
        format!("{} {:4.0}", prefix, clamped)
    } else {
        format!("{} 999+", prefix)
    }
}
