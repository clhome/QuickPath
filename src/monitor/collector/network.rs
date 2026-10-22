#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::time::Instant;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
};

/// 局域网以太网类型 (RFC 1213 / IANA)
const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
/// 无线局域网 IEEE 802.11 类型
const IF_TYPE_IEEE80211: u32 = 71;
/// 移动宽带 4G/5G WWAN 类型
const IF_TYPE_WWANPP: u32 = 243;
const IF_TYPE_WWANPP2: u32 = 244;

/// 接口连接正常工作状态
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
    /// 会话期间累计已用物理流量 (字节)
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

/// 核心网速增量跟踪状态机（按 Interface LUID 独立跟踪增量，杜绝全局累加跳变）
#[derive(Debug, Default)]
pub struct NetworkDeltaTracker {
    pub adapters_history: HashMap<u64, (u64, u64)>,
    pub session_bytes: u64,
    pub primary_adapter_name: String,
}

impl NetworkDeltaTracker {
    pub fn new() -> Self {
        Self {
            adapters_history: HashMap::new(),
            session_bytes: 0,
            primary_adapter_name: "以太网 / Wi-Fi".to_string(),
        }
    }

    /// 根据当前轮采样的所有在线物理网卡数据更新增量并返回 `(delta_in, delta_out)`
    pub fn update(&mut self, active_rows: &[(u64, u64, u64, String)]) -> (u64, u64) {
        let mut total_delta_in: u64 = 0;
        let mut total_delta_out: u64 = 0;
        let mut max_card_delta: u64 = 0;
        let mut most_active_name = String::new();
        let mut active_luids = Vec::with_capacity(active_rows.len());

        for (luid, cur_in, cur_out, name) in active_rows {
            active_luids.push(*luid);

            if let Some((prev_in, prev_out)) = self.adapters_history.get_mut(luid) {
                // 已有网卡：计算此物理网卡在本采样周期的真实增量
                let d_in = cur_in.saturating_sub(*prev_in);
                let d_out = cur_out.saturating_sub(*prev_out);
                total_delta_in = total_delta_in.saturating_add(d_in);
                total_delta_out = total_delta_out.saturating_add(d_out);

                let card_delta = d_in.saturating_add(d_out);
                if card_delta > max_card_delta && !name.is_empty() {
                    max_card_delta = card_delta;
                    most_active_name = name.clone();
                }

                *prev_in = *cur_in;
                *prev_out = *cur_out;
            } else {
                // 新上线网卡（如热插拔/Wi-Fi重连）：首轮仅建立 baseline，当期增量记为 0，杜绝数十GB历史累计虚假暴增
                self.adapters_history.insert(*luid, (*cur_in, *cur_out));
                if most_active_name.is_empty() && !name.is_empty() {
                    most_active_name = name.clone();
                }
            }
        }

        // 移除已下线/断开连接的网卡历史条目
        self.adapters_history
            .retain(|k, _| active_luids.contains(k));

        // 动态更新活跃主网卡友好名称：有流量时选用当前流量最大的物理网卡，静默时保持
        if !most_active_name.is_empty()
            && (max_card_delta > 0 || self.primary_adapter_name == "以太网 / Wi-Fi")
        {
            self.primary_adapter_name = most_active_name;
        }

        // 会话累计真实物理流量单调递增
        self.session_bytes = self
            .session_bytes
            .saturating_add(total_delta_in)
            .saturating_add(total_delta_out);

        (total_delta_in, total_delta_out)
    }

    pub fn clear(&mut self) {
        self.adapters_history.clear();
    }
}

pub struct NetworkCollector {
    tracker: NetworkDeltaTracker,
    last_sample_time: Option<Instant>,
}

impl NetworkCollector {
    pub fn new() -> Self {
        let mut collector = Self {
            tracker: NetworkDeltaTracker::new(),
            last_sample_time: None,
        };
        let _ = collector.sample();
        collector
    }

    /// 采集瞬时网络速率
    pub fn sample(&mut self) -> NetworkSpeed {
        let now = Instant::now();
        let active_rows = match self.fetch_active_adapters() {
            Some(rows) => rows,
            None => {
                return NetworkSpeed {
                    adapter_name: self.tracker.primary_adapter_name.clone(),
                    total_bytes_session: self.tracker.session_bytes,
                    ..Default::default()
                };
            }
        };

        let last_time = match self.last_sample_time {
            Some(t) => t,
            None => {
                // 首轮采样：建立所有当前在线物理网卡的初始 baseline
                self.tracker.update(&active_rows);
                self.last_sample_time = Some(now);
                return NetworkSpeed {
                    adapter_name: self.tracker.primary_adapter_name.clone(),
                    total_bytes_session: self.tracker.session_bytes,
                    ..Default::default()
                };
            }
        };

        let elapsed_sec = now.duration_since(last_time).as_secs_f64();
        // 异常时间差（休眠唤醒或调度阻塞过长）防御：重置基准，避免虚假突发
        if elapsed_sec <= 0.05 || elapsed_sec > 10.0 {
            self.tracker.clear();
            self.tracker.update(&active_rows);
            self.last_sample_time = Some(now);
            return NetworkSpeed {
                adapter_name: self.tracker.primary_adapter_name.clone(),
                total_bytes_session: self.tracker.session_bytes,
                ..Default::default()
            };
        }

        let (total_delta_in, total_delta_out) = self.tracker.update(&active_rows);
        self.last_sample_time = Some(now);

        // 字节/秒
        let in_bytes_per_sec = (total_delta_in as f64) / elapsed_sec;
        let out_bytes_per_sec = (total_delta_out as f64) / elapsed_sec;

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
            adapter_name: self.tracker.primary_adapter_name.clone(),
            total_bytes_session: self.tracker.session_bytes,
        }
    }

    /// 重置基准（用于休眠唤醒）
    pub fn reset_baseline(&mut self) {
        self.last_sample_time = None;
        self.tracker.clear();
    }

    /// 读取所有有效物理网卡当前状态及 In/Out Octets
    fn fetch_active_adapters(&self) -> Option<Vec<(u64, u64, u64, String)>> {
        unsafe {
            let mut table_ptr: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
            if GetIfTable2(&mut table_ptr).is_err() || table_ptr.is_null() {
                return None;
            }

            let num_entries = (*table_ptr).NumEntries as usize;
            let rows_slice = std::slice::from_raw_parts((*table_ptr).Table.as_ptr(), num_entries);

            let mut results = Vec::with_capacity(num_entries.min(8));

            for row in rows_slice {
                if !is_physical_active_adapter(row) {
                    continue;
                }

                let luid = row.InterfaceLuid.Value;
                let desc = parse_utf16_desc(&row.Description);
                let alias = parse_utf16_desc(&row.Alias);

                let display_name = if !desc.is_empty() {
                    desc
                } else if !alias.is_empty() {
                    alias
                } else {
                    "物理网络接口".to_string()
                };

                results.push((luid, row.InOctets, row.OutOctets, display_name));
            }

            FreeMibTable(table_ptr as *const _);
            Some(results)
        }
    }
}

/// 判定是否为有效的物理活跃网卡 (彻底过滤虚拟网卡、VPN隧道及NDIS轻量级过滤驱动)
fn is_physical_active_adapter(row: &MIB_IF_ROW2) -> bool {
    // 1. 过滤非连接状态 (非 IfOperStatusUp)
    if row.OperStatus.0 != IF_OPER_STATUS_UP {
        return false;
    }

    // 2. 接口类型严格限定为物理局域网/无线网络/蜂窝网络，彻底排除隧道 (131)、回环 (24)、PPP (23) 等
    if row.Type != IF_TYPE_ETHERNET_CSMACD
        && row.Type != IF_TYPE_IEEE80211
        && row.Type != IF_TYPE_WWANPP
        && row.Type != IF_TYPE_WWANPP2
    {
        return false;
    }

    // 3. 检查 Win32 接口状态位域标志 (微软标准硬件网卡与过滤驱动识别):
    //    Bit 0: HardwareInterface (必须为真，排除纯软件虚拟网卡)
    //    Bit 1: FilterInterface (必须为假，排除 NDIS 轻量级过滤驱动 LWF，杜绝多倍重复累加)
    let flags = row.InterfaceAndOperStatusFlags._bitfield;
    let is_hardware = (flags & 0x01) != 0;
    let is_filter = (flags & 0x02) != 0;

    if !is_hardware || is_filter {
        return false;
    }

    // 4. 双重防线：针对某些未规范设置标志位的非标驱动做关键字黑名单防御
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

/// 解析 MIB_IF_ROW2.Description / Alias 宽字符
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::NetworkManagement::IpHelper::MIB_IF_ROW2_0;
    use windows::Win32::NetworkManagement::Ndis::IF_OPER_STATUS;

    fn make_mock_row(if_type: u32, oper_status: i32, bitfield: u8, desc: &str) -> MIB_IF_ROW2 {
        let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
        row.Type = if_type;
        row.OperStatus = IF_OPER_STATUS(oper_status);
        row.InterfaceAndOperStatusFlags = MIB_IF_ROW2_0 {
            _bitfield: bitfield,
        };

        let utf16: Vec<u16> = desc.encode_utf16().collect();
        for (i, &ch) in utf16.iter().enumerate() {
            if i < row.Description.len() {
                row.Description[i] = ch;
            }
        }
        row
    }

    #[test]
    fn test_physical_active_adapter_filtering() {
        // 1. 真实物理以太网网卡: Type 6, OperStatus 1, Bitfield 0x05 (Hw=true, Filter=false, Conn=true) -> 应通过
        let realtek = make_mock_row(
            IF_TYPE_ETHERNET_CSMACD,
            IF_OPER_STATUS_UP,
            0x05,
            "Realtek Gaming 2.5GbE Family Controller",
        );
        assert!(is_physical_active_adapter(&realtek));

        // 2. 真实物理 Wi-Fi 网卡: Type 71, OperStatus 1, Bitfield 0x05 -> 应通过
        let intel_wifi = make_mock_row(
            IF_TYPE_IEEE80211,
            IF_OPER_STATUS_UP,
            0x05,
            "Intel(R) Wi-Fi 6E AX210 160MHz",
        );
        assert!(is_physical_active_adapter(&intel_wifi));

        // 3. 真实 4G/5G 移动网卡: Type 243, OperStatus 1, Bitfield 0x01 -> 应通过
        let wwan = make_mock_row(
            IF_TYPE_WWANPP,
            IF_OPER_STATUS_UP,
            0x01,
            "Fibocom 5G Wireless Network Adapter",
        );
        assert!(is_physical_active_adapter(&wwan));

        // 4. NDIS 轻量级过滤驱动 (火绒/QoS/WFP等): 即使驱动标了 Hw=true，但只要 Filter=true (Bitfield 0x03) -> 必须被拒绝（防5倍放大）
        let huorong_filter = make_mock_row(
            IF_TYPE_ETHERNET_CSMACD,
            IF_OPER_STATUS_UP,
            0x03,
            "Realtek Gaming 2.5GbE Family Controller-Huorong NDIS Filter Driver-0000",
        );
        assert!(!is_physical_active_adapter(&huorong_filter));

        let qos_filter = make_mock_row(
            IF_TYPE_ETHERNET_CSMACD,
            IF_OPER_STATUS_UP,
            0x02,
            "Realtek Gaming 2.5GbE Family Controller-QoS Packet Scheduler-0000",
        );
        assert!(!is_physical_active_adapter(&qos_filter));

        // 5. 纯软件虚拟网卡: Type 6, OperStatus 1, Bitfield 0x00 (Hw=false) -> 必须被拒绝
        let v_ethernet = make_mock_row(
            IF_TYPE_ETHERNET_CSMACD,
            IF_OPER_STATUS_UP,
            0x00,
            "Hyper-V Virtual Ethernet Adapter",
        );
        assert!(!is_physical_active_adapter(&v_ethernet));

        // 6. 回环接口 (Type 24) -> 必须被拒绝
        let loopback = make_mock_row(24, IF_OPER_STATUS_UP, 0x00, "Software Loopback Interface 1");
        assert!(!is_physical_active_adapter(&loopback));

        // 7. 隧道接口 (Type 131) -> 必须被拒绝
        let tunnel = make_mock_row(
            131,
            IF_OPER_STATUS_UP,
            0x00,
            "Microsoft Teredo Tunneling Adapter",
        );
        assert!(!is_physical_active_adapter(&tunnel));

        // 8. 断开连接的物理网卡: OperStatus != 1 -> 必须被拒绝
        let disconnected_wifi = make_mock_row(
            IF_TYPE_IEEE80211,
            2, // Down
            0x05,
            "Intel(R) Wi-Fi 6E AX210 160MHz",
        );
        assert!(!is_physical_active_adapter(&disconnected_wifi));

        // 9. 描述包含虚拟关键字的网卡 -> 必须被拒绝
        let clash_tap = make_mock_row(
            IF_TYPE_ETHERNET_CSMACD,
            IF_OPER_STATUS_UP,
            0x01,
            "Clash Core TAP-Windows Adapter V9",
        );
        assert!(!is_physical_active_adapter(&clash_tap));
    }

    #[test]
    fn test_network_delta_tracker_baseline_no_spike() {
        let mut tracker = NetworkDeltaTracker::new();

        // 模拟开机已久的有线网卡，携带 50 GB 历史累积流量
        let round1 = vec![(
            1001u64,
            50_000_000_000u64,
            10_000_000_000u64,
            "Realtek 2.5GbE".to_string(),
        )];

        // 第一轮采样：必须建立基准，delta 必须为 0，不能将 50GB 当作增量！
        let (d_in, d_out) = tracker.update(&round1);
        assert_eq!(d_in, 0);
        assert_eq!(d_out, 0);
        assert_eq!(tracker.session_bytes, 0);
        assert_eq!(tracker.primary_adapter_name, "Realtek 2.5GbE");

        // 第二轮采样：真实下载了 1024 字节，上传了 512 字节
        let round2 = vec![(
            1001u64,
            50_000_001_024u64,
            10_000_000_512u64,
            "Realtek 2.5GbE".to_string(),
        )];
        let (d_in, d_out) = tracker.update(&round2);
        assert_eq!(d_in, 1024);
        assert_eq!(d_out, 512);
        assert_eq!(tracker.session_bytes, 1536);

        // 第三轮采样：突然连上 Wi-Fi 网卡（携带 20 GB 历史流量）
        let round3 = vec![
            (
                1001u64,
                50_000_002_048u64,
                10_000_001_024u64,
                "Realtek 2.5GbE".to_string(),
            ),
            (
                2002u64,
                20_000_000_000u64,
                5_000_000_000u64,
                "Intel Wi-Fi 6E".to_string(),
            ),
        ];
        // 关键断言：Wi-Fi 第一次出现，其 20GB 历史绝对不能漏进 delta！
        let (d_in, d_out) = tracker.update(&round3);
        assert_eq!(d_in, 1024); // 仅有有线网卡的增量
        assert_eq!(d_out, 512);
        assert_eq!(tracker.session_bytes, 1536 + 1536);

        // 第四轮采样：Wi-Fi 产生真实 2048 字节下载，有线产生 1024 字节下载
        let round4 = vec![
            (
                1001u64,
                50_000_003_072u64,
                10_000_001_024u64,
                "Realtek 2.5GbE".to_string(),
            ),
            (
                2002u64,
                20_000_002_048u64,
                5_000_000_512u64,
                "Intel Wi-Fi 6E".to_string(),
            ),
        ];
        let (d_in, d_out) = tracker.update(&round4);
        assert_eq!(d_in, 1024 + 2048);
        assert_eq!(d_out, 512);
        assert_eq!(tracker.session_bytes, 3072 + 3584);
        // Wi-Fi 流量更大，活跃主网卡应切换为 Wi-Fi
        assert_eq!(tracker.primary_adapter_name, "Intel Wi-Fi 6E");
    }

    #[test]
    fn test_network_delta_tracker_adapter_offline_resilience() {
        let mut tracker = NetworkDeltaTracker::new();

        let round1 = vec![
            (1u64, 1000u64, 500u64, "Card A".to_string()),
            (2u64, 2000u64, 800u64, "Card B".to_string()),
        ];
        tracker.update(&round1);

        // Card A 产生增量，Card B 离线（不再出现在活跃列表中）
        let round2 = vec![(1u64, 1500u64, 600u64, "Card A".to_string())];
        let (d_in, d_out) = tracker.update(&round2);
        assert_eq!(d_in, 500);
        assert_eq!(d_out, 100);
        // Card B 应已从历史记录中被安全摘除，不会引起任何越界或异常
        assert!(!tracker.adapters_history.contains_key(&2));
    }

    #[test]
    fn test_format_speed_compact() {
        assert_eq!(format_speed_compact('↑', 0.0), "↑  0.0");
        assert_eq!(format_speed_compact('↑', 1.23), "↑  1.2");
        assert_eq!(format_speed_compact('↓', 99.9), "↓ 99.9");
        assert_eq!(format_speed_compact('↓', 100.0), "↓  100");
        assert_eq!(format_speed_compact('↓', 999.4), "↓  999");
        assert_eq!(format_speed_compact('↑', 1250.0), "↑ 1250");
        assert_eq!(format_speed_compact('↓', 9999.0), "↓ 999+");
    }
}
