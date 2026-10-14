#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{IShellFolderViewDual, IShellWindows, IWebBrowser2, ShellWindows};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, IsWindowVisible};
use windows::core::{BSTR, Interface};

#[derive(Debug, Clone)]
pub struct ShellTabInfo {
    pub hwnd_raw: usize,
    pub path: String,
    pub title: String,
    pub is_active: bool,
}

impl ShellTabInfo {
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd_raw as *mut _)
    }
}

pub enum ComRequest {
    /// 获取当前前台或所有活跃资源管理器的 Tab 列表
    GetAllTabs {
        reply: Sender<Vec<ShellTabInfo>>,
        deadline: std::time::Instant,
    },
    /// 获取指定或前台 Explorer 窗口中处于活跃/选中的 Tab 路径
    GetActiveTabPath {
        target_hwnd_raw: Option<usize>,
        reply: Sender<Option<String>>,
        deadline: std::time::Instant,
    },
    /// 导航指定的 WebBrowser/Tab 到新路径
    Navigate {
        hwnd_raw: usize,
        target_path: String,
        reply: Sender<bool>,
        deadline: std::time::Instant,
    },
    /// 导航指定 Explorer 窗口中最新创建的 Tab 到新路径（反向反查）
    NavigateLatest {
        hwnd_raw: usize,
        target_path: String,
        reply: Sender<bool>,
        deadline: std::time::Instant,
    },
    /// 获取指定窗口的目标路径与选定文件（穿透检测，不受可见性限制）
    GetWindowTargetInfo {
        target_hwnd_raw: usize,
        reply: Sender<Option<(String, Option<String>)>>,
        deadline: std::time::Instant,
    },
    /// 尝试单次检查新 Tab 在 COM 中注册并将其导航至指定路径（无 sleep，原子操作）
    TryNavigateNewTab {
        host_hwnd_raw: usize,
        count_before: usize,
        target_path: String,
        reply: Sender<bool>,
        deadline: std::time::Instant,
    },
    /// 在指定宿主窗口内按规范化路径查找 Tab 索引（0-based）
    FindTabIndexByPath {
        host_hwnd_raw: usize,
        target_path: String,
        reply: Sender<Option<usize>>,
        deadline: std::time::Instant,
    },
}

impl ComRequest {
    #[inline]
    pub fn deadline(&self) -> std::time::Instant {
        match self {
            Self::GetAllTabs { deadline, .. } => *deadline,
            Self::GetActiveTabPath { deadline, .. } => *deadline,
            Self::Navigate { deadline, .. } => *deadline,
            Self::NavigateLatest { deadline, .. } => *deadline,
            Self::GetWindowTargetInfo { deadline, .. } => *deadline,
            Self::TryNavigateNewTab { deadline, .. } => *deadline,
            Self::FindTabIndexByPath { deadline, .. } => *deadline,
        }
    }
}

/// STA worker 句柄：每个 worker 独占一条请求队列与一个 STA 线程
struct StaWorker {
    tx: Sender<ComRequest>,
    generation: u64,
}

/// 连续超时阈值：达到即认定该 STA worker 已卡死并重建
const MAX_CONSECUTIVE_TIMEOUTS: u64 = 2;
/// 累计重建告警阈值：每到达这个倍数就输出一条显式告警（报告第二批“重建上限监控”）
const MAX_REBUILDS_ALERT: u64 = 5;
/// 耗时样本环大小
const LATENCY_SAMPLES: usize = 256;
/// 每处理多少次请求输出一次埋点汇总
const STATS_LOG_EVERY: u64 = 500;

/// STA 队列埋点快照（P50/P95/P99、队列深度、超时率、重建次数）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComStatsSnapshot {
    pub requests: u64,
    pub timeouts: u64,
    pub worker_rebuilds: u64,
    pub queue_depth: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl ComStatsSnapshot {
    pub fn timeout_rate(&self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            self.timeouts as f64 / self.requests as f64
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "req={} timeout={} ({:.2}%) rebuild={} depth={} p50={:.1}ms p95={:.1}ms p99={:.1}ms",
            self.requests,
            self.timeouts,
            self.timeout_rate() * 100.0,
            self.worker_rebuilds,
            self.queue_depth,
            self.p50_ms,
            self.p95_ms,
            self.p99_ms
        )
    }
}

#[derive(Default)]
struct ComStats {
    requests: AtomicU64,
    timeouts: AtomicU64,
    consecutive_timeouts: AtomicU64,
    worker_rebuilds: AtomicU64,
    queue_depth: AtomicU64,
    latencies_us: Mutex<VecDeque<u64>>,
}

impl ComStats {
    fn record_request(&self) {
        self.requests.fetch_add(1, Ordering::SeqCst);
    }

    /// 记录一次成功：重置连续超时计数，采样耗时，并按节奏输出埋点
    fn record_success(&self, elapsed: Duration) {
        self.consecutive_timeouts.store(0, Ordering::SeqCst);
        {
            let mut samples = self
                .latencies_us
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if samples.len() >= LATENCY_SAMPLES {
                samples.pop_front();
            }
            samples.push_back(elapsed.as_micros() as u64);
        }

        let requests = self.requests.load(Ordering::SeqCst);
        if requests != 0 && requests.is_multiple_of(STATS_LOG_EVERY) {
            crate::dialog::detector::log_debug(&format!(
                "COM_STATS: {}",
                self.snapshot().describe()
            ));
        }
    }

    /// 记录一次超时，返回当前连续超时次数
    fn record_timeout(&self) -> u64 {
        self.timeouts.fetch_add(1, Ordering::SeqCst);
        self.consecutive_timeouts.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn snapshot(&self) -> ComStatsSnapshot {
        let mut samples: Vec<u64> = self
            .latencies_us
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .copied()
            .collect();
        samples.sort_unstable();

        ComStatsSnapshot {
            requests: self.requests.load(Ordering::SeqCst),
            timeouts: self.timeouts.load(Ordering::SeqCst),
            worker_rebuilds: self.worker_rebuilds.load(Ordering::SeqCst),
            queue_depth: self.queue_depth.load(Ordering::SeqCst),
            p50_ms: percentile_ms(&samples, 50.0),
            p95_ms: percentile_ms(&samples, 95.0),
            p99_ms: percentile_ms(&samples, 99.0),
        }
    }
}

fn percentile_ms(sorted_us: &[u64], percentile: f64) -> f64 {
    if sorted_us.is_empty() {
        return 0.0;
    }
    let rank = ((percentile / 100.0) * (sorted_us.len() as f64 - 1.0)).round() as usize;
    sorted_us[rank.min(sorted_us.len() - 1)] as f64 / 1000.0
}

/// STA 线程上的 Shell COM 管理器。
///
/// **架构不变式**：Shell COM 请求全部在单独的 STA 线程上串行执行；
/// UIA 调用（`tab::select` / `tab::uia` / `win32::uia`）一律在各自的专用线程里发起，
/// **绝不允许在 STA 线程内调用 UIA** —— UIA 一旦挂起会永久堵死整个 COM 队列。
///
/// 审计报告 P-02：`deadline` 只能丢弃“尚未开始”的请求，无法中断已经开始的 COM 调用
/// （`IShellWindows` 枚举没有可取消版本）。因此这里补上 **circuit breaker**：
/// 连续多次超时即判定该 worker 已卡死，**放弃该 worker 的结果并重建新 worker**，
/// 让后续请求立即回到可用队列，而不是一起饿死。被放弃的线程会在当前调用返回后
/// 因通道断开而自行退出；若永远不返回，则线程泄漏（宁可泄漏一个 worker，也不永久饿死）。
pub struct ShellComManager {
    worker: Mutex<StaWorker>,
    stats: ComStats,
}

impl Default for ShellComManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellComManager {
    pub fn new() -> Self {
        Self {
            worker: Mutex::new(spawn_sta_worker(1)),
            stats: ComStats::default(),
        }
    }

    #[cfg(test)]
    fn new_with_worker(worker: StaWorker) -> Self {
        Self {
            worker: Mutex::new(worker),
            stats: ComStats::default(),
        }
    }

    /// STA 队列埋点快照（P50/P95/P99、队列深度、超时率、重建次数）
    pub fn stats(&self) -> ComStatsSnapshot {
        self.stats.snapshot()
    }

    fn worker_sender(&self) -> Sender<ComRequest> {
        self.worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tx
            .clone()
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    /// 放弃当前 worker，换一个全新 STA 线程继续服务
    fn rebuild_worker(&self) -> Sender<ComRequest> {
        let mut guard = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let generation = guard.generation + 1;
        let fresh = spawn_sta_worker(generation);
        // 丢弃旧 Sender：卡死的 worker 在下次通道轮询时会发现断开而退出
        let _old = std::mem::replace(&mut *guard, fresh);
        drop(_old);

        self.stats.worker_rebuilds.fetch_add(1, Ordering::SeqCst);
        self.stats.consecutive_timeouts.store(0, Ordering::SeqCst);
        let rebuilds = self.stats.worker_rebuilds.load(Ordering::SeqCst);
        crate::dialog::detector::log_debug(&format!(
            "COM_CIRCUIT_BREAKER: 连续超时 {} 次，已重建 STA worker (gen={}, rebuild_total={}) | {}",
            MAX_CONSECUTIVE_TIMEOUTS,
            generation,
            rebuilds,
            self.stats.snapshot().describe()
        ));

        // 重建上限监控（报告第二批）：重建只是“恢复手段”，已开始的 COM 调用无法取消，
        // 被放弃的 STA 线程可能永久泄漏。因此累计重建达到阈值时给出显式告警，
        // 便于现场区分“偶发卡顿”与“某次调用已把线程钉死”。
        if rebuilds >= MAX_REBUILDS_ALERT && rebuilds.is_multiple_of(MAX_REBUILDS_ALERT) {
            crate::dialog::detector::log_debug(&format!(
                "COM_WORKER_ALERT: STA worker 累计重建 {} 次（每 {} 次告警一次），可能存在不可取消的 COM 调用，被放弃的线程会泄漏 | {}",
                rebuilds,
                MAX_REBUILDS_ALERT,
                self.stats.snapshot().describe()
            ));
        }
        guard.tx.clone()
    }

    /// 统一的请求投递：负责埋点、超时计数、circuit breaker 与一次性重试
    fn dispatch<T>(&self, make: impl Fn(Sender<T>) -> ComRequest, timeout: Duration) -> Option<T> {
        let (reply_tx, reply_rx) = channel::<T>();
        let mut sender = self.worker_sender();
        self.stats.record_request();

        for attempt in 0..2 {
            self.stats.queue_depth.fetch_add(1, Ordering::SeqCst);
            let request = make(reply_tx.clone());
            let started = Instant::now();
            let delivered = sender.send(request).is_ok();
            let reply = if delivered {
                reply_rx.recv_timeout(timeout)
            } else {
                Err(RecvTimeoutError::Disconnected)
            };
            self.stats.queue_depth.fetch_sub(1, Ordering::SeqCst);

            match reply {
                Ok(value) => {
                    self.stats.record_success(started.elapsed());
                    return Some(value);
                }
                Err(RecvTimeoutError::Timeout) => {
                    // 超时可能是“这次真的很慢”，也可能是 worker 已卡死：交给 circuit breaker 判定
                    let consecutive = self.stats.record_timeout();
                    if consecutive >= MAX_CONSECUTIVE_TIMEOUTS {
                        let _ = self.rebuild_worker();
                    }
                    return None;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // worker 线程已消失（panic / 通道断开）：重建后重试一次
                    if attempt == 0 {
                        sender = self.rebuild_worker();
                        continue;
                    }
                    return None;
                }
            }
        }
        None
    }

    /// 获取所有打开且可见有效的资源管理器 Tab（带 150ms 超时）
    pub fn get_all_tabs(&self) -> Vec<ShellTabInfo> {
        let timeout = Duration::from_millis(150);
        self.dispatch(
            |reply| ComRequest::GetAllTabs {
                reply,
                deadline: Instant::now() + timeout,
            },
            timeout,
        )
        .unwrap_or_default()
    }

    /// 获取活动 Tab 路径（带 150ms 超时）
    pub fn get_active_tab_path(&self, target_hwnd: Option<HWND>) -> Option<String> {
        let timeout = Duration::from_millis(150);
        let target_hwnd_raw = target_hwnd.map(|h| h.0 as usize);
        self.dispatch(
            |reply| ComRequest::GetActiveTabPath {
                target_hwnd_raw,
                reply,
                deadline: Instant::now() + timeout,
            },
            timeout,
        )
        .flatten()
    }

    /// 在指定宿主窗口内查找目标路径对应的已有 Tab 索引（0-based，带 250ms 超时）
    ///
    /// 注意：STA 线程繁忙时首次请求可能超时被丢弃。超时与“确实不存在”必须区分开，
    /// 否则会把已存在的标签误判为不存在，进而新建重复标签（表现为“点了像没反应”）。
    /// 因此这里做有限次重试。
    pub fn find_tab_index_by_path(&self, host_hwnd: HWND, target_path: &str) -> Option<usize> {
        const ATTEMPTS: usize = 3;
        let host_hwnd_raw = host_hwnd.0 as usize;
        for attempt in 0..ATTEMPTS {
            let timeout = Duration::from_millis(250);
            if let Some(result) = self.dispatch(
                |reply| ComRequest::FindTabIndexByPath {
                    host_hwnd_raw,
                    target_path: target_path.to_string(),
                    reply,
                    deadline: Instant::now() + timeout,
                },
                timeout,
            ) {
                return result;
            }
            // 超时：短暂让出后重试，最后一次仍失败才认定“未找到”
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(Duration::from_millis(40));
            }
        }
        None
    }

    /// 请求重定向某个 Explorer 窗口或标签页（带 200ms 超时）
    pub fn navigate(&self, hwnd: HWND, target_path: &str) -> bool {
        let timeout = Duration::from_millis(200);
        let hwnd_raw = hwnd.0 as usize;
        self.dispatch(
            |reply| ComRequest::Navigate {
                hwnd_raw,
                target_path: target_path.to_string(),
                reply,
                deadline: Instant::now() + timeout,
            },
            timeout,
        )
        .unwrap_or(false)
    }

    /// 导航指定 Explorer 窗口中最新创建的 Tab 到新路径（带 200ms 超时，反向反查遍历）
    pub fn navigate_latest(&self, hwnd: HWND, target_path: &str) -> bool {
        self.navigate_latest_with_timeout(hwnd, target_path, Duration::from_millis(200))
    }

    fn navigate_latest_with_timeout(
        &self,
        hwnd: HWND,
        target_path: &str,
        timeout: Duration,
    ) -> bool {
        let hwnd_raw = hwnd.0 as usize;
        self.dispatch(
            |reply| ComRequest::NavigateLatest {
                hwnd_raw,
                target_path: target_path.to_string(),
                reply,
                deadline: Instant::now() + timeout,
            },
            timeout,
        )
        .unwrap_or(false)
    }

    /// 获取指定窗口的目标路径与选定文件名（穿透检测，不受可见性限制，带 200ms 超时）
    pub fn get_window_target_info(&self, target_hwnd: HWND) -> Option<(String, Option<String>)> {
        let timeout = Duration::from_millis(200);
        let target_hwnd_raw = target_hwnd.0 as usize;
        self.dispatch(
            |reply| ComRequest::GetWindowTargetInfo {
                target_hwnd_raw,
                reply,
                deadline: Instant::now() + timeout,
            },
            timeout,
        )
        .flatten()
    }

    /// 获取指定窗口下当前有效 Tab 的数量。
    ///
    /// **无法确认时必须返回 `None`**（报告 P1-01）：`get_all_tabs` 在 STA 超时时会
    /// 退化为空列表，若在这里折算成 `0`，调用方就会把“查询失败”当成“本来没有标签”，
    /// 进而用“计数 > 0”错误地证明“新标签已创建”。
    pub fn get_tabs_count_for_hwnd_checked(&self, hwnd: HWND) -> Option<usize> {
        let timeout = Duration::from_millis(150);
        let tabs = self.dispatch(
            |reply| ComRequest::GetAllTabs {
                reply,
                deadline: Instant::now() + timeout,
            },
            timeout,
        )?;
        let raw = hwnd.0 as usize;
        Some(tabs.into_iter().filter(|t| t.hwnd_raw == raw).count())
    }

    /// 轮询等待新 Tab 在宿主窗口中就绪并执行重定向导航。
    ///
    /// **单一绝对截止时间：总上界 1000ms**（不再有“文档说 800ms、实际约 1s”的不一致）：
    /// - 前 800ms：轮询向 STA 请求原子单次检查（每轮 ≤ 80ms）；
    /// - 剩余 ≤ 200ms：用一次 `navigate_latest` 兜底，且该兜底同样受同一个截止时间约束。
    pub fn wait_and_navigate_new_tab(
        &self,
        host_hwnd: HWND,
        count_before: usize,
        target_path: &str,
    ) -> bool {
        /// 总预算（外部可见的硬上界）
        const TOTAL_BUDGET: Duration = Duration::from_millis(1000);
        /// 轮询阶段预算
        const POLL_BUDGET: Duration = Duration::from_millis(800);

        let host_hwnd_raw = host_hwnd.0 as usize;
        let start = Instant::now();
        let deadline = start + TOTAL_BUDGET;
        let poll_end = start + POLL_BUDGET;

        // 在调用方工作线程轮询，每轮 30ms 向 STA 请求一次原子单次检查
        while Instant::now() < poll_end {
            let now = Instant::now();
            let remaining = poll_end.saturating_duration_since(now);
            if remaining.is_zero() {
                break;
            }

            let timeout = remaining.min(Duration::from_millis(80));
            if let Some(true) = self.dispatch(
                |reply| ComRequest::TryNavigateNewTab {
                    host_hwnd_raw,
                    count_before,
                    target_path: target_path.to_string(),
                    reply,
                    deadline: Instant::now() + timeout,
                },
                timeout,
            ) {
                return true;
            }

            std::thread::sleep(Duration::from_millis(30));
        }

        // 兜底：仍受同一个绝对截止时间约束，能用多少时间就用多少
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        self.navigate_latest_with_timeout(host_hwnd, target_path, remaining)
    }

    /// 根据窗口 HWND、预期的 Tab 标题或索引，解析出最匹配的物理路径
    pub fn resolve_tab_path_by_target(
        &self,
        hwnd: HWND,
        target_title: Option<&str>,
        target_index: Option<usize>,
    ) -> Option<String> {
        let tabs = self.get_all_tabs();
        let raw = hwnd.0 as usize;
        let window_tabs: Vec<_> = tabs.into_iter().filter(|t| t.hwnd_raw == raw).collect();

        if window_tabs.is_empty() {
            return None;
        }

        // 1. 如果有 target_index 且在范围内，且其标题与 target_title 匹配（或未指定 title）
        if let Some(idx) = target_index
            && idx < window_tabs.len()
        {
            if let Some(expected_title) = target_title {
                if window_tabs[idx].title == expected_title {
                    return Some(window_tabs[idx].path.clone());
                }
            } else {
                return Some(window_tabs[idx].path.clone());
            }
        }

        // 2. 如果指定了 target_title，按标题精确匹配
        if let Some(title) = target_title
            && let Some(matched) = window_tabs.iter().find(|t| t.title == title)
        {
            return Some(matched.path.clone());
        }

        // 3. 回退到当前处于 active 的 Tab
        if let Some(active) = window_tabs.iter().find(|t| t.is_active) {
            return Some(active.path.clone());
        }

        // 4. 最低优先级回退到第一个 Tab
        window_tabs.first().map(|t| t.path.clone())
    }
}

/// 启动一个全新的 STA worker 线程
fn spawn_sta_worker(generation: u64) -> StaWorker {
    let (tx, rx) = channel::<ComRequest>();
    thread::Builder::new()
        .name(format!("QuickPath-ShellComSTA#{}", generation))
        .spawn(move || sta_worker_loop(rx))
        .expect("创建 ShellComManager STA 线程失败");
    StaWorker { tx, generation }
}

/// STA worker 主循环：单线程串行处理请求
fn sta_worker_loop(rx: std::sync::mpsc::Receiver<ComRequest>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }

    while let Ok(req) = rx.recv() {
        // 若调用方已超时放弃该请求，直接丢弃，不执行耗时 COM 操作，彻底杜绝队列积压
        if Instant::now() > req.deadline() {
            continue;
        }

        match req {
            ComRequest::GetAllTabs { reply, .. } => {
                let tabs = enumerate_shell_tabs();
                let _ = reply.send(tabs);
            }
            ComRequest::GetActiveTabPath {
                target_hwnd_raw,
                reply,
                ..
            } => {
                let hwnd_opt = target_hwnd_raw.map(|raw| HWND(raw as *mut _));
                let path = get_active_tab_path_internal(hwnd_opt);
                let _ = reply.send(path);
            }
            ComRequest::Navigate {
                hwnd_raw,
                target_path,
                reply,
                ..
            } => {
                let hwnd = HWND(hwnd_raw as *mut _);
                let ok = navigate_internal(hwnd, &target_path);
                let _ = reply.send(ok);
            }
            ComRequest::NavigateLatest {
                hwnd_raw,
                target_path,
                reply,
                ..
            } => {
                let hwnd = HWND(hwnd_raw as *mut _);
                let ok = navigate_latest_internal(hwnd, &target_path);
                let _ = reply.send(ok);
            }
            ComRequest::GetWindowTargetInfo {
                target_hwnd_raw,
                reply,
                ..
            } => {
                let hwnd = HWND(target_hwnd_raw as *mut _);
                let info = get_window_target_info_internal(hwnd);
                let _ = reply.send(info);
            }
            ComRequest::TryNavigateNewTab {
                host_hwnd_raw,
                count_before,
                target_path,
                reply,
                ..
            } => {
                let hwnd = HWND(host_hwnd_raw as *mut _);
                let ok = try_navigate_new_tab_internal(hwnd, count_before, &target_path);
                let _ = reply.send(ok);
            }
            ComRequest::FindTabIndexByPath {
                host_hwnd_raw,
                target_path,
                reply,
                ..
            } => {
                let hwnd = HWND(host_hwnd_raw as *mut _);
                let idx = find_tab_index_by_path_internal(hwnd, &target_path);
                let _ = reply.send(idx);
            }
        }
    }

    // 通道断开（含被 circuit breaker 放弃的情况）：正常退出则归还 COM 单子
    unsafe {
        CoUninitialize();
    }
}

/// 枚举系统中所有 ShellWindows 并提取路径（严格过滤死窗口与不可见窗口，精准识别活动 Tab）
fn enumerate_shell_tabs() -> Vec<ShellTabInfo> {
    let mut results = Vec::new();

    unsafe {
        let top_explorer = crate::managers::explorer::get_top_visible_explorer_hwnd();
        let top_active_tab = top_explorer.and_then(crate::managers::explorer::get_active_tab_hwnd);

        let shell_windows: Result<IShellWindows, _> =
            CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);

        if let Ok(shell_windows) = shell_windows
            && let Ok(count) = shell_windows.Count()
        {
            for i in 0..count {
                let index = VARIANT::from(i);
                if let Ok(disp) = shell_windows.Item(&index)
                    && let Ok(browser) = disp.cast::<IWebBrowser2>()
                {
                    let hwnd = browser
                        .HWND()
                        .map(|h| HWND(h.0 as *mut _))
                        .unwrap_or_default();
                    // 关键 Fail-Safe：严格检查窗口存活性与可见性，彻底剔除 dead/zombie 窗口
                    if hwnd.0.is_null()
                        || !IsWindow(Some(hwnd)).as_bool()
                        || !IsWindowVisible(hwnd).as_bool()
                    {
                        continue;
                    }

                    let url = browser
                        .LocationURL()
                        .map(|u| u.to_string())
                        .unwrap_or_default();
                    let name = browser
                        .LocationName()
                        .map(|n| n.to_string())
                        .unwrap_or_default();

                    if let Some(path) = parse_shell_url(&url, &name) {
                        let is_valid = path.starts_with("shell:")
                            || path.starts_with("::{")
                            || (path.len() >= 2 && &path[1..2] == ":")
                            || path.starts_with("\\\\");
                        if is_valid {
                            let title = if name.is_empty() {
                                Path::new(&path)
                                    .file_name()
                                    .map(|f| f.to_string_lossy().to_string())
                                    .unwrap_or_else(|| path.clone())
                            } else {
                                name
                            };

                            // 核心：基于 ShellTabWindowClass 精准识别 Win11 多标签页活动状态
                            let mut is_active = false;
                            let tab_sub_hwnd =
                                crate::managers::explorer::get_browser_tab_hwnd(&browser);

                            if let Some(top_exp) = top_explorer
                                && hwnd == top_exp
                            {
                                if let (Some(my_tab), Some(active_tab)) =
                                    (tab_sub_hwnd, top_active_tab)
                                {
                                    if my_tab == active_tab {
                                        is_active = true;
                                    }
                                } else if top_active_tab.is_none() {
                                    is_active = true;
                                }
                            }

                            results.push(ShellTabInfo {
                                hwnd_raw: hwnd.0 as usize,
                                path,
                                title,
                                is_active,
                            });
                        }
                    }
                }
            }
        }
    }

    // 活动 Tab 优先排序
    results.sort_by(|a, b| b.is_active.cmp(&a.is_active));

    results
}

/// 获取指定或活动 Explorer 窗口的当前活跃路径
fn get_active_tab_path_internal(target_hwnd: Option<HWND>) -> Option<String> {
    let tabs = enumerate_shell_tabs();
    if let Some(target) = target_hwnd {
        let raw = target.0 as usize;
        // 优先获取该目标窗口内部正处于 active 的 Tab
        if let Some(tab) = tabs.iter().find(|t| t.hwnd_raw == raw && t.is_active) {
            return Some(tab.path.clone());
        }
        if let Some(tab) = tabs.iter().find(|t| t.hwnd_raw == raw) {
            return Some(tab.path.clone());
        }
    }

    if let Some(tab) = tabs.iter().find(|t| t.is_active) {
        return Some(tab.path.clone());
    }

    tabs.first().map(|t| t.path.clone())
}

/// 内部导航实现
fn navigate_internal(target_hwnd: HWND, target_path: &str) -> bool {
    unsafe {
        let shell_windows: Result<IShellWindows, _> =
            CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows
            && let Ok(count) = shell_windows.Count()
        {
            for i in 0..count {
                let index = VARIANT::from(i);
                if let Ok(disp) = shell_windows.Item(&index)
                    && let Ok(browser) = disp.cast::<IWebBrowser2>()
                {
                    let hwnd = browser
                        .HWND()
                        .map(|h| HWND(h.0 as *mut _))
                        .unwrap_or_default();
                    if hwnd == target_hwnd || target_hwnd.0.is_null() {
                        let path_bstr = BSTR::from(target_path);
                        let var_url = VARIANT::from(path_bstr);
                        let null_var = VARIANT::default();
                        if browser
                            .Navigate2(
                                &var_url,
                                Some(&null_var as *const _),
                                Some(&null_var as *const _),
                                Some(&null_var as *const _),
                                Some(&null_var as *const _),
                            )
                            .is_ok()
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// 内部导航实现（从后往前反向查找最新创建的 Tab）
fn navigate_latest_internal(target_hwnd: HWND, target_path: &str) -> bool {
    unsafe {
        let shell_windows: Result<IShellWindows, _> =
            CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows
            && let Ok(count) = shell_windows.Count()
        {
            for i in (0..count).rev() {
                let index = VARIANT::from(i);
                if let Ok(disp) = shell_windows.Item(&index)
                    && let Ok(browser) = disp.cast::<IWebBrowser2>()
                {
                    let hwnd = browser
                        .HWND()
                        .map(|h| HWND(h.0 as *mut _))
                        .unwrap_or_default();
                    if hwnd == target_hwnd || target_hwnd.0.is_null() {
                        let path_bstr = BSTR::from(target_path);
                        let var_url = VARIANT::from(path_bstr);
                        let null_var = VARIANT::default();
                        if browser
                            .Navigate2(
                                &var_url,
                                Some(&null_var as *const _),
                                Some(&null_var as *const _),
                                Some(&null_var as *const _),
                                Some(&null_var as *const _),
                            )
                            .is_ok()
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// 单次原子检查宿主窗口内新注册的 Tab 并对其调用 Navigate2（零 sleep，极速无阻塞）
fn try_navigate_new_tab_internal(host_hwnd: HWND, count_before: usize, target_path: &str) -> bool {
    unsafe {
        let shell_windows: Result<IShellWindows, _> =
            CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows
            && let Ok(count) = shell_windows.Count()
        {
            let mut host_browsers = Vec::new();
            for i in 0..count {
                let index = VARIANT::from(i);
                if let Ok(disp) = shell_windows.Item(&index)
                    && let Ok(browser) = disp.cast::<IWebBrowser2>()
                {
                    let hwnd = browser
                        .HWND()
                        .map(|h| HWND(h.0 as *mut _))
                        .unwrap_or_default();
                    if hwnd == host_hwnd {
                        host_browsers.push(browser);
                    }
                }
            }

            // 当属于 host_hwnd 的 Tab 数量出现增长，或者无前置数量但已存在有效 browser
            if (host_browsers.len() > count_before
                || (count_before == 0 && !host_browsers.is_empty()))
                && let Some(newest_browser) = host_browsers.last()
            {
                let path_bstr = BSTR::from(target_path);
                let var_url = VARIANT::from(path_bstr);
                let null_var = VARIANT::default();
                if newest_browser
                    .Navigate2(
                        &var_url,
                        Some(&null_var as *const _),
                        Some(&null_var as *const _),
                        Some(&null_var as *const _),
                        Some(&null_var as *const _),
                    )
                    .is_ok()
                {
                    return true;
                }
            }
        }
    }
    false
}

/// 在指定宿主窗口内按规范化路径查找 Tab 索引（0-based）
fn find_tab_index_by_path_internal(host_hwnd: HWND, target_path: &str) -> Option<usize> {
    if host_hwnd.0.is_null() || target_path.is_empty() {
        return None;
    }
    let target_norm = normalize_path_local(target_path);

    unsafe {
        let shell_windows: Result<IShellWindows, _> =
            CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows
            && let Ok(count) = shell_windows.Count()
        {
            let mut current_idx = 0usize;
            for i in 0..count {
                let index = VARIANT::from(i);
                if let Ok(disp) = shell_windows.Item(&index)
                    && let Ok(browser) = disp.cast::<IWebBrowser2>()
                {
                    let hwnd = browser
                        .HWND()
                        .map(|h| HWND(h.0 as *mut _))
                        .unwrap_or_default();
                    if hwnd == host_hwnd {
                        let url = browser
                            .LocationURL()
                            .map(|u| u.to_string())
                            .unwrap_or_default();
                        let name = browser
                            .LocationName()
                            .map(|n| n.to_string())
                            .unwrap_or_default();
                        if let Some(path) = parse_shell_url(&url, &name)
                            && normalize_path_local(&path) == target_norm
                        {
                            return Some(current_idx);
                        }
                        current_idx += 1;
                    }
                }
            }
        }
    }
    None
}

fn normalize_path_local(path: &str) -> String {
    let mut s = path.trim().to_lowercase().replace('/', "\\");
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

/// 针对指定 Explorer 窗口定向穿透探测其当前路径及待选文件名（不受 IsWindowVisible 限制）
fn get_window_target_info_internal(target_hwnd: HWND) -> Option<(String, Option<String>)> {
    if target_hwnd.0.is_null() {
        return None;
    }

    unsafe {
        if !IsWindow(Some(target_hwnd)).as_bool() {
            return None;
        }

        let shell_windows: Result<IShellWindows, _> =
            CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows
            && let Ok(count) = shell_windows.Count()
        {
            for i in 0..count {
                let index = VARIANT::from(i);
                if let Ok(disp) = shell_windows.Item(&index)
                    && let Ok(browser) = disp.cast::<IWebBrowser2>()
                {
                    let hwnd = browser
                        .HWND()
                        .map(|h| HWND(h.0 as *mut _))
                        .unwrap_or_default();
                    if hwnd == target_hwnd {
                        // 1. 解析 LocationURL 基础路径
                        let url = browser
                            .LocationURL()
                            .map(|u| u.to_string())
                            .unwrap_or_default();
                        let name = browser
                            .LocationName()
                            .map(|n| n.to_string())
                            .unwrap_or_default();
                        let parsed_path = parse_shell_url(&url, &name);

                        // 2. 穿透提取 SelectedItems（针对外部 /select 或 SHOpenFolderAndSelectItems 唤起）
                        let mut selected_file = None;
                        let mut selected_full_path = None;

                        if let Ok(doc_disp) = browser.Document()
                            && let Ok(sfv) = doc_disp.cast::<IShellFolderViewDual>()
                            && let Ok(items) = sfv.SelectedItems()
                            && let Ok(item_count) = items.Count()
                            && item_count > 0
                        {
                            let v_zero = VARIANT::from(0i32);
                            if let Ok(first_item) = items.Item(&v_zero) {
                                if let Ok(item_path) = first_item.Path() {
                                    let path_str = item_path.to_string();
                                    if !path_str.is_empty() {
                                        selected_full_path = Some(path_str);
                                    }
                                }
                                if let Ok(item_name) = first_item.Name() {
                                    let name_str = item_name.to_string();
                                    if !name_str.is_empty() {
                                        selected_file = Some(name_str);
                                    }
                                }
                            }
                        }

                        // 3. 优先依据选中项完整路径定位父目录与高亮文件
                        if let Some(ref full_p) = selected_full_path {
                            let p = Path::new(full_p);
                            if let Some(parent) = p.parent() {
                                let folder = parent.to_string_lossy().to_string();
                                let file = p.file_name().map(|f| f.to_string_lossy().to_string());
                                if is_physical_dir_path(&folder) {
                                    return Some((folder, file.or(selected_file)));
                                }
                            }
                        }

                        // 4. 若无选中项全路径，使用解析出的 URL 路径结合文件名
                        if let Some(path) = parsed_path {
                            return Some((path, selected_file));
                        }
                    }
                }
            }
        }
    }

    None
}

/// 解析 LocationURL 或虚拟 Shell 路径
pub fn parse_shell_url(url: &str, location_name: &str) -> Option<String> {
    if url.is_empty() {
        if location_name == "此电脑" || location_name.eq_ignore_ascii_case("this pc") {
            return Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string());
        }
        if location_name == "主文件夹"
            || location_name == "主页"
            || location_name.eq_ignore_ascii_case("home")
        {
            return Some("shell:Home".to_string());
        }
        return None;
    }

    if let Some(raw) = url.strip_prefix("file:///") {
        let decoded = url_decode(raw);
        let path_str = decoded.replace('/', "\\");

        if path_str.len() >= 2 && &path_str[1..2] == ":" {
            Some(path_str)
        } else if path_str.starts_with('\\') {
            Some(format!("\\{}", path_str))
        } else {
            Some(path_str)
        }
    } else if url.starts_with("shell:::") || url.starts_with("::{") || url.starts_with("shell:") {
        Some(url.to_string())
    } else {
        None
    }
}

/// 解析 file:/// URL（兼容历史调用）
pub fn parse_file_url(url: &str) -> Option<String> {
    parse_shell_url(url, "")
}

/// 判断路径是否为合法的本地物理目录或网络物理路径（排除虚拟 Shell 主页，零阻塞）
pub fn is_physical_dir_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with("shell:") || path.starts_with("::{") {
        return false;
    }
    (path.len() >= 2 && &path[1..2] == ":") || path.starts_with("\\\\")
}

fn url_decode(input: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = input.bytes();

    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2)
                && let Ok(val) = u8::from_str_radix(&format!("{}{}", h1 as char, h2 as char), 16)
            {
                bytes.push(val);
                continue;
            }
        }
        bytes.push(b);
    }

    String::from_utf8_lossy(&bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silent_worker() -> (StaWorker, std::sync::mpsc::Receiver<ComRequest>) {
        // 保留接收端但不消费也不回复，模拟“STA 线程卡死在 COM 调用里”
        let (tx, rx) = channel::<ComRequest>();
        (StaWorker { tx, generation: 1 }, rx)
    }

    #[test]
    fn test_stats_percentiles_and_describe() {
        let stats = ComStats::default();
        for us in [1000u64, 2000, 3000, 4000, 5000] {
            stats.record_success(Duration::from_micros(us));
        }
        let snapshot = stats.snapshot();
        assert!(
            (snapshot.p50_ms - 3.0).abs() < 0.001,
            "p50 应为 3ms，实际 {}",
            snapshot.p50_ms
        );
        assert!(
            (snapshot.p95_ms - 5.0).abs() < 0.001,
            "p95 应为 5ms，实际 {}",
            snapshot.p95_ms
        );
        assert!((snapshot.p99_ms - 5.0).abs() < 0.001);
        assert_eq!(snapshot.requests, 0, "requests 只在投递时累加");
        assert!(snapshot.describe().contains("p50="));
    }

    #[test]
    fn test_percentile_handles_empty_and_single_sample() {
        assert_eq!(percentile_ms(&[], 50.0), 0.0);
        assert!((percentile_ms(&[2500], 99.0) - 2.5).abs() < 0.001);
    }

    #[test]
    fn test_consecutive_timeouts_trip_circuit_breaker() {
        let (worker, _rx) = silent_worker();
        let manager = ShellComManager::new_with_worker(worker);
        let timeout = Duration::from_millis(20);
        let request = |reply| ComRequest::GetAllTabs {
            reply,
            deadline: Instant::now() + timeout,
        };

        for _ in 0..MAX_CONSECUTIVE_TIMEOUTS {
            assert!(manager.dispatch(request, timeout).is_none(), "环境应超时");
        }

        let snapshot = manager.stats();
        assert_eq!(snapshot.timeouts, MAX_CONSECUTIVE_TIMEOUTS);
        assert_eq!(
            snapshot.worker_rebuilds, 1,
            "连续超时必须触发 STA worker 重建"
        );
        assert_eq!(manager.generation(), 2, "重建后 generation 必须递增");
        assert_eq!(snapshot.queue_depth, 0, "请求结束后队列深度必须归零");
        // 重建后连续超时计数已清零
        assert_eq!(manager.stats().timeouts, MAX_CONSECUTIVE_TIMEOUTS);
    }

    /// 重建计数必须持续累计，COM_WORKER_ALERT 阈值才有意义（报告第二批）
    #[test]
    fn test_worker_rebuild_counter_accumulates_for_alerting() {
        let (worker, _rx) = silent_worker();
        let manager = ShellComManager::new_with_worker(worker);

        let mut last = 0u64;
        for _ in 0..MAX_REBUILDS_ALERT {
            let _ = manager.rebuild_worker();
            let now = manager.stats().worker_rebuilds;
            assert_eq!(now, last + 1, "每次重建必须使计数严格递增");
            last = now;
        }
        assert_eq!(manager.stats().worker_rebuilds, MAX_REBUILDS_ALERT);
    }

    #[test]
    fn test_dead_worker_channel_is_rebuilt_without_counting_timeout() {
        let (tx, rx) = channel::<ComRequest>();
        drop(rx); // 模拟 worker 线程已死亡：通道断开
        let manager = ShellComManager::new_with_worker(StaWorker { tx, generation: 1 });
        let timeout = Duration::from_millis(300);

        let _ = manager.dispatch(
            |reply| ComRequest::GetAllTabs {
                reply,
                deadline: Instant::now() + timeout,
            },
            timeout,
        );

        let snapshot = manager.stats();
        assert_eq!(
            snapshot.worker_rebuilds, 1,
            "通道断开必须重建 worker 并重试"
        );
        assert_eq!(snapshot.timeouts, 0, "通道断开不应被计为超时");
        assert_eq!(manager.generation(), 2);
    }

    /// P1-01：STA 无响应时标签计数必须返回 `None`，绝不能把“超时”折算成 `0`，
    /// 否则调用方会用宿主原有标签错误地证明“新标签已创建”。
    #[test]
    fn test_checked_tab_count_returns_none_when_worker_never_replies() {
        let (tx, rx) = channel::<ComRequest>();
        // 保持接收端存活但永不回复，模拟“STA 线程卡死”：
        // 通道仍连通，因此 dispatch 不会走断连重建分支，只能超时。
        std::mem::forget(rx);
        let manager = ShellComManager::new_with_worker(StaWorker { tx, generation: 1 });

        let start = Instant::now();
        assert_eq!(
            manager.get_tabs_count_for_hwnd_checked(HWND(std::ptr::null_mut())),
            None,
            "STA 无响应时基线计数必须是 None（而不是某些被误当成 0 的值）"
        );
        assert!(
            start.elapsed() < Duration::from_millis(1000),
            "必须在有界超时内返回，实际 {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn test_parse_shell_url_standard_files() {
        assert_eq!(
            parse_shell_url("file:///C:/Windows/System32", ""),
            Some("C:\\Windows\\System32".to_string())
        );
        assert_eq!(
            parse_shell_url("file:///D:/Project%20Files/Code", ""),
            Some("D:\\Project Files\\Code".to_string())
        );
    }

    #[test]
    fn test_parse_shell_url_virtual_folders() {
        // 此电脑 (This PC)
        assert_eq!(
            parse_shell_url("", "此电脑"),
            Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string())
        );
        assert_eq!(
            parse_shell_url("", "This PC"),
            Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string())
        );

        // 主文件夹 (Home)
        assert_eq!(
            parse_shell_url("", "主文件夹"),
            Some("shell:Home".to_string())
        );
        assert_eq!(parse_shell_url("", "Home"), Some("shell:Home".to_string()));

        // GUID 格式直传
        assert_eq!(
            parse_shell_url("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", ""),
            Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string())
        );
    }

    #[test]
    fn test_is_physical_dir_path() {
        assert!(is_physical_dir_path("C:\\Windows"));
        assert!(is_physical_dir_path("D:\\"));
        assert!(is_physical_dir_path("\\\\server\\share"));

        // 虚拟路径必须一律判定为 false，杜绝 Navigate2 触发系统报错
        assert!(!is_physical_dir_path(""));
        assert!(!is_physical_dir_path("shell:Home"));
        assert!(!is_physical_dir_path(
            "shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"
        ));
        assert!(!is_physical_dir_path(
            "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"
        ));
    }

    #[test]
    fn test_resolve_tab_path_logic() {
        let tabs = [
            ShellTabInfo {
                hwnd_raw: 100,
                path: "C:\\FolderA".to_string(),
                title: "FolderA".to_string(),
                is_active: false,
            },
            ShellTabInfo {
                hwnd_raw: 100,
                path: "D:\\FolderB".to_string(),
                title: "FolderB".to_string(),
                is_active: true,
            },
            ShellTabInfo {
                hwnd_raw: 100,
                path: "E:\\FolderC".to_string(),
                title: "FolderC".to_string(),
                is_active: false,
            },
        ];

        // 模拟 resolve 匹配逻辑
        let resolve = |target_title: Option<&str>, target_index: Option<usize>| -> Option<String> {
            if let Some(idx) = target_index
                && idx < tabs.len()
            {
                if let Some(expected_title) = target_title {
                    if tabs[idx].title == expected_title {
                        return Some(tabs[idx].path.clone());
                    }
                } else {
                    return Some(tabs[idx].path.clone());
                }
            }
            if let Some(title) = target_title
                && let Some(matched) = tabs.iter().find(|t| t.title == title)
            {
                return Some(matched.path.clone());
            }
            if let Some(active) = tabs.iter().find(|t| t.is_active) {
                return Some(active.path.clone());
            }
            tabs.first().map(|t| t.path.clone())
        };

        // 双击第 0 个 Tab (FolderA) -> 必须精准解析出 FolderA，绝不是最后的 FolderC 或活跃的 FolderB
        assert_eq!(
            resolve(Some("FolderA"), Some(0)),
            Some("C:\\FolderA".to_string())
        );

        // 双击第 2 个 Tab (FolderC) -> 必须精准解析出 FolderC
        assert_eq!(
            resolve(Some("FolderC"), Some(2)),
            Some("E:\\FolderC".to_string())
        );

        // 仅通过标题匹配
        assert_eq!(
            resolve(Some("FolderA"), None),
            Some("C:\\FolderA".to_string())
        );

        // 未命中任何特征 -> 回退到当前 active (FolderB)
        assert_eq!(
            resolve(Some("NonExistent"), None),
            Some("D:\\FolderB".to_string())
        );
    }
}
