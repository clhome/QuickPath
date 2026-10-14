#![allow(dead_code)]

//! 第三方文件管理器候选路径的后台扫描 worker + 结果缓存（审计报告 P-03）。
//!
//! 目标：把 DOpus / Total Commander / XYplorer 的**跨进程扫描从调用线程移走**。
//! 调用方（宿主消息线程）只从缓存取“最后一次成功结果”，同时获得一次**有预算的等待机会**：
//! - 正常情况下扫描只需十几毫秒，调用方几乎无感地拿到最新结果；
//! - 管理器挂起时，调用方最多等待 `budget` 就带着上一次（或空）结果继续，绝不卡住界面。
//!
//! 扫描本身仍走 `managers::ipc` 的有界 IPC + 失败冷却，因此单轮扫描也绝不会无限期占住 worker。
//!
//! **遥测（审计报告 P-04）**：worker 侧扫描耗时、caller 侧等待耗时分别采样（各自有界环），
//! 并统计缓存命中率、预算耗尽、队头阻塞、过期结果退回，用于回答“预算是被谁吃掉的、是否
//! 需要拆 worker”。全部计数走无锁原子，样本环走一把独立 Mutex；只在周期性/真实耗尽时
//! 才做字符串格式化，宿主热路径（零预算缓存读取）不产生任何日志。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::dopus::get_dopus_folders;
use super::explorer::get_explorer_folders;
use super::totalcmd::get_totalcmd_folders;
use super::tracker::{FolderCandidate, normalize_path};
use super::xyplorer::get_xyplorer_folders;
use crate::telemetry::{SampleRing, percentile_ms};

/// 调用方在“等一轮后台扫描”上愿意花的时间上限（超过就先用缓存）
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(120);

/// 首次扫描（尚无任何缓存）时的宽裕预算：
/// 避免“启动后第一个文件对话框”就直接缺失第三方管理器候选。
pub const FIRST_SCAN_BUDGET: Duration = Duration::from_millis(400);

/// 耗时样本环容量（worker 扫描 / caller 等待各自一份）。
const SCAN_SAMPLES: usize = 256;
/// 每完成多少次 worker 扫描输出一次 `SCAN_STATS:` 汇总。
const SCAN_STATS_LOG_EVERY: u64 = 500;
/// 预算耗尽日志的最小实际等待：低于此值视为“零预算缓存读取”（宿主热路径），不打日志。
const SCAN_EXHAUSTED_LOG_MIN_WAIT: Duration = Duration::from_millis(1);

struct ScanRequest {
    enable_explorer: bool,
    enable_dopus: bool,
    enable_totalcmd: bool,
    enable_xyplorer: bool,
    generation: u64,
}

struct Scanner {
    request: Mutex<Option<ScanRequest>>,
    result: Mutex<Option<(u64, Vec<FolderCandidate>)>>,
    signal: Condvar,
    /// worker 是否正在执行一轮扫描：新请求到来时若为真即计入「队头阻塞」。
    in_flight: AtomicBool,
}

impl Scanner {
    const fn new() -> Self {
        Self {
            request: Mutex::new(None),
            result: Mutex::new(None),
            signal: Condvar::new(),
            in_flight: AtomicBool::new(false),
        }
    }
}

static SCANNER: OnceLock<Scanner> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);
static WORKER_STARTED: AtomicBool = AtomicBool::new(false);

/// 后台扫描遥测快照（P-04：预算耗尽 / 队头阻塞 / 缓存命中率 / worker 与 caller 分位耗时）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanStatsSnapshot {
    /// 调用方发起的扫描轮次（启用至少一个管理器、进入 `scan_within` 的调用）。
    pub scans: u64,
    /// 其中预算为 0 的“缓存读取”次数（宿主热路径，不等 worker）。
    pub zero_budget_reads: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// 未在本轮预算内拿到新结果、退回缓存/空的次数。
    pub budget_exhausted: u64,
    /// 提交请求时 worker 仍在执行上一轮扫描的次数（队头阻塞）。
    pub head_of_line_blocked: u64,
    /// 预算耗尽时确实退回上一轮旧结果的次数（无缓存可退时不计数）。
    pub stale_served: u64,
    /// worker 实际执行的扫描次数（含结果被后续请求覆盖前的每一轮）。
    pub worker_runs: u64,
    pub worker_p50_ms: f64,
    pub worker_p95_ms: f64,
    pub worker_p99_ms: f64,
    pub caller_p50_ms: f64,
    pub caller_p95_ms: f64,
    pub caller_p99_ms: f64,
    /// worker 扫描耗时样本数（有界，`<= SCAN_SAMPLES`）。
    pub worker_samples: u64,
    /// caller 等待耗时样本数（有界；仅统计预算 > 0 的真实等待）。
    pub caller_samples: u64,
}

impl ScanStatsSnapshot {
    /// 缓存命中率 = 命中 /（命中 + 未命中）；分母为 0 时返回 `0.0`（不返回 NaN）。
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "scans={} zero_budget={} cache_hit={}/{} ({:.1}%) budget_exhausted={} hol_blocked={} stale_served={} worker_runs={} worker[p50={:.1} p95={:.1} p99={:.1}ms n={}] caller[p50={:.1} p95={:.1} p99={:.1}ms n={}]",
            self.scans,
            self.zero_budget_reads,
            self.cache_hits,
            self.cache_hits + self.cache_misses,
            self.cache_hit_rate() * 100.0,
            self.budget_exhausted,
            self.head_of_line_blocked,
            self.stale_served,
            self.worker_runs,
            self.worker_p50_ms,
            self.worker_p95_ms,
            self.worker_p99_ms,
            self.worker_samples,
            self.caller_p50_ms,
            self.caller_p95_ms,
            self.caller_p99_ms,
            self.caller_samples,
        )
    }
}

/// 后台扫描埋点：计数走原子，耗时样本走有界环。
struct ScanStats {
    scans: AtomicU64,
    zero_budget_reads: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    budget_exhausted: AtomicU64,
    head_of_line_blocked: AtomicU64,
    stale_served: AtomicU64,
    worker_runs: AtomicU64,
    worker_latencies_us: SampleRing,
    caller_latencies_us: SampleRing,
}

impl ScanStats {
    const fn new() -> Self {
        Self {
            scans: AtomicU64::new(0),
            zero_budget_reads: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            budget_exhausted: AtomicU64::new(0),
            head_of_line_blocked: AtomicU64::new(0),
            stale_served: AtomicU64::new(0),
            worker_runs: AtomicU64::new(0),
            worker_latencies_us: SampleRing::new(SCAN_SAMPLES),
            caller_latencies_us: SampleRing::new(SCAN_SAMPLES),
        }
    }

    /// 记录一轮 worker 扫描耗时，并按节奏输出 `SCAN_STATS:` 汇总。
    fn record_worker_scan(&self, elapsed: Duration) {
        self.worker_latencies_us.record(elapsed);
        let runs = self.worker_runs.fetch_add(1, Ordering::SeqCst) + 1;
        if runs.is_multiple_of(SCAN_STATS_LOG_EVERY) {
            crate::dialog::detector::log_debug(&format!(
                "SCAN_STATS: {}",
                self.snapshot().describe()
            ));
        }
    }

    fn snapshot(&self) -> ScanStatsSnapshot {
        let worker = self.worker_latencies_us.sorted_samples();
        let caller = self.caller_latencies_us.sorted_samples();
        ScanStatsSnapshot {
            scans: self.scans.load(Ordering::SeqCst),
            zero_budget_reads: self.zero_budget_reads.load(Ordering::SeqCst),
            cache_hits: self.cache_hits.load(Ordering::SeqCst),
            cache_misses: self.cache_misses.load(Ordering::SeqCst),
            budget_exhausted: self.budget_exhausted.load(Ordering::SeqCst),
            head_of_line_blocked: self.head_of_line_blocked.load(Ordering::SeqCst),
            stale_served: self.stale_served.load(Ordering::SeqCst),
            worker_runs: self.worker_runs.load(Ordering::SeqCst),
            worker_p50_ms: percentile_ms(&worker, 50.0),
            worker_p95_ms: percentile_ms(&worker, 95.0),
            worker_p99_ms: percentile_ms(&worker, 99.0),
            caller_p50_ms: percentile_ms(&caller, 50.0),
            caller_p95_ms: percentile_ms(&caller, 95.0),
            caller_p99_ms: percentile_ms(&caller, 99.0),
            worker_samples: worker.len() as u64,
            caller_samples: caller.len() as u64,
        }
    }
}

static SCAN_STATS: ScanStats = ScanStats::new();

/// 读取后台扫描遥测快照（P-04）。低成本：仅原子读取 + 两份有界样本排序。
pub fn stats() -> ScanStatsSnapshot {
    SCAN_STATS.snapshot()
}

fn scanner() -> &'static Scanner {
    let scanner = SCANNER.get_or_init(Scanner::new);
    if !WORKER_STARTED.swap(true, Ordering::SeqCst) {
        let _ = thread::Builder::new()
            .name("QuickPath-ManagerScan".to_string())
            .spawn(move || worker_loop(scanner));
    }
    scanner
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn worker_loop(scanner: &'static Scanner) -> ! {
    loop {
        let request = {
            let mut guard = lock(&scanner.request);
            while guard.is_none() {
                guard = scanner
                    .signal
                    .wait(guard)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            scanner.in_flight.store(true, Ordering::SeqCst);
            guard.take().expect("循环条件保证非空")
        };

        let started = Instant::now();
        let candidates = scan_sources(&request);
        let elapsed = started.elapsed();

        {
            let mut result = lock(&scanner.result);
            *result = Some((request.generation, candidates));
        }
        scanner.in_flight.store(false, Ordering::SeqCst);
        SCAN_STATS.record_worker_scan(elapsed);
        scanner.signal.notify_all();
    }
}

/// 是否已经有过一次成功的扫描结果（用于选择首次/稳态预算）
pub fn has_cached_result() -> bool {
    SCANNER
        .get()
        .map(|scanner| lock(&scanner.result).is_some())
        .unwrap_or(false)
}

/// 请求一次后台扫描，并在 `budget` 内等待**本轮**结果。
///
/// 超预算则退回上一次成功结果（可能为空列表）——这正是“绝不阻塞调用线程”的保证。
pub fn scan_within(
    enable_explorer: bool,
    enable_dopus: bool,
    enable_totalcmd: bool,
    enable_xyplorer: bool,
    budget: Duration,
) -> Vec<FolderCandidate> {
    if !(enable_explorer || enable_dopus || enable_totalcmd || enable_xyplorer) {
        return Vec::new();
    }

    let scanner = scanner();
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let request = ScanRequest {
        enable_explorer,
        enable_dopus,
        enable_totalcmd,
        enable_xyplorer,
        generation,
    };
    serve_request(scanner, request, budget, &SCAN_STATS)
}

/// 提交一轮扫描并在 `budget` 内等待**本轮**结果；超预算退回上一次成功结果。
///
/// 从 `scan_within` 抽出以便单测：测试用独立 `Scanner`（无 worker）构造确定性场景，
/// 既不需要真实第三方管理器，也不受全局 `NEXT_GENERATION` 并发竞争影响。
fn serve_request(
    scanner: &Scanner,
    request: ScanRequest,
    budget: Duration,
    stats: &ScanStats,
) -> Vec<FolderCandidate> {
    let generation = request.generation;
    let zero_budget = budget.is_zero();

    stats.scans.fetch_add(1, Ordering::SeqCst);
    if zero_budget {
        stats.zero_budget_reads.fetch_add(1, Ordering::SeqCst);
    }

    // 缓存命中判定：进入时缓存已有“上一轮或更新”的结果（热缓存）。
    // 本轮新分配的 generation 必然严格大于任何既有缓存结果的 generation（单调递增），
    // 因此字面的 `done >= generation` 恒为假；按报告意图取 `done + 1 >= generation`，
    // 语义为「缓存已含上一轮结果，调用方有可退回的快照」。
    let cache_hit = {
        let result = lock(&scanner.result);
        matches!(result.as_ref(), Some((done, _)) if done.saturating_add(1) >= generation)
    };
    if cache_hit {
        stats.cache_hits.fetch_add(1, Ordering::SeqCst);
    } else {
        stats.cache_misses.fetch_add(1, Ordering::SeqCst);
    }

    // 队头阻塞定义：提交新请求的瞬间 worker 仍在执行上一轮扫描。
    // 选择它而非「等待超时且 worker 仍在忙」的理由：后者是前者的子集（几乎只在预算
    // 耗尽时成立），会与 `budget_exhausted` 高度重合；前者独立度量“单 worker 排队”这一
    // 报告 P-04 关心的现象。worker 取走请求到置位之间有一个极小窗口会造成轻微低估，可接受。
    if scanner.in_flight.load(Ordering::SeqCst) {
        stats.head_of_line_blocked.fetch_add(1, Ordering::SeqCst);
    }

    {
        let mut slot = lock(&scanner.request);
        *slot = Some(request);
    }
    scanner.signal.notify_all();

    let wait_start = Instant::now();
    let deadline = wait_start + budget;
    let mut result = lock(&scanner.result);
    let mut accepted: Option<Vec<FolderCandidate>> = None;
    loop {
        if let Some((done_generation, candidates)) = result.as_ref()
            && *done_generation >= generation
        {
            accepted = Some(candidates.clone());
            break;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (guard, timeout) = scanner
            .signal
            .wait_timeout(result, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        result = guard;
        if timeout.timed_out() {
            break;
        }
    }

    let waited = wait_start.elapsed();

    if let Some(candidates) = accepted {
        drop(result);
        // 零预算读取按设计不等，不计入等待样本，避免把等待分位稀释为 0。
        if !zero_budget {
            stats.caller_latencies_us.record(waited);
        }
        return candidates;
    }

    // 超预算：退回上一次成功结果，绝不继续等待
    stats.budget_exhausted.fetch_add(1, Ordering::SeqCst);
    let stale_generation = result.as_ref().map(|(done, _)| *done);
    let stale = result.as_ref().map(|(_, candidates)| candidates.clone());
    drop(result);

    if !zero_budget {
        stats.caller_latencies_us.record(waited);
    }
    if waited >= SCAN_EXHAUSTED_LOG_MIN_WAIT {
        // 只有真实等到预算上限的调用才输出日志；零预算缓存读取（宿主热路径，等待≈0）
        // 不在此格式化字符串，避免热路径性能回归。
        crate::dialog::detector::log_debug(&format!(
            "SCAN_BUDGET_EXHAUSTED: request_generation={} worker_busy={} waited_ms={:.1} returned_generation={}",
            generation,
            scanner.in_flight.load(Ordering::SeqCst),
            waited.as_secs_f64() * 1000.0,
            stale_generation.map_or_else(|| "none".to_string(), |g| g.to_string()),
        ));
    }

    match stale {
        Some(candidates) => {
            stats.stale_served.fetch_add(1, Ordering::SeqCst);
            candidates
        }
        None => Vec::new(),
    }
}

fn scan_sources(request: &ScanRequest) -> Vec<FolderCandidate> {
    let mut list = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // 资源管理器（含 Win11 标签页）：与第三方共用同一个后台 worker，
    // 这样宿主消息线程读取候选时永远不需要做任何跨进程/COM 等待。
    if request.enable_explorer {
        for folder in get_explorer_folders() {
            let normalized = normalize_path(&folder.path);
            if seen.insert(normalized.to_lowercase()) {
                list.push(FolderCandidate {
                    path: normalized,
                    title: folder.title,
                    source: "资源管理器".to_string(),
                    is_active: folder.is_active,
                });
            }
        }
    }

    if request.enable_dopus {
        for folder in get_dopus_folders() {
            let normalized = normalize_path(&folder.path);
            if seen.insert(normalized.to_lowercase()) {
                list.push(FolderCandidate {
                    path: normalized,
                    title: folder.title,
                    source: "Directory Opus".to_string(),
                    is_active: folder.is_active,
                });
            }
        }
    }

    if request.enable_totalcmd {
        for folder in get_totalcmd_folders() {
            let normalized = normalize_path(&folder.path);
            if seen.insert(normalized.to_lowercase()) {
                list.push(FolderCandidate {
                    path: normalized,
                    title: folder.title,
                    source: "Total Commander".to_string(),
                    is_active: folder.is_active,
                });
            }
        }
    }

    if request.enable_xyplorer {
        for folder in get_xyplorer_folders() {
            let normalized = normalize_path(&folder.path);
            if seen.insert(normalized.to_lowercase()) {
                list.push(FolderCandidate {
                    path: normalized,
                    title: folder.title,
                    source: "XYplorer".to_string(),
                    is_active: folder.is_active,
                });
            }
        }
    }

    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(generation: u64) -> ScanRequest {
        ScanRequest {
            enable_explorer: true,
            enable_dopus: true,
            enable_totalcmd: true,
            enable_xyplorer: true,
            generation,
        }
    }

    fn candidate(path: &str) -> FolderCandidate {
        FolderCandidate {
            path: path.to_string(),
            title: path.to_string(),
            source: "资源管理器".to_string(),
            is_active: true,
        }
    }

    #[test]
    fn test_disabled_managers_never_scan() {
        let start = Instant::now();
        let list = scan_within(false, false, false, false, Duration::from_millis(500));
        assert!(list.is_empty());
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "全部禁用时必须直接返回，不走 worker"
        );
    }

    #[test]
    fn test_zero_budget_returns_without_blocking() {
        // 预算为 0：无论 worker 是否完成，都必须在毫秒级返回（绝不阻塞调用线程）
        let start = Instant::now();
        let _ = scan_within(true, true, true, true, Duration::ZERO);
        assert!(
            start.elapsed() < Duration::from_millis(60),
            "零预算必须立即返回，实际 {:?}",
            start.elapsed()
        );
    }

    /// 缓存命中率分母为 0 时必须返回 0，而不是 NaN/panic。
    #[test]
    fn test_cache_hit_rate_is_zero_without_samples() {
        let stats = ScanStats::new();
        assert_eq!(
            stats.snapshot().cache_hit_rate(),
            0.0,
            "无命中/未命中样本时命中率必须为 0"
        );
    }

    /// 零预算连续调用（worker 未产出、无缓存）每轮都必须计入预算耗尽。
    #[test]
    fn test_zero_budget_exhaustion_is_counted() {
        let scanner = Scanner::new();
        let stats = ScanStats::new();
        for generation in 1..=3 {
            let out = serve_request(&scanner, req(generation), Duration::ZERO, &stats);
            assert!(out.is_empty(), "无 worker 且无缓存时必须返回空");
        }
        let snap = stats.snapshot();
        assert_eq!(snap.scans, 3);
        assert_eq!(
            snap.budget_exhausted, 3,
            "零预算且 worker 未产出时每轮都应计入预算耗尽"
        );
        assert_eq!(snap.zero_budget_reads, 3);
        assert_eq!(snap.stale_served, 0, "无缓存可退回时不应计入过期退回");
    }

    /// 预算耗尽且存在上一轮结果时：必须退回旧结果并计入 `stale_served`。
    #[test]
    fn test_stale_result_served_and_counted() {
        let scanner = Scanner::new();
        let stats = ScanStats::new();
        *lock(&scanner.result) = Some((1, vec![candidate("C:/prev")]));

        let out = serve_request(&scanner, req(2), Duration::ZERO, &stats);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "C:/prev", "预算耗尽必须退回上一轮缓存结果");

        let snap = stats.snapshot();
        assert_eq!(snap.budget_exhausted, 1);
        assert_eq!(snap.stale_served, 1, "退回旧结果必须计入 stale_served");
    }

    /// 冷启动（无缓存）记未命中；已有上一轮结果记命中。
    #[test]
    fn test_cache_hit_and_miss_recorded() {
        let scanner = Scanner::new();
        let stats = ScanStats::new();

        let _ = serve_request(&scanner, req(1), Duration::ZERO, &stats);
        *lock(&scanner.result) = Some((1, vec![candidate("C:/x")]));
        let _ = serve_request(&scanner, req(2), Duration::ZERO, &stats);

        let snap = stats.snapshot();
        assert_eq!(snap.cache_misses, 1, "冷启动应为未命中");
        assert_eq!(snap.cache_hits, 1, "已有上一轮结果应为命中");
        assert!((snap.cache_hit_rate() - 0.5).abs() < 1e-9);
    }

    /// 队头阻塞只在提交请求时 worker 仍在忙的情况下计数。
    #[test]
    fn test_head_of_line_blocked_counted_only_when_worker_busy() {
        let scanner = Scanner::new();
        let stats = ScanStats::new();

        scanner.in_flight.store(true, Ordering::SeqCst);
        let _ = serve_request(&scanner, req(1), Duration::ZERO, &stats);
        scanner.in_flight.store(false, Ordering::SeqCst);
        let _ = serve_request(&scanner, req(2), Duration::ZERO, &stats);

        let snap = stats.snapshot();
        assert_eq!(
            snap.head_of_line_blocked, 1,
            "仅 worker 忙时到来的请求计入队头阻塞"
        );
    }

    /// caller 等待样本环只记录预算 > 0 的真实等待，零预算缓存读取不进入样本。
    #[test]
    fn test_caller_wait_samples_only_for_nonzero_budget() {
        let scanner = Scanner::new();
        let stats = ScanStats::new();

        let _ = serve_request(&scanner, req(1), Duration::ZERO, &stats);
        assert_eq!(
            stats.snapshot().caller_samples,
            0,
            "零预算缓存读取不计入等待样本"
        );

        let _ = serve_request(&scanner, req(2), Duration::from_millis(1), &stats);
        assert_eq!(stats.snapshot().caller_samples, 1, "真实等待应留下一个样本");
    }

    /// worker 扫描分位：给定已知样本断言 p50/p95/p99（复用 `com.rs` 的分位写法）。
    #[test]
    fn test_worker_scan_percentiles() {
        let stats = ScanStats::new();
        for ms in [1u64, 2, 3, 4, 5] {
            stats.record_worker_scan(Duration::from_millis(ms));
        }
        let snap = stats.snapshot();
        assert!(
            (snap.worker_p50_ms - 3.0).abs() < 1e-6,
            "p50 应为 3ms，实际 {}",
            snap.worker_p50_ms
        );
        assert!(
            (snap.worker_p95_ms - 5.0).abs() < 1e-6,
            "p95 应为 5ms，实际 {}",
            snap.worker_p95_ms
        );
        assert!((snap.worker_p99_ms - 5.0).abs() < 1e-6);
        assert_eq!(snap.worker_runs, 5);
        assert_eq!(snap.worker_samples, 5);
    }

    /// 耗时样本环必须有界：样本数不超过环容量，而计数持续累加。
    #[test]
    fn test_sample_rings_are_bounded() {
        let stats = ScanStats::new();
        let records = SCAN_SAMPLES as u64 * 4;
        for i in 0..records {
            stats.record_worker_scan(Duration::from_micros(i));
        }
        let snap = stats.snapshot();
        assert_eq!(
            snap.worker_samples, SCAN_SAMPLES as u64,
            "样本数必须被环容量封顶，不得无限增长"
        );
        assert_eq!(snap.worker_runs, records, "计数不受环容量限制");
    }
}
