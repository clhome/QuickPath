#![allow(dead_code)]

//! 第三方文件管理器候选路径的后台扫描 worker + 结果缓存（审计报告 P-03）。
//!
//! 目标：把 DOpus / Total Commander / XYplorer 的**跨进程扫描从调用线程移走**。
//! 调用方（宿主消息线程）只从缓存取“最后一次成功结果”，同时获得一次**有预算的等待机会**：
//! - 正常情况下扫描只需十几毫秒，调用方几乎无感地拿到最新结果；
//! - 管理器挂起时，调用方最多等待 `budget` 就带着上一次（或空）结果继续，绝不卡住界面。
//!
//! 扫描本身仍走 `managers::ipc` 的有界 IPC + 失败冷却，因此单轮扫描也绝不会无限期占住 worker。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::dopus::get_dopus_folders;
use super::totalcmd::get_totalcmd_folders;
use super::tracker::{normalize_path, FolderCandidate};
use super::xyplorer::get_xyplorer_folders;

/// 调用方在“等一轮后台扫描”上愿意花的时间上限（超过就先用缓存）
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(120);

/// 首次扫描（尚无任何缓存）时的宽裕预算：
/// 避免“启动后第一个文件对话框”就直接缺失第三方管理器候选。
pub const FIRST_SCAN_BUDGET: Duration = Duration::from_millis(400);

struct ScanRequest {
    enable_dopus: bool,
    enable_totalcmd: bool,
    enable_xyplorer: bool,
    generation: u64,
}

struct Scanner {
    request: Mutex<Option<ScanRequest>>,
    result: Mutex<Option<(u64, Vec<FolderCandidate>)>>,
    signal: Condvar,
}

impl Scanner {
    const fn new() -> Self {
        Self {
            request: Mutex::new(None),
            result: Mutex::new(None),
            signal: Condvar::new(),
        }
    }
}

static SCANNER: OnceLock<Scanner> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);
static WORKER_STARTED: AtomicBool = AtomicBool::new(false);

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
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn worker_loop(scanner: &'static Scanner) -> ! {    loop {
        let request = {
            let mut guard = lock(&scanner.request);
            while guard.is_none() {
                guard = scanner
                    .signal
                    .wait(guard)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            guard.take().expect("循环条件保证非空")
        };

        let candidates = scan_third_party(&request);

        {
            let mut result = lock(&scanner.result);
            *result = Some((request.generation, candidates));
        }
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
    enable_dopus: bool,
    enable_totalcmd: bool,
    enable_xyplorer: bool,
    budget: Duration,
) -> Vec<FolderCandidate> {
    if !(enable_dopus || enable_totalcmd || enable_xyplorer) {
        return Vec::new();
    }

    let scanner = scanner();
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    {
        let mut request = lock(&scanner.request);
        *request = Some(ScanRequest {
            enable_dopus,
            enable_totalcmd,
            enable_xyplorer,
            generation,
        });
    }
    scanner.signal.notify_all();

    let deadline = Instant::now() + budget;
    let mut result = lock(&scanner.result);
    loop {
        if let Some((done_generation, candidates)) = result.as_ref()
            && *done_generation >= generation
        {
            return candidates.clone();
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

    // 超预算：退回上一次成功结果，绝不继续等待
    result
        .as_ref()
        .map(|(_, candidates)| candidates.clone())
        .unwrap_or_default()
}

fn scan_third_party(request: &ScanRequest) -> Vec<FolderCandidate> {
    let mut list = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

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

    #[test]
    fn test_disabled_managers_never_scan() {
        let start = Instant::now();
        let list = scan_within(false, false, false, Duration::from_millis(500));
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
        let _ = scan_within(true, true, true, Duration::ZERO);
        assert!(
            start.elapsed() < Duration::from_millis(60),
            "零预算必须立即返回，实际 {:?}",
            start.elapsed()
        );
    }
}
