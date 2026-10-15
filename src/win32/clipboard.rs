#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM,
    WPARAM,
};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, CountClipboardFormats, EmptyClipboard,
    EnumClipboardFormats, GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber,
    OpenClipboard, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowThreadProcessId, IsWindow, MSG, PostMessageW, PostQuitMessage, PostThreadMessageW,
    RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE, WM_CLOSE,
    WM_DESTROY, WM_QUIT, WNDCLASSW,
};
use windows::core::w;

/// 剪贴板标准格式常量定义
pub const CF_TEXT: u32 = 1;
pub const CF_BITMAP: u32 = 2;
pub const CF_METAFILEPICT: u32 = 3;
pub const CF_OEMTEXT: u32 = 7;
pub const CF_DIB: u32 = 8;
pub const CF_PALETTE: u32 = 9;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_ENHMETAFILE: u32 = 14;
pub const CF_HDROP: u32 = 15;
pub const CF_LOCALE: u32 = 16;
pub const CF_DIBV5: u32 = 17;

/// 判定是否为 GDI 或图元文件专有句柄格式（严禁调用 GlobalLock / GlobalSize）
pub fn is_gdi_or_metafile_format(fmt: u32) -> bool {
    matches!(
        fmt,
        CF_BITMAP | CF_PALETTE | CF_METAFILEPICT | CF_ENHMETAFILE
    )
}

/// 单项数据最大允许备份字节数（20 MB），防范极端大对象内存耗尽
const MAX_ITEM_BYTES: usize = 20 * 1024 * 1024;
/// 总备份最大允许字节数（40 MB）
const MAX_TOTAL_BYTES: usize = 40 * 1024 * 1024;

/// 剪贴板安全操作错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardSafetyError {
    /// 剪贴板被其他应用锁定占用，超时无法打开
    Busy,
    /// 剪贴板数据备份不完整或存在无法保真克隆的格式，为保护用户数据放弃写入
    BackupIncomplete(String),
    /// 剪贴板临时文本写入失败
    WriteFailed,
    /// 临时使用结束后恢复原剪贴板失败（逐格式恢复未全部成功）
    RestoreFailed(ClipboardRestoreError),
}

/// 剪贴板恢复失败原因：与「主动放弃覆盖」严格区分，便于上层决定是否补救与重试
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardRestoreError {
    /// 剪贴板被其他进程长时间占用，无法打开
    Busy,
    /// EmptyClipboard 失败，剪贴板未能清空
    EmptyFailed,
    /// 部分格式未能写回（列出失败格式 ID，便于定位与复现）
    PartialRestore(Vec<u32>),
}

/// 剪贴板恢复结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreStatus {
    /// 全部格式已成功写回
    Restored,
    /// 此前已成功恢复过，无需重复操作
    AlreadyRestored,
    /// 序列号不一致 / owner 不匹配 / 状态不明：**永久放弃**该快照，绝不清空或写入
    /// （审计需求 `9549648` §3.4.3）
    Abandoned,
}

impl std::fmt::Display for ClipboardSafetyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(f, "剪贴板被其他进程占用，打开超时"),
            Self::BackupIncomplete(msg) => write!(f, "剪贴板备份未完整完成: {}", msg),
            Self::WriteFailed => write!(f, "临时写入剪贴板文本失败"),
            Self::RestoreFailed(err) => write!(f, "恢复原剪贴板失败: {:?}", err),
        }
    }
}

/// 单次恢复尝试的最大次数（失败只补写未成功格式，不重复清空已恢复内容）
const RESTORE_ATTEMPTS: u32 = 3;

/// 无注入测试钩子：值为 u32::MAX 表示不注入失败
#[cfg(test)]
static TEST_FAIL_FORMAT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// 测试注入的失败环节（仅当 `TEST_FAIL_FORMAT` 命中时生效）：
/// 0 = `SetClipboardData` 失败（默认），1 = `GlobalAlloc` 失败，2 = `GlobalLock` 失败。
#[cfg(test)]
static TEST_FAIL_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// 读取系统剪贴板序列号（每次剪贴板内容变化都会自增）
fn clipboard_sequence_number() -> u32 {
    #[cfg(test)]
    if TEST_FORCE_SEQ_ZERO.load(std::sync::atomic::Ordering::SeqCst) {
        return 0;
    }
    unsafe { GetClipboardSequenceNumber() }
}

// ══════════════════════════════════════════════════════════════════════════
// 剪贴板更新事件链（审计报告 P0-04 竞态 B）
//
// 「请求前后各读一次序列号」只能证明两个采样点，无法证明两点之间没有第三方写入。
// owner 窗口注册 `AddClipboardFormatListener` 后，每次剪贴板更新都会收到
// `WM_CLIPBOARDUPDATE`；这里把「更新序号 + 当时序列号 + owner 进程」记入有界环，
// 使 `ClipboardTransaction` 能对「请求窗口内的更新是否全部来自目标管理器」做来源链
// 证明：一旦出现无法归因到目标管理器的更新，就必须判为外部改写并保守放弃认领。
// ══════════════════════════════════════════════════════════════════════════

/// 一次剪贴板更新事件（owner 线程在 `WM_CLIPBOARDUPDATE` 时登记）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClipboardUpdateEvent {
    /// 全局单调递增的事件序号（用作「请求开始标记」）
    index: u64,
    /// 登记时读到的剪贴板序列号
    sequence: u32,
    /// 登记时的剪贴板 owner 进程 id（无法判定为 0）
    owner_pid: u32,
}

/// 事件环容量：只需覆盖单次请求窗口（毫秒级到 500ms）内的更新，64 条足够。
const CLIPBOARD_EVENT_CAPACITY: usize = 64;

/// 等待事件链追上当前序列号的最大轮数（每轮 1ms，约 200ms 上限）。
/// `WM_CLIPBOARDUPDATE` 是投递消息，登记可能落后于写入；机器繁忙 / CI runner 较慢时
/// 需要比“几毫秒”更宽松的预算，否则会把正常的滞后误判为「来源链不可证明」。
const CLIPBOARD_EVENT_COVER_POLLS: u32 = 200;

/// 「排空自身写入通知」时要求的静默时长：最后一次登记追平自身序列号后，
/// 仍需连续这么多毫秒没有新通知，才认定本次写入的全部通知都已登记完毕。
///
/// 为什么必须排空：请求窗口的来源判据是「窗口内恰好一次可归因更新」；
/// 若本事务自身那次临时写入的通知迟到、落进请求窗口，就会把合法扫描误判为
/// 不可证明（fail-closed 的误伤）。实测通知延迟在 0~16ms 量级，故取 5ms 为静默阈值。
const CLIPBOARD_QUIESCE_MS: u32 = 5;

static CLIPBOARD_EVENTS: Mutex<VecDeque<ClipboardUpdateEvent>> = Mutex::new(VecDeque::new());
/// 已登记事件总数（即下一个事件的序号）；作为请求开始标记，避免依赖清空环。
static CLIPBOARD_EVENT_TOTAL: AtomicU64 = AtomicU64::new(0);

/// 登记一次剪贴板更新（仅由 owner 线程的 `WM_CLIPBOARDUPDATE` 调用）。
fn record_clipboard_update_event(owner_pid: u32) {
    let index = CLIPBOARD_EVENT_TOTAL.fetch_add(1, Ordering::SeqCst);
    let event = ClipboardUpdateEvent {
        index,
        sequence: clipboard_sequence_number(),
        owner_pid,
    };
    let mut events = CLIPBOARD_EVENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if events.len() == CLIPBOARD_EVENT_CAPACITY {
        events.pop_front();
    }
    events.push_back(event);
}

/// 测试注入：把一条「合成剪贴板更新」追加进事件环，用于确定性地驱动来源链判定。
#[cfg(test)]
pub(crate) fn inject_clipboard_update_event_for_test(owner_pid: u32) {
    record_clipboard_update_event(owner_pid);
}

/// 取得「请求开始标记」：记录此刻的事件总数，之后登记的更新都属于本次请求窗口。
fn clipboard_update_marker() -> u64 {
    CLIPBOARD_EVENT_TOTAL.load(Ordering::SeqCst)
}

/// 取出标记之后的全部事件；若事件已溢出有界环则返回 `None`（来源链不可证明）。
fn clipboard_updates_since(marker: u64) -> Option<Vec<ClipboardUpdateEvent>> {
    let events = CLIPBOARD_EVENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let total = CLIPBOARD_EVENT_TOTAL.load(Ordering::SeqCst);
    if total.saturating_sub(marker) > CLIPBOARD_EVENT_CAPACITY as u64 {
        return None;
    }
    Some(
        events
            .iter()
            .filter(|event| event.index >= marker)
            .copied()
            .collect(),
    )
}

/// 当前剪贴板 owner 的进程 id（owner 为 NULL / 已失效时返回 0）。
fn clipboard_owner_pid() -> u32 {
    let Ok(owner) = (unsafe { GetClipboardOwner() }) else {
        return 0;
    };
    if owner.0.is_null() {
        return 0;
    }
    // 注意：`GetWindowThreadProcessId` 的返回值是**线程 id**，进程 id 写在出参里，
    // 传 `None` 只能拿到线程 id，不能当进程 id 用。
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(owner, Some(&mut pid)) };
    pid
}

/// 事件链是否已追平给定序列号（即最后一次更新已被 owner 线程登记）。
///
/// `WM_CLIPBOARDUPDATE` 是投递消息，登记可能略滞后于写入；这里做有界等待，
/// 等待不到就返回 `false`，由调用方按「来源链不可证明」保守处理。
fn clipboard_update_log_covers(sequence: u32) -> bool {
    for _ in 0..CLIPBOARD_EVENT_COVER_POLLS {
        {
            let events = CLIPBOARD_EVENTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(last) = events.back()
                && last.sequence == sequence
            {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    false
}

/// 有界等待「本次写入触发的全部通知都已登记且事件环已静默」。
///
/// 供 `probe_panel_path` 在**取请求标记之前**调用：只有把自身那次临时写入的通知
/// 排空，请求窗口里的「恰好一次更新」计数才只统计到第三方的更新。
fn clipboard_update_log_quiesced(expected_sequence: u32) -> bool {
    if !clipboard_update_log_covers(expected_sequence) {
        return false;
    }
    let mut last_total = CLIPBOARD_EVENT_TOTAL.load(Ordering::SeqCst);
    let mut quiet_since = Instant::now();
    let deadline = Instant::now() + Duration::from_millis(u64::from(CLIPBOARD_QUIESCE_MS) * 8);
    while Instant::now() < deadline {
        let total = CLIPBOARD_EVENT_TOTAL.load(Ordering::SeqCst);
        if total != last_total {
            last_total = total;
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= Duration::from_millis(u64::from(CLIPBOARD_QUIESCE_MS)) {
            return true;
        }
        thread::sleep(Duration::from_millis(1));
    }
    // 静默窗口内一直有新事件：来源无法收敛，判为不可证明（fail-closed）。
    false
}

/// 测试期会合点：等待事件环在 `marker` 之后登记到至少 `count` 条事件（超时返回 false）。
#[cfg(test)]
fn wait_for_events_after(marker: u64, count: u64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if CLIPBOARD_EVENT_TOTAL
            .load(Ordering::SeqCst)
            .saturating_sub(marker)
            >= count
        {
            return true;
        }
        thread::sleep(Duration::from_millis(1));
    }
    false
}

impl std::error::Error for ClipboardSafetyError {}

/// 测试期强制「剪贴板被占用」注入（同进程内无法真实模拟跨进程独占）
#[cfg(test)]
static TEST_FORCE_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 测试期强制「剪贴板序列号不可读」注入：让 `clipboard_sequence_number()` 返回 0，
/// 用于验证事务在归属不可判定时必须保守放弃恢复（绝不覆盖当前内容）。
#[cfg(test)]
static TEST_FORCE_SEQ_ZERO: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 测试期强制「owner 创建失败」注入：让 `clipboard_owner()` 返回 None，
/// 用于验证 §12「owner 创建失败时原剪贴板保持不变」的 fail-closed 语义。
#[cfg(test)]
static TEST_FORCE_NO_OWNER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 测试期会合点：在统一原子写入原语**开始之前**执行一次注入动作。
///
/// 这是确定性复现「决策通过后、真正持锁复核前发生外部写入」的会合点：
/// 既覆盖审计报告 P0-04 竞态 A（写入方向），也覆盖清单 9549648 §3.5 的恢复方向。
#[cfg(test)]
struct AtomicWriteTestHook {
    /// 跳过前面这么多次原子写入后再执行（用于「第一次恢复部分失败、第二次重试前」的交错）
    skip: usize,
    action: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
static TEST_BEFORE_ATOMIC_WRITE: Mutex<Option<AtomicWriteTestHook>> = Mutex::new(None);

/// 测试注入：安排在下一次（可跳过前 `skip` 次）统一原子写入开始之前执行 `action`。
#[cfg(test)]
fn arm_atomic_write_hook(skip: usize, action: impl FnOnce() + Send + 'static) {
    *TEST_BEFORE_ATOMIC_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(AtomicWriteTestHook {
        skip,
        action: Box::new(action),
    });
}

/// 执行（并消费）原子写入会合点。
#[cfg(test)]
fn run_atomic_write_hook() {
    let mut guard = TEST_BEFORE_ATOMIC_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ready = match guard.as_mut() {
        None => false,
        Some(hook) => {
            if hook.skip > 0 {
                hook.skip -= 1;
                false
            } else {
                true
            }
        }
    };
    if !ready {
        return;
    }
    let hook = guard.take();
    drop(guard);
    if let Some(hook) = hook {
        (hook.action)();
    }
}

/// 测试期统计：统一原子原语真正执行过多少次 `EmptyClipboard`。
/// 用于断言「检出外部改写后必须零修改退出」「Drop 不得发生第二次恢复」等。
#[cfg(test)]
static TEST_EMPTY_CLIPBOARD_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// owner 消息泵测试会合点：消息泵在此阻塞，直到用例放行。
///
/// 用途：确定性复现清单 §2.3「两次剪贴板更新都已发生、通知尚未处理」的交错 ——
/// 这是 P0-04-B 的核心反例，必须经**真实生产事件采集入口**验证。
#[cfg(test)]
struct OwnerPumpBlock {
    reached_tx: mpsc::Sender<()>,
    release_rx: Mutex<mpsc::Receiver<()>>,
}

#[cfg(test)]
static TEST_OWNER_PUMP_BLOCK: Mutex<Option<OwnerPumpBlock>> = Mutex::new(None);

/// 测试期专用窗口消息：让 owner 线程在消息泵内部阻塞（`WM_APP + 0x51`）。
#[cfg(test)]
const WM_QP_TEST_BLOCK_PUMP: u32 = 0x8000 + 0x51;

/// 带指数退避的剪贴板独占打开函数
fn open_clipboard_with_retry(hwnd: Option<HWND>, max_retries: u32) -> bool {
    #[cfg(test)]
    if TEST_FORCE_BUSY.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }

    for i in 0..max_retries {
        unsafe {
            if OpenClipboard(hwnd).is_ok() {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(25 * (1 << i.min(3))));
    }
    false
}

/// 进程级剪贴板 owner 的拥有式状态：可关闭、可重建。
///
/// 审计报告 R-06：此前用 `Once` 一次性创建、`JoinHandle` 直接丢弃，owner 线程没有
/// 显式退出协议，进程内无法「启动 → 停止 → 再启动」，只能依赖进程结束回收。
/// 这里保存窗口句柄、线程 ID、世代号，以及退出确认通道与线程句柄。
///
/// `HWND` 本身不是 `Send`，无法直接放进跨线程共享的静态量；因此以 `usize` 保存原始
/// 句柄值，使用前重建 `HWND`（`HWND` 只是不透明指针包装，重建无额外成本）。
struct ClipboardOwnerHandle {
    /// owner 窗口原始句柄值（`HWND` 的 `*mut c_void` 地址）
    hwnd_raw: usize,
    /// 创建该窗口的线程 ID，用于 `PostThreadMessageW(WM_QUIT)` 兜底
    thread_id: u32,
    /// 每次重建递增的世代号，仅用于诊断日志
    generation: u64,
    /// 线程退出确认通道：线程体返回前 `send(())`
    done_rx: mpsc::Receiver<()>,
    /// 线程句柄；`join` 前由 `Option` 取出，超时则丢弃（detach）
    thread: Option<JoinHandle<()>>,
}

/// 创建串行化锁（审计报告 R-07）：与 owner 状态锁 `OWNER` 分离。
///
/// `create_clipboard_owner()` 需要串行化并发创建；但创建期的等待（`ready`/`done`）
/// 绝不能持有 `OWNER`——owner 线程在取消检查点与退出清理时都要短暂获取 `OWNER`，
/// 一旦创建方持锁等待就会与清理互相阻塞，让「超时后确认线程退出」结构性失效。
static OWNER_CREATE_LOCK: Mutex<()> = Mutex::new(());

static OWNER: Mutex<Option<ClipboardOwnerHandle>> = Mutex::new(None);
static OWNER_GENERATION: AtomicU64 = AtomicU64::new(0);

/// owner 线程退出确认的有界等待时长；超时即放弃 `join`（detach），绝不永久阻塞宿主线程。
const OWNER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// owner 窗口就绪的默认等待时长；超时后进入「取消握手」路径（见 `create_clipboard_owner`）。
const OWNER_READY_TIMEOUT: Duration = Duration::from_secs(3);

/// owner 线程创建过程中可由测试驱动的会合阶段（仅测试构建存在）。
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OwnerCreationStage {
    /// `RegisterClassW` 之后、创建窗口之前
    AfterClassRegistered,
    /// `CreateWindowExW` 成功之后、进入消息泵之前
    AfterWindowCreated,
}

/// owner 创建期的测试会合点：让用例以通道/屏障在指定阶段会合，用阶段信号替代
/// `sleep` 猜时序，从而确定性地验证「超时取消」与「取消/创建并发」（仅测试构建存在）。
#[cfg(test)]
struct OwnerTestHooks {
    /// 需要会合的目标阶段
    stage: OwnerCreationStage,
    /// owner 线程到达目标阶段时上报
    reached_tx: mpsc::Sender<OwnerCreationStage>,
    /// owner 线程在目标阶段阻塞，等待用例放行
    release_rx: Mutex<mpsc::Receiver<()>>,
    /// 超时路径置位取消标记后通知用例
    cancel_tx: mpsc::Sender<()>,
    /// owner 线程已创建窗口的原始句柄（供用例断言无主窗口）
    created_hwnd: Mutex<Option<usize>>,
    /// 覆盖就绪等待超时，避免用例真的等待默认时长
    ready_timeout: Duration,
}

#[cfg(test)]
impl OwnerTestHooks {
    /// 到达指定阶段：记录已创建窗口（若有）、上报阶段，并阻塞等待用例放行。
    fn rendezvous(&self, stage: OwnerCreationStage, created_hwnd_raw: Option<usize>) {
        if self.stage != stage {
            return;
        }
        if let Some(raw) = created_hwnd_raw {
            *self
                .created_hwnd
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(raw);
        }
        let _ = self.reached_tx.send(stage);
        let _ = self
            .release_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recv();
    }
}

/// 当前生效的 owner 创建期测试会合点（仅测试构建存在）。
#[cfg(test)]
static TEST_OWNER_HOOKS: Mutex<Option<Arc<OwnerTestHooks>>> = Mutex::new(None);

unsafe extern "system" fn clipboard_owner_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    // 测试期会合点：按用例指令在消息泵内部阻塞（只影响测试构建）。
    #[cfg(test)]
    if msg == WM_QP_TEST_BLOCK_PUMP {
        let hook = TEST_OWNER_PUMP_BLOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(hook) = hook {
            let _ = hook.reached_tx.send(());
            let _ = hook
                .release_rx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv_timeout(Duration::from_secs(30));
        }
        return windows::Win32::Foundation::LRESULT(0);
    }
    match msg {
        // 审计报告 R-06：显式退出协议。收到 `WM_CLOSE` 时销毁窗口，`DestroyWindow`
        // 会在同一线程上同步派发 `WM_DESTROY`。
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            windows::Win32::Foundation::LRESULT(0)
        }
        // `WM_DESTROY` 时注销剪贴板监听并投递 `WM_QUIT`，使 `GetMessageW` 返回 0、消息泵自然结束。
        WM_DESTROY => {
            let _ = unsafe { RemoveClipboardFormatListener(hwnd) };
            unsafe {
                PostQuitMessage(0);
            }
            windows::Win32::Foundation::LRESULT(0)
        }
        // 审计报告 P0-04 竞态 B：登记每次剪贴板更新（序列号 + owner 进程），
        // 供事务证明「请求窗口内的更新是否全部来自目标管理器」。
        WM_CLIPBOARDUPDATE => {
            record_clipboard_update_event(clipboard_owner_pid());
            windows::Win32::Foundation::LRESULT(0)
        }
        // owner 窗口永不可见、不需要处理任何业务消息（本模块不使用延迟渲染），
        // 其余消息交由默认窗口过程处理。
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 懒创建并返回进程唯一的隐藏 owner 窗口句柄；已有存活 owner 时复用。
///
/// **审计报告 P0-01**：Win32 明确约定「以 `NULL` 打开剪贴板后 `EmptyClipboard` 会把
/// owner 置为 `NULL`，从而导致 `SetClipboardData` 失败」。因此所有会调用
/// `EmptyClipboard` / `SetClipboardData` 的写路径都必须以有效 HWND 打开剪贴板。
/// 这里在专用线程上创建一个隐藏顶层窗口并跑消息泵，使句柄在进程存活期内始终有效。
fn clipboard_owner() -> Option<HWND> {
    #[cfg(test)]
    if TEST_FORCE_NO_OWNER.load(Ordering::SeqCst) {
        return None;
    }

    // 快路径：已有 owner 时复用。仅在锁内读取原始句柄值，`IsWindow` 校验放到锁外，
    // 避免在使用 owner 期间长时间持有 `Mutex`。
    let existing_raw = {
        let guard = OWNER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.as_ref().map(|owner| owner.hwnd_raw)
    };
    if let Some(raw) = existing_raw
        && raw != 0
    {
        let hwnd = HWND(raw as *mut _);
        if unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Some(hwnd);
        }
    }

    create_clipboard_owner()
}

/// 创建 owner 线程与隐藏窗口，并把拥有式状态写入 `OWNER`。
///
/// 创建期间持有 `OWNER` 锁以串行化并发创建（对齐原 `Once::call_once` 语义：其他调用方
/// 在创建完成前等待）。owner 窗口过程不会回调本模块，因此持锁创建不存在重入死锁。
fn create_clipboard_owner() -> Option<HWND> {
    // 审计报告 R-07：用独立的创建串行化锁；创建期的等待不持有 `OWNER`（详见
    // `OWNER_CREATE_LOCK` 的说明）。
    let _create_guard = OWNER_CREATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // 双重检查：等待锁期间可能已由其他线程创建。仅持 `OWNER` 做短状态读写，立即释放。
    {
        let mut guard = OWNER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = guard.as_ref()
            && existing.hwnd_raw != 0
        {
            let hwnd = HWND(existing.hwnd_raw as *mut _);
            if unsafe { IsWindow(Some(hwnd)).as_bool() } {
                return Some(hwnd);
            }
        }
        // 陈旧句柄：清空后重建（正常退出路径已在 `shutdown_clipboard_owner` 中清空）。
        *guard = None;
    }

    let generation = OWNER_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    let (ready_tx, ready_rx) = mpsc::channel::<usize>();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    // 审计报告 P2-4.1：共享取消标记，超时后由父线程置位，owner 线程在三个检查点
    // （注册类后 / 创建窗口前 / 进入消息泵前）观察并干净退出。
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = Arc::clone(&cancel);

    #[cfg(test)]
    let test_hooks = TEST_OWNER_HOOKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    let spawned = thread::Builder::new()
        .name("QuickPath-ClipboardOwner".to_string())
        .spawn(move || unsafe {
            let class_name = w!("QuickPath_ClipboardOwner_Class");

            // 审计报告 S-02：`GetModuleHandleW(None)` 失败时绝不能静默退化为 NULL 实例。
            // 拿不到模块实例就无法正确注册/查找窗口类，此处显式判定失败并让
            // `clipboard_owner()` 返回 None（保持写路径 fail-closed），同时留下诊断日志。
            let Ok(module) = GetModuleHandleW(None) else {
                crate::dialog::detector::log_debug(&format!(
                    "CLIPBOARD_OWNER: GetModuleHandleW 失败，err={}，无法注册 owner 窗口类",
                    GetLastError().0
                ));
                let _ = ready_tx.send(0);
                let _ = done_tx.send(());
                return;
            };
            // 注册与创建必须使用同一个 hinstance（审计报告 S-02）。
            let hinstance = HINSTANCE(module.0);

            let wc = WNDCLASSW {
                lpfnWndProc: Some(clipboard_owner_proc),
                hInstance: hinstance,
                lpszClassName: class_name,
                ..Default::default()
            };

            // 审计报告 S-02：显式处理 `RegisterClassW` 返回值（0 表示失败）。
            // 本函数可能被重复调用，仅当 `GetLastError == ERROR_CLASS_ALREADY_EXISTS`
            // 时才算「已有可用类」；其他错误一律视为失败并中止，不得继续 CreateWindowExW。
            if RegisterClassW(&wc) == 0 {
                let err = GetLastError();
                if err != ERROR_CLASS_ALREADY_EXISTS {
                    crate::dialog::detector::log_debug(&format!(
                        "CLIPBOARD_OWNER: RegisterClassW 失败，err={}，owner 创建中止",
                        err.0
                    ));
                    let _ = ready_tx.send(0);
                    let _ = done_tx.send(());
                    return;
                }
            }

            // 审计报告 P2-4.1 检查点 1：注册窗口类之后命中取消则干净退出，不得创建窗口。
            if thread_cancel.load(Ordering::SeqCst) {
                let _ = done_tx.send(());
                return;
            }
            #[cfg(test)]
            if let Some(hooks) = test_hooks.as_ref() {
                hooks.rendezvous(OwnerCreationStage::AfterClassRegistered, None);
            }

            // 审计报告 P2-4.1 检查点 2：创建窗口之前再检查一次（两检查点之间可能被置位）。
            if thread_cancel.load(Ordering::SeqCst) {
                let _ = done_tx.send(());
                return;
            }

            let created = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("QuickPath Clipboard Owner"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                // 审计报告 S-02：必须与 RegisterClassW 写入 WNDCLASSW.hInstance
                // 的实例句柄完全一致，否则进程本地窗口类查找可能失败。
                Some(hinstance),
                None,
            );

            match created {
                Ok(hwnd) if !hwnd.0.is_null() => {
                    let hwnd_raw = hwnd.0 as usize;

                    // 审计报告 P0-04 竞态 B：在窗口就绪前注册剪贴板监听，使事务能从
                    // 「登记 ready → 任何写路径拿到 owner」之前就具备完整的更新事件链。
                    if let Err(err) = AddClipboardFormatListener(hwnd) {
                        crate::dialog::detector::log_debug(&format!(
                            "CLIPBOARD_OWNER: AddClipboardFormatListener 失败，err={:?}，\
                             剪贴板更新来源链无法证明（事务将保守放弃认领管理器写入）",
                            err
                        ));
                    }

                    #[cfg(test)]
                    if let Some(hooks) = test_hooks.as_ref() {
                        hooks.rendezvous(OwnerCreationStage::AfterWindowCreated, Some(hwnd_raw));
                    }

                    let _ = ready_tx.send(hwnd_raw);

                    // 审计报告 P2-4.1 检查点 3：进入消息泵之前检查取消。窗口已创建时
                    // 走既有正常关闭协议（DestroyWindow → WM_DESTROY → PostQuitMessage），
                    // 绝不进入消息泵，避免留下未登记、无人可关的无主窗口。
                    if thread_cancel.load(Ordering::SeqCst) {
                        let _ = DestroyWindow(hwnd);
                        // 沿用 hwnd_raw 比对后再清空的既有模式，避免误删其他代数的状态。
                        {
                            let mut guard = OWNER
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if guard.as_ref().map(|owner| owner.hwnd_raw) == Some(hwnd_raw) {
                                *guard = None;
                            }
                        }
                        let _ = done_tx.send(());
                        return;
                    }

                    // 审计报告 R-06：显式退出协议的消息泵。`GetMessageW` 返回 0
                    // （收到 `WM_QUIT`）或 -1（错误）都必须退出循环，绝不因 -1 被
                    // `as_bool()` 当作 true 而死循环。
                    let mut msg = MSG::default();
                    loop {
                        let ret = GetMessageW(&mut msg, None, 0, 0);
                        if ret.0 == 0 || ret.0 == -1 {
                            if ret.0 == -1 {
                                crate::dialog::detector::log_debug(&format!(
                                    "CLIPBOARD_OWNER: GetMessageW 返回 -1（err={}），消息泵退出",
                                    GetLastError().0
                                ));
                            }
                            break;
                        }
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }

                    // 线程退出前清空全局状态，杜绝「窗口已销毁但句柄仍被当作有效 owner」
                    // 的窗口期（正常关闭路径中 `shutdown_clipboard_owner` 已先取出状态）。
                    {
                        let mut guard = OWNER
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if guard.as_ref().map(|owner| owner.hwnd_raw) == Some(hwnd_raw) {
                            *guard = None;
                        }
                    }
                    let _ = done_tx.send(());
                }
                _ => {
                    let _ = ready_tx.send(0);
                    let _ = done_tx.send(());
                }
            }
        });

    let thread = match spawned {
        Ok(handle) => handle,
        Err(err) => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_OWNER: 线程创建失败，err={}，owner 不可用",
                err
            ));
            return None;
        }
    };

    #[cfg(test)]
    let ready_timeout = TEST_OWNER_HOOKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|hooks| hooks.ready_timeout)
        .unwrap_or(OWNER_READY_TIMEOUT);
    #[cfg(not(test))]
    let ready_timeout = OWNER_READY_TIMEOUT;

    let hwnd_raw = match ready_rx.recv_timeout(ready_timeout) {
        Ok(raw) => raw,
        Err(err) => {
            // 审计报告 P2-4.1：超时不再直接丢弃 `JoinHandle`（detach）。先置共享取消
            // 标记，令尚未越过检查点的线程干净退出；再接住可能已入队的「迟到 ready」
            // 按已创建窗口回收；最后有界确认线程退出，杜绝未登记线程 / 无主窗口窗口期。
            cancel.store(true, Ordering::SeqCst);
            #[cfg(test)]
            if let Some(hooks) = TEST_OWNER_HOOKS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
            {
                // 通知测试：取消标记已置位（后续会合时序由用例控制）。
                let _ = hooks.cancel_tx.send(());
            }

            // 迟到 ready：线程可能已越过检查点并创建了窗口，非阻塞接住已入队的就绪
            // 消息，按既有正常关闭协议回收（`WM_CLOSE` + `WM_QUIT` 兜底）。
            if let Ok(late_raw) = ready_rx.try_recv()
                && late_raw != 0
            {
                let late_hwnd = HWND(late_raw as *mut _);
                if let Err(post_err) =
                    unsafe { PostMessageW(Some(late_hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }
                {
                    crate::dialog::detector::log_debug(&format!(
                        "CLIPBOARD_OWNER: 超时取消时 PostMessageW(WM_CLOSE) 失败，err={:?}",
                        post_err
                    ));
                }
                let late_tid = unsafe { GetWindowThreadProcessId(late_hwnd, None) };
                if late_tid != 0
                    && let Err(post_err) =
                        unsafe { PostThreadMessageW(late_tid, WM_QUIT, WPARAM(0), LPARAM(0)) }
                {
                    crate::dialog::detector::log_debug(&format!(
                        "CLIPBOARD_OWNER: 超时取消时 PostThreadMessageW(WM_QUIT) 失败，err={:?}",
                        post_err
                    ));
                }
            }

            // 有界确认：确认线程已在超时内退出（能 join 就 join），而不是静默 detach。
            match done_rx.recv_timeout(OWNER_SHUTDOWN_TIMEOUT) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = thread.join();
                    crate::dialog::detector::log_debug(&format!(
                        "CLIPBOARD_OWNER: owner 就绪等待超时（{}），已请求取消并确认线程退出",
                        err
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    crate::dialog::detector::log_debug(&format!(
                        "CLIPBOARD_OWNER_TIMEOUT_CANCEL: owner 就绪等待超时（{}）后请求取消，但未在 {:?} 内确认线程退出，残余风险：可能存在未登记线程或窗口",
                        err, OWNER_SHUTDOWN_TIMEOUT
                    ));
                }
            }
            return None;
        }
    };

    if hwnd_raw == 0 {
        return None;
    }

    let hwnd = HWND(hwnd_raw as *mut _);
    let thread_id = unsafe { GetWindowThreadProcessId(hwnd, None) };
    {
        let mut guard = OWNER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(ClipboardOwnerHandle {
            hwnd_raw,
            thread_id,
            generation,
            done_rx,
            thread: Some(thread),
        });
    }

    Some(hwnd)
}

/// 显式关闭进程级剪贴板 owner：投递退出消息 → 有界确认线程退出 → `join`。
///
/// 幂等：没有 owner 时直接返回 `true`（no-op 成功）。返回值 `true` 表示 owner 线程已在
/// 有界超时内确认退出（或本就无 owner），`false` 表示确认超时、已放弃 `join`（detach），
/// 宿主线程绝不被永久阻塞。
pub(crate) fn shutdown_clipboard_owner() -> bool {
    // 先把拥有式状态取出锁外，避免在 `join` 期间长时间持有 `Mutex`。
    let handle = {
        let mut guard = OWNER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.take()
    };

    let Some(mut handle) = handle else {
        return true;
    };

    // 1. 首选 `WM_CLOSE` → 窗口过程内 `DestroyWindow` → `WM_DESTROY` → `PostQuitMessage`。
    if handle.hwnd_raw != 0 {
        let hwnd = HWND(handle.hwnd_raw as *mut _);
        if let Err(err) = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) } {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_OWNER_SHUTDOWN: PostMessageW(WM_CLOSE) 投递失败，err={:?}，继续走兜底",
                err
            ));
        }
    }

    // 2. `WM_QUIT` 兜底：即便窗口过程未按预期销毁窗口，消息泵也能直接退出。
    if handle.thread_id != 0
        && let Err(err) =
            unsafe { PostThreadMessageW(handle.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
    {
        crate::dialog::detector::log_debug(&format!(
            "CLIPBOARD_OWNER_SHUTDOWN: PostThreadMessageW(WM_QUIT) 投递失败，err={:?}",
            err
        ));
    }

    // 3. 有界确认：`JoinHandle` 没有超时 join，先用确认通道确认线程已退出，再 `join`。
    match handle.done_rx.recv_timeout(OWNER_SHUTDOWN_TIMEOUT) {
        // `Disconnected` 表示线程已结束但发送端先被丢弃，同样视为已退出。
        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            if let Some(thread) = handle.thread.take() {
                let _ = thread.join();
            }
            true
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_OWNER_SHUTDOWN_TIMEOUT: owner 线程（generation={}）未在 {:?} 内退出，放弃 join（detach）",
                handle.generation, OWNER_SHUTDOWN_TIMEOUT
            ));
            // 丢弃 `JoinHandle` 即 detach：宿主线程继续运行，绝不永久阻塞。
            handle.thread.take();
            false
        }
    }
}

/// 以合法 owner 打开剪贴板（**所有写路径必须先经过这里**）。
/// owner 无法建立时一律返回 false，调用方按“写入失败”处理（fail-closed）。
fn open_clipboard_for_write(max_retries: u32) -> bool {
    match clipboard_owner() {
        Some(owner) => open_clipboard_with_retry(Some(owner), max_retries),
        None => false,
    }
}

/// 以 owner（若可用）打开剪贴板用于只读捕获；owner 不可用时退回 `NULL`（只读不受影响）。
fn open_clipboard_for_read(max_retries: u32) -> bool {
    match clipboard_owner() {
        Some(owner) => open_clipboard_with_retry(Some(owner), max_retries),
        None => open_clipboard_with_retry(None, max_retries),
    }
}

/// 剪贴板快照数据，存储全量枚举格式的原始内存块副本
///
/// 审计需求 `9549648` §3.4.1：快照必须与**捕获时刻的 `capture_sequence`** 绑定，
/// 否则「捕获 → 清空/写入」与「检查 → 恢复」两处都会出现 check-then-act 窗口
/// （用户在这两个窗口内复制的新内容会被旧快照静默覆盖）。
pub struct ClipboardSnapshot {
    /// 各格式数据 (format_id, raw_bytes)
    pub items: Vec<(u32, Vec<u8>)>,
    /// 是否在备份时原本就为空
    pub was_empty: bool,
    /// 是否已完成还原（仅当全部格式都成功写回才置位）
    pub restored: bool,
    /// 已成功写回的格式集合：重试时只补写缺失项，避免反复清空导致已恢复数据丢失
    restored_formats: Vec<u32>,
    /// 是否已执行过一次 EmptyClipboard
    emptied: bool,
    /// 是否已**永久放弃**该快照（外部改写 / 归属不可判定）。一旦放弃不再做任何补救，
    /// `Drop` 也不得再补写（§3.4.3 第 4 条）。
    abandoned: bool,
    /// 捕获时（持锁期内）读到的剪贴板序列号；0 表示不可判定（不得自动恢复）
    capture_sequence: u32,
    /// 该快照当前在剪贴板上对应的序列号：初值为 `capture_sequence`，
    /// 本快照每次成功清空/写入后更新为本次写入产生的新序列号。
    /// 所有清空/写入都必须在持锁期内核对该值（§3.4.2）。
    expected_sequence: u32,
    /// 最近一次由本快照写入的精确文本（仅供诊断定位，绝不记入日志）
    owned_text: Option<String>,
}

impl ClipboardSnapshot {
    /// 完整捕获当前系统剪贴板的所有格式数据
    /// 若剪贴板被占用或关键格式无法保真备份，返回 Err 坚决不碰剪贴板
    pub fn capture() -> Result<Self, ClipboardSafetyError> {
        if !open_clipboard_for_read(4) {
            return Err(ClipboardSafetyError::Busy);
        }

        // 持锁期内读取序列号：此刻无人能修改剪贴板，该值就是**本次捕获内容**的序列号。
        // （注意 `GetClipboardSequenceNumber` 在 `CloseClipboard` 之后才自增，
        //  所以持锁期内读到的正是“已存在内容”的序列号。）
        let capture_sequence = clipboard_sequence_number();

        let mut items = Vec::new();
        let mut total_bytes = 0usize;
        let was_empty;

        unsafe {
            let count = CountClipboardFormats();
            if count == 0 {
                was_empty = true;
                let _ = CloseClipboard();
                return Ok(Self::empty(items, was_empty, capture_sequence));
            }

            was_empty = false;
            let mut present_formats = Vec::new();
            let mut fmt = 0u32;
            loop {
                fmt = EnumClipboardFormats(fmt);
                if fmt == 0 {
                    break;
                }
                present_formats.push(fmt);
            }

            let has_hdrop = present_formats.contains(&CF_HDROP);
            let has_unicode = present_formats.contains(&CF_UNICODETEXT);
            let has_bitmap = present_formats.contains(&CF_BITMAP);
            let has_dib = present_formats.contains(&CF_DIB) || present_formats.contains(&CF_DIBV5);

            let mut hdrop_captured = false;
            let mut unicode_captured = false;
            let mut dib_captured = false;

            // 1. 若剪贴板包含位图 (CF_BITMAP 或 CF_DIB)，Windows 会自动合成 CF_DIB (HGLOBAL 格式)。
            // 优先通过标准内存格式 CF_DIB 捕获完整位图数据，绝不要对 CF_BITMAP 的 HBITMAP 句柄调用 GlobalLock！
            if (has_bitmap || has_dib)
                && !items.iter().any(|(f, _)| *f == CF_DIB)
                && let Ok(h) = GetClipboardData(CF_DIB)
                && !h.0.is_null()
            {
                let size = GlobalSize(HGLOBAL(h.0));
                if size > 0 {
                    if size > MAX_ITEM_BYTES || total_bytes + size > MAX_TOTAL_BYTES {
                        let _ = CloseClipboard();
                        return Err(ClipboardSafetyError::BackupIncomplete(
                            "剪贴板位图内容过大，超过安全保护上限".to_string(),
                        ));
                    }
                    let ptr = GlobalLock(HGLOBAL(h.0));
                    if !ptr.is_null() {
                        let mut data = vec![0u8; size];
                        std::ptr::copy_nonoverlapping(ptr as *const u8, data.as_mut_ptr(), size);
                        let _ = GlobalUnlock(HGLOBAL(h.0));
                        total_bytes += size;
                        items.push((CF_DIB, data));
                        dib_captured = true;
                    }
                }
            }

            // 2. 遍历枚举到的全部剪贴板格式
            for &f in &present_formats {
                // 如果已成功捕获 CF_DIB，则由 Windows 自动合成的 CF_BITMAP、CF_DIB、CF_DIBV5 可安全复用
                if dib_captured && (f == CF_BITMAP || f == CF_DIB || f == CF_DIBV5) {
                    continue;
                }

                // 防范非内存句柄：GDI与图元文件句柄绝不能调用 GlobalLock
                if is_gdi_or_metafile_format(f) {
                    let _ = CloseClipboard();
                    return Err(ClipboardSafetyError::BackupIncomplete(format!(
                        "检测到无法通过标准内存克隆的 GDI/图元文件格式 (ID: {})，Fail-Closed 放弃操作以保护用户数据",
                        f
                    )));
                }

                // 读取当前格式的数据句柄 (保证均为 HGLOBAL 格式)
                let mut captured_this_format = false;
                if let Ok(h) = GetClipboardData(f)
                    && !h.0.is_null()
                {
                    let size = GlobalSize(HGLOBAL(h.0));
                    if size > 0 {
                        if size > MAX_ITEM_BYTES || total_bytes + size > MAX_TOTAL_BYTES {
                            let _ = CloseClipboard();
                            return Err(ClipboardSafetyError::BackupIncomplete(
                                "剪贴板内容过大，超过安全保护上限".to_string(),
                            ));
                        }

                        let ptr = GlobalLock(HGLOBAL(h.0));
                        if !ptr.is_null() {
                            let mut data = vec![0u8; size];
                            std::ptr::copy_nonoverlapping(
                                ptr as *const u8,
                                data.as_mut_ptr(),
                                size,
                            );
                            let _ = GlobalUnlock(HGLOBAL(h.0));

                            if f == CF_HDROP {
                                hdrop_captured = true;
                            }
                            if f == CF_UNICODETEXT {
                                unicode_captured = true;
                            }

                            total_bytes += size;
                            items.push((f, data));
                            captured_this_format = true;
                        }
                    }
                }

                // 3. 【Fail-Closed 核心防御】：如果当前格式未成功捕获，检查是否为已被源格式覆盖的系统合成格式
                if !captured_this_format {
                    let is_synthesized_covered = (unicode_captured
                        && (f == CF_TEXT || f == CF_OEMTEXT))
                        || (dib_captured && (f == CF_BITMAP || f == CF_DIBV5));

                    if !is_synthesized_covered {
                        // 检测到无法保真复制的非内存或专有格式，为尽可能完整保留用户数据，坚决拒绝改写
                        let _ = CloseClipboard();
                        return Err(ClipboardSafetyError::BackupIncomplete(format!(
                            "检测到不可保真克隆的剪贴板格式 (ID: {})，Fail-Closed 放弃操作以保护用户数据",
                            f
                        )));
                    }
                }
            }

            let _ = CloseClipboard();

            // 4. 关键类型保真终审校验
            if has_hdrop && !hdrop_captured {
                return Err(ClipboardSafetyError::BackupIncomplete(
                    "检测到文件列表(CF_HDROP)但无法保真复制".to_string(),
                ));
            }
            if has_unicode && !unicode_captured {
                return Err(ClipboardSafetyError::BackupIncomplete(
                    "检测到文本(CF_UNICODETEXT)但无法保真复制".to_string(),
                ));
            }
            if (has_bitmap || has_dib) && !dib_captured {
                return Err(ClipboardSafetyError::BackupIncomplete(
                    "检测到位图图像(CF_BITMAP/CF_DIB)但无法保真复制".to_string(),
                ));
            }
        }

        Ok(Self::empty(items, was_empty, capture_sequence))
    }

    fn empty(items: Vec<(u32, Vec<u8>)>, was_empty: bool, capture_sequence: u32) -> Self {
        Self {
            items,
            was_empty,
            restored: false,
            restored_formats: Vec::new(),
            emptied: false,
            abandoned: false,
            capture_sequence,
            expected_sequence: capture_sequence,
            owned_text: None,
        }
    }

    /// 捕获时刻（持锁期内）的剪贴板序列号；0 表示不可判定。
    pub fn capture_sequence(&self) -> u32 {
        self.capture_sequence
    }

    /// 把快照绑定到「本进程刚刚写入剪贴板的那个序列号」。
    ///
    /// 每次成功清空/写入后都要重新绑定，否则下次恢复会拿过期的期望值去核对。
    pub fn bind_owned_write(&mut self, text: &str, sequence: u32) {
        self.expected_sequence = sequence;
        self.owned_text = Some(text.to_string());
    }
    /// 尚未成功写回的格式 ID 列表
    fn pending_formats(&self) -> Vec<u32> {
        self.items
            .iter()
            .map(|(fmt, _)| *fmt)
            .filter(|fmt| !self.restored_formats.contains(fmt))
            .collect()
    }

    /// 还原剪贴板至快照状态。
    ///
    /// 语义保证（审计需求 `9549648` §3.4.2 / §3.4.3）：
    /// 1. 首次恢复与**每一次重试**都走统一原子原语：在**同一次 `OpenClipboard` 持锁期**
    ///    内核对该轮期望的 `sequence`，不一致就零修改退出（消除 check-then-act 窗口）；
    /// 2. 部分格式失败时，记录本次写入产生的新 `sequence`，下一次重试重新持锁核对；
    /// 3. 序列号不一致 / owner 不匹配 / 状态不明 → **永久放弃**该快照，不清空、不写入，
    ///    `Drop` 也不得再补写；
    /// 4. 只有全部格式成功才返回 `Ok(Restored)` 并置 `restored = true`。
    pub fn restore(&mut self) -> Result<RestoreStatus, ClipboardRestoreError> {
        if self.restored {
            return Ok(RestoreStatus::AlreadyRestored);
        }
        if self.abandoned {
            return Ok(RestoreStatus::Abandoned);
        }
        if self.expected_sequence == 0 {
            // 归属不可判定：绝不用旧快照覆盖当前内容
            self.abandoned = true;
            self.restored = true;
            return Ok(RestoreStatus::Abandoned);
        }

        let mut expected = self.expected_sequence;
        let mut last_error = None;
        for attempt in 0..RESTORE_ATTEMPTS {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(30 * u64::from(attempt)));
            }

            let pending = self.pending_formats();
            if self.emptied && pending.is_empty() {
                self.restored = true;
                return Ok(RestoreStatus::Restored);
            }
            let empty_first = !self.emptied;
            let items = &self.items;
            let result = clipboard_atomic_write(expected, empty_first, || {
                let mut written = 0usize;
                let mut failed = Vec::new();
                for fmt in &pending {
                    match items.iter().find(|(f, _)| f == fmt) {
                        Some((_, data)) => {
                            if write_clipboard_format(*fmt, data) {
                                written += 1;
                            } else {
                                failed.push(*fmt);
                            }
                        }
                        // 理论上不可达（pending 由 items 派生）：保守计为失败
                        None => failed.push(*fmt),
                    }
                }
                (written, failed)
            });

            match result {
                Ok((sequence, _written, failed)) => {
                    expected = sequence;
                    self.expected_sequence = sequence;
                    self.emptied = true;
                    for fmt in &pending {
                        if !failed.contains(fmt) {
                            self.restored_formats.push(*fmt);
                        }
                    }
                    if failed.is_empty() {
                        self.restored = true;
                        return Ok(RestoreStatus::Restored);
                    }
                    last_error = Some(ClipboardRestoreError::PartialRestore(failed));
                }
                Err(AtomicWriteError::NotOwned) | Err(AtomicWriteError::OwnershipUnknown) => {
                    // 永久放弃：不清空、不写入，Drop 也不得再补写
                    self.abandoned = true;
                    self.restored = true;
                    return Ok(RestoreStatus::Abandoned);
                }
                Err(AtomicWriteError::Busy) => last_error = Some(ClipboardRestoreError::Busy),
                Err(AtomicWriteError::EmptyFailed) => {
                    last_error = Some(ClipboardRestoreError::EmptyFailed)
                }
            }
        }

        Err(last_error.unwrap_or(ClipboardRestoreError::Busy))
    }
}

/// 单格式写回所需的底层句柄操作。
///
/// 生产环境为真实 Win32 调用；测试可替换其中任一项注入故障，
/// 从而覆盖「分配失败 / 加锁失败 / 设置失败」三条分支的**句柄回收路径**
///（报告 P0-01 明确要求补充这三类故障注入）。
#[derive(Clone, Copy)]
struct HandleOps {
    alloc: fn(usize) -> Option<HGLOBAL>,
    lock: fn(HGLOBAL) -> *mut u8,
    unlock: fn(HGLOBAL),
    free: fn(HGLOBAL),
    set: fn(u32, HGLOBAL) -> bool,
}

const PROD_HANDLE_OPS: HandleOps = HandleOps {
    alloc: prod_alloc,
    lock: prod_lock,
    unlock: prod_unlock,
    free: prod_free,
    set: prod_set,
};

fn prod_alloc(len: usize) -> Option<HGLOBAL> {
    unsafe { GlobalAlloc(GMEM_MOVEABLE, len).ok() }
}

fn prod_lock(handle: HGLOBAL) -> *mut u8 {
    unsafe { GlobalLock(handle) as *mut u8 }
}

fn prod_unlock(handle: HGLOBAL) {
    unsafe {
        let _ = GlobalUnlock(handle);
    }
}

fn prod_free(handle: HGLOBAL) {
    unsafe {
        let _ = GlobalFree(Some(handle));
    }
}

fn prod_set(format: u32, handle: HGLOBAL) -> bool {
    unsafe { SetClipboardData(format, Some(HANDLE(handle.0))).is_ok() }
}

/// 将单格式数据写回剪贴板，全程检查句柄返回值。
///
/// 剪贴板数据句柄的所有权只在 `SetClipboardData` 成功那一刻交接给系统；
/// 此前任一环节失败都必须 `GlobalFree`，否则造成内存泄漏。
fn write_clipboard_format(format: u32, data: &[u8]) -> bool {
    #[cfg(test)]
    if TEST_FAIL_FORMAT.load(std::sync::atomic::Ordering::SeqCst) == format {
        // 端到端用例沿用的“该格式写回必失败”注入：按 `TEST_FAIL_MODE` 选定
        // 分配 / 加锁 / 设置哪一环失败。三条分支都必须返回失败，
        // 绝不允许“失败仍上报成功”。
        let ops = match TEST_FAIL_MODE.load(std::sync::atomic::Ordering::SeqCst) {
            1 => HandleOps {
                alloc: |_| None,
                ..PROD_HANDLE_OPS
            },
            2 => HandleOps {
                lock: |_| std::ptr::null_mut(),
                ..PROD_HANDLE_OPS
            },
            _ => HandleOps {
                set: |_, _| false,
                ..PROD_HANDLE_OPS
            },
        };
        return write_clipboard_format_with(format, data, &ops);
    }

    write_clipboard_format_with(format, data, &PROD_HANDLE_OPS)
}

fn write_clipboard_format_with(format: u32, data: &[u8], ops: &HandleOps) -> bool {
    if data.is_empty() {
        // 空数据无内容可写（也就没有可交接的句柄）
        return true;
    }

    let Some(handle) = (ops.alloc)(data.len()) else {
        return false;
    };
    if handle.0.is_null() {
        return false;
    }

    let ptr = (ops.lock)(handle);
    if ptr.is_null() {
        // 加锁失败：句柄尚未交接给系统，必须自行释放
        (ops.free)(handle);
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
    }
    (ops.unlock)(handle);

    if !(ops.set)(format, handle) {
        // 设置失败：句柄所有权同样尚未交接，必须自行释放
        (ops.free)(handle);
        return false;
    }
    true
}

/// 剪贴板「临时占用事务」的最终结局（结构化状态，供调用方观测与记日志）。
///
/// 命名对齐审计需求 `9549648` §4.2，**只记录结构化状态，绝不携带剪贴板正文**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardTransactionOutcome {
    /// 用户原剪贴板内容已完整恢复（正常提交）
    Committed,
    /// 检出外部改写（用户/第三方已复制新内容），主动放弃恢复以保护其新内容
    SkippedForeignChange,
    /// 剪贴板归属不可判定（序列号不可读 / 读回失败 / 剪贴板忙），保守放弃恢复
    SkippedUnknownOwner,
    /// 请求窗口内的更新历史不可证明（事件缺失 / 事件数不为 1 / 事件环溢出 / 静默超时）——
    /// 这是 P0-04-B 的 fail-closed 终止态：**绝不认领**不可证明的第三方写入。
    AbortedEventHistoryUnprovable,
    /// 恢复过程中检出外部改写 / owner 不匹配：**永久放弃**该快照（§3.4.3）
    RestoreAbandoned,
    /// 归属校验通过，但恢复动作本身失败（携带底层错误）
    RestorePartialFailure(ClipboardRestoreError),
}

/// 事务操作失败原因：任一 `Err` 都要求调用方立即停止后续写入（保守退出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardTransactionError {
    /// 归属已丢失：已检出外部改写，绝不能继续写入或恢复
    NotOwned,
    /// 归属无法判定（序列号不可读 / 剪贴板忙），保守停止
    OwnershipUnknown,
    /// 临时文本写入失败
    WriteFailed,
}

impl std::fmt::Display for ClipboardTransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOwned => write!(f, "剪贴板已被外部改写，归属已丢失"),
            Self::OwnershipUnknown => write!(f, "剪贴板归属无法判定"),
            Self::WriteFailed => write!(f, "临时写入剪贴板文本失败"),
        }
    }
}

impl std::error::Error for ClipboardTransactionError {}

/// 事务内部对「剪贴板是否仍归本次临时占用所有」的判定结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnershipState {
    /// 仍归本事务所有（序列号未变）
    Owned,
    /// 已被外部改写（序列号已变）
    Lost,
    /// 无法判定（序列号不可读 / 从未取得有效基准）
    Unknown,
}

/// 剪贴板「临时占用事务」：把「快照 → 清空/写入 → 请求第三方写入 → 读回 → 恢复」
/// 收口成**单一事务 API**（审计需求 `9549648` §4.1）。所有调用方都不得手工拼装
/// capture → check → write → restore 流程。
///
/// 语义：
/// - 快照与 `capture_sequence` 绑定，首次写入在同一次持锁期内复核该序列号；
/// - 归属判定严格按 `GetClipboardSequenceNumber`，序列号变化即视为外部改写，
///   **不存在**「文本相等仍视为持有」的 fail-open 回退（§3.4.4）；
/// - 归属不可判定或事件史不可证明时保守放弃恢复，绝不用旧快照覆盖当前内容；
/// - 每次写入前重新校验归属，检出外部改写立即返回 `Err` 让调用方停止写入；
/// - `Drop` 兜底走同一套归属校验，绝不因提前 return 漏恢复或错误覆盖。
pub struct ClipboardTransaction {
    /// 用户原始剪贴板快照；`take` 后为 `None`（表示已消费于恢复动作）
    snapshot: Option<ClipboardSnapshot>,
    /// 最近一次由本事务认可（自身写入 / 请求第三方写入后读回）的剪贴板序列号
    owned_seq: Option<u32>,
    /// 最近一次认可写入的精确文本（仅用于诊断定位，不参与严格归属判定，绝不记入日志）
    owned_text: Option<String>,
    /// 是否已因外部改写而失去所有权（一旦置位不再改变判定）
    lost_ownership: bool,
    /// 是否因序列号/读回不可用导致归属无法判定（一旦置位即保守放弃恢复）
    ownership_unknown: bool,
    /// 是否因请求窗口内的事件史不可证明而终止认领（P0-04-B fail-closed）
    event_history_unprovable: bool,
    /// 是否已进入「永久放弃快照」状态（恢复检出外部改写 / owner 不匹配）
    abandoned_snapshot: bool,
    /// 最终结局；一旦确定即幂等返回，`Drop` 不会再重复动作
    outcome: Option<ClipboardTransactionOutcome>,
}

impl ClipboardTransaction {
    /// 开启事务：完整快照当前剪贴板（含与 `capture_sequence` 的绑定）。
    /// 快照失败（`Busy` / `BackupIncomplete`）直接返回 `Err`，绝不触碰剪贴板内容。
    pub fn begin() -> Result<Self, ClipboardSafetyError> {
        let snapshot = ClipboardSnapshot::capture()?;
        // 期望序列号直接取自**持锁捕获期**读到的值，不再另读一次（否则“捕获”与
        // “记录基准”之间又会多出一个 check-then-act 窗口）。
        let capture_sequence = snapshot.capture_sequence();
        Ok(Self {
            snapshot: Some(snapshot),
            // 序列号不可读（返回 0）时不建立基准，后续写入一律 fail-closed
            owned_seq: (capture_sequence != 0).then_some(capture_sequence),
            owned_text: None,
            lost_ownership: false,
            ownership_unknown: capture_sequence == 0,
            event_history_unprovable: false,
            abandoned_snapshot: false,
            outcome: None,
        })
    }

    /// 刷新归属判定：把「未知/丢失」的判定结果固化到内部状态，便于幂等复用。
    fn refresh_ownership(&mut self) -> OwnershipState {
        if self.lost_ownership {
            return OwnershipState::Lost;
        }
        if self.ownership_unknown {
            return OwnershipState::Unknown;
        }
        let Some(seq) = self.owned_seq else {
            self.ownership_unknown = true;
            return OwnershipState::Unknown;
        };
        let current = clipboard_sequence_number();
        if current == 0 {
            // 归属未知：绝不恢复，避免用旧快照覆盖用户当前内容
            self.ownership_unknown = true;
            return OwnershipState::Unknown;
        }
        if current == seq {
            return OwnershipState::Owned;
        }
        self.lost_ownership = true;
        OwnershipState::Lost
    }

    /// 写入前归属校验：第二次清空及任何后续写入前必须调用。
    /// 检出外部改写或归属无法判定时返回 `Err`，调用方必须立即停止后续写入。
    pub fn ensure_still_owned(&mut self) -> Result<(), ClipboardTransactionError> {
        match self.refresh_ownership() {
            OwnershipState::Owned => Ok(()),
            OwnershipState::Lost => Err(ClipboardTransactionError::NotOwned),
            OwnershipState::Unknown => Err(ClipboardTransactionError::OwnershipUnknown),
        }
    }

    /// 由 `QuickPath` 自己向剪贴板写入临时文本（如清空），并登记序列号与精确文本。
    ///
    /// 审计报告 P0-04 竞态 A：原实现是「先 `ensure_still_owned()`、后 `OpenClipboard`」
    /// 的 check-then-act，两步之间用户复制会被随后的 `EmptyClipboard` 静默清掉。
    /// 现在复核与写入收口到 `set_clipboard_text_checked()` 的**同一次剪贴板持锁期**内：
    /// Windows 同一时刻只允许一个窗口打开剪贴板，因此持锁复核通过后到写入完成之间
    /// 不可能再发生第三方写入。任何序列号不一致或不可读都 fail-closed（不写入）。
    pub fn write_owned_text(&mut self, text: &str) -> Result<(), ClipboardTransactionError> {
        self.ensure_still_owned()?;

        // 确定性测试注入点：在「写前校验」与「持锁复核」之间制造一次外部改写，
        // 验证持锁复核会 fail-closed（注入动作由统一原语开头执行，仅测试构建存在）。
        let Some(expected) = self.owned_seq else {
            self.ownership_unknown = true;
            return Err(ClipboardTransactionError::OwnershipUnknown);
        };

        match set_clipboard_text_checked(text, expected) {
            Ok(seq) => {
                self.owned_seq = Some(seq);
                self.owned_text = Some(text.to_string());
                Ok(())
            }
            Err(ClipboardTransactionError::NotOwned) => {
                self.lost_ownership = true;
                Err(ClipboardTransactionError::NotOwned)
            }
            Err(ClipboardTransactionError::OwnershipUnknown) => {
                self.ownership_unknown = true;
                Err(ClipboardTransactionError::OwnershipUnknown)
            }
            Err(err) => {
                // 写入失败可能已清空剪贴板（归属未知），保守放弃恢复
                self.ownership_unknown = true;
                Err(err)
            }
        }
    }

    /// 取得本次「管理器请求」的开始标记：之后登记的全部剪贴板更新都属于该请求窗口。
    /// 调用方应在发起请求**之前**调用，并把返回值交给
    /// [`Self::note_observed_owned_text_proven`]。
    pub fn request_marker(&self) -> u64 {
        clipboard_update_marker()
    }

    /// 排空「本次临时写入」触发的全部 `WM_CLIPBOARDUPDATE` 通知。
    ///
    /// **必须在 `request_marker()` 之前调用**：请求窗口的来源判据是「窗口内恰好一次
    /// 可归因更新」；若本事务自身那次写入的通知迟到并落进请求窗口，就会把合法扫描
    /// 误判为不可证明（fail-closed 的误伤）。等待有界，超时即判为归属不可证明。
    pub fn await_own_write_observed(&self) -> Result<(), ClipboardTransactionError> {
        let Some(sequence) = self.owned_seq else {
            return Err(ClipboardTransactionError::OwnershipUnknown);
        };
        if sequence == 0 || !clipboard_update_log_quiesced(sequence) {
            return Err(ClipboardTransactionError::OwnershipUnknown);
        }
        Ok(())
    }

    /// 请求管理器写入路径后调用：只有在**能证明**本次剪贴板更新来自目标管理器时，
    /// 才读回文本并登记当时的序列号作为新的归属基准（审计报告 P0-04 竞态 B）。
    ///
    /// 证明规则（任一不满足即不认领）：
    /// 1. 序列号未变 → 内容仍是本事务写入的临时文本，归属未变（返回 `Ok(None)`）；
    /// 2. 序列号已变 → 事件链必须完整（未溢出环、已追平当前序列号），
    ///    且 `marker` 之后的每一次更新都必须被 `is_expected_source` 认可，
    ///    并且当前 `GetClipboardOwner()` 也属于目标来源。
    ///
    /// 一旦出现无法归因到目标管理器的更新，立即判为外部改写（`Err(NotOwned)`），
    /// 绝不把最终状态无条件认领为本事务所有。
    pub fn note_observed_owned_text_proven<A>(
        &mut self,
        marker: u64,
        is_expected_source: A,
    ) -> Result<Option<String>, ClipboardTransactionError>
    where
        A: Fn(u32) -> bool,
    {
        let before = self.owned_seq.unwrap_or(0);
        let current = clipboard_sequence_number();
        if current == 0 {
            self.ownership_unknown = true;
            return Err(ClipboardTransactionError::OwnershipUnknown);
        }

        let own_pid = std::process::id();
        // 本进程只会因本事务的临时写入（`write_owned_text`）而改动剪贴板，因此本进程来源
        // 的更新永远不算「外部改写」。若不显式放行，本次事务自己那次清空所投递的**延迟**
        // `WM_CLIPBOARDUPDATE` 就会在请求窗口内被误判为外部来源，导致合法扫描被拒。
        let acceptable = |pid: u32| pid == own_pid || is_expected_source(pid);

        // 序列号未变：无写入发生，内容仍是本事务的临时文本，归属未变。
        // （事件环仍做防御性核查：出现非目标来源的更新绝不得放过。）
        if current == before {
            if let Some(events) = clipboard_updates_since(marker) {
                for event in &events {
                    if !acceptable(event.owner_pid) {
                        self.lost_ownership = true;
                        return Err(ClipboardTransactionError::NotOwned);
                    }
                }
            }
        } else {
            // 序列号已变：必须能完整归因到目标管理器。
            // 先等事件链追平当前序列号（`WM_CLIPBOARDUPDATE` 是**投递**消息，登记可能滞后），
            // 再取事件——否则会把「尚未登记」误判为「没有任何事件」。
            if clipboard_updates_since(marker).is_none() {
                self.event_history_unprovable = true;
                return Err(ClipboardTransactionError::OwnershipUnknown);
            }
            if !clipboard_update_log_covers(current) {
                self.event_history_unprovable = true;
                return Err(ClipboardTransactionError::OwnershipUnknown);
            }
            let Some(events) = clipboard_updates_since(marker) else {
                self.event_history_unprovable = true;
                return Err(ClipboardTransactionError::OwnershipUnknown);
            };
            // 任何一次非目标来源的更新：直接判为外部改写。
            for event in &events {
                if !acceptable(event.owner_pid) {
                    self.lost_ownership = true;
                    return Err(ClipboardTransactionError::NotOwned);
                }
            }
            // P0-04-B 核心 fail-closed 规则（审计需求 `9549648` §2.4 方案 B）：
            //
            // `WM_CLIPBOARDUPDATE` 在**处理时**才采样 sequence/owner，采到的是处理时
            // 的当前状态而非「更新发生瞬间」的历史状态。因此事件环**不能**作为完整
            // 来源链的证明；唯一可保守采纳的情形是：请求窗口内恰好只有一次更新、
            // 且该次登记的序列号与当前序列号一致（无滞后、无合并）。
            // 两次更新被延迟处理时会登记两条事件，此时无论如何都不得认领。
            if events.len() != 1 {
                self.event_history_unprovable = true;
                return Err(ClipboardTransactionError::OwnershipUnknown);
            }
            if events[0].sequence != current {
                self.event_history_unprovable = true;
                return Err(ClipboardTransactionError::OwnershipUnknown);
            }
            let final_owner = clipboard_owner_pid();
            if !acceptable(final_owner) {
                self.lost_ownership = true;
                return Err(ClipboardTransactionError::NotOwned);
            }
        }

        // 读回文本与序列号必须在**同一次持锁期**内完成，否则「校验的序列号」与
        // 「读到的文本」可能不是同一份内容（又一个 check-then-act）。
        match get_clipboard_text_checked() {
            Some((text, sequence)) if sequence == current => {
                self.owned_seq = Some(current);
                self.owned_text = Some(text.clone());
                Ok(Some(text))
            }
            Some(_) => {
                self.ownership_unknown = true;
                Err(ClipboardTransactionError::OwnershipUnknown)
            }
            None => {
                self.ownership_unknown = true;
                Err(ClipboardTransactionError::OwnershipUnknown)
            }
        }
    }

    /// 最终恢复：先按严格 sequence 校验归属，校验通过才恢复。
    ///
    /// 结局见 [`ClipboardTransactionOutcome`]。幂等：重复调用返回同一结局。
    pub fn restore_if_still_owned(&mut self) -> ClipboardTransactionOutcome {
        if let Some(outcome) = self.outcome.clone() {
            return outcome;
        }

        let outcome = if self.event_history_unprovable {
            // 事件史不可证明（P0-04-B）：绝不认领，也绝不用旧快照覆盖当前内容
            ClipboardTransactionOutcome::AbortedEventHistoryUnprovable
        } else {
            match self.refresh_ownership() {
                // 被外部改写：绝不 EmptyClipboard、绝不写回旧快照
                OwnershipState::Lost => ClipboardTransactionOutcome::SkippedForeignChange,
                // 归属未知：不得用旧快照覆盖当前内容
                OwnershipState::Unknown => ClipboardTransactionOutcome::SkippedUnknownOwner,
                // 归属未变（含管理器超时/窗口退出：剪贴板仍是本事务的临时写入，
                // 用户原始内容依然安全地被快照保存）→ 正常还回去
                OwnershipState::Owned => self.perform_restore(),
            }
        };
        self.outcome = Some(outcome.clone());
        outcome
    }

    /// 执行真实恢复：把事务认可的最后序列号交给快照绑定，恢复动作内部在持锁期内复核。
    fn perform_restore(&mut self) -> ClipboardTransactionOutcome {
        let owned = self.owned_seq.zip(self.owned_text.clone());
        let Some(snapshot) = self.snapshot.as_mut() else {
            return ClipboardTransactionOutcome::Committed;
        };
        if let Some((seq, text)) = owned {
            snapshot.bind_owned_write(&text, seq);
        }
        match snapshot.restore() {
            Ok(RestoreStatus::Restored | RestoreStatus::AlreadyRestored) => {
                ClipboardTransactionOutcome::Committed
            }
            Ok(RestoreStatus::Abandoned) => {
                // 永久放弃：已由快照置位 abandoned，Drop 不会再补写
                self.abandoned_snapshot = true;
                ClipboardTransactionOutcome::RestoreAbandoned
            }
            Err(err) => ClipboardTransactionOutcome::RestorePartialFailure(err),
        }
    }
}

impl Drop for ClipboardTransaction {
    fn drop(&mut self) {
        // 显式恢复已遭遇「部分失败」：Drop 再补一次（只补写未成功格式，且仍走严格归属
        // 校验，被外部改写时绝不过写）。
        //
        // 注意：`RestoreAbandoned` / `AbortedEventHistoryUnprovable` / `SkippedForeignChange`
        // 等终止态**绝不**在此补写（§3.4.3 第 4 条：放弃后 Drop 不得再次补写）。
        if matches!(
            self.outcome,
            Some(ClipboardTransactionOutcome::RestorePartialFailure(_))
        ) {
            if let Some(snapshot) = self.snapshot.as_mut() {
                let _ = snapshot.restore();
            }
            return;
        }
        // RAII 兜底：调用方未显式恢复（如提前 return）时，走完全同一套归属校验。
        // 已确定结局时 `restore_if_still_owned` 幂等返回，不会重复动作。
        let _ = self.restore_if_still_owned();
    }
}

/// 统一原子写入原语的失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomicWriteError {
    /// 持锁复核时序列号已变（外部改写）：**零修改**退出
    NotOwned,
    /// 序列号不可读（返回 0）：保守失败，零修改
    OwnershipUnknown,
    /// 剪贴板打不开（被其他进程占用）
    Busy,
    /// `EmptyClipboard` 失败（此时剪贴板内容未被修改）
    EmptyFailed,
}

/// **全仓唯一**的剪贴板「清空 + 写入」原子原语（审计需求 `9549648` §3.4.2 / §4.1 / §4.3）。
///
/// 语义（所有修改剪贴板内容的路径都必须走这里）：
///
/// ```text
/// OpenClipboard(valid_owner)
///   -> 在同一次持锁期内读取 current_sequence
///   -> current_sequence != expected_sequence：关闭并返回 NotOwned，零修改
///   -> 相等：（可选）EmptyClipboard -> write() 逐格式 SetClipboardData
///   -> CloseClipboard
///   -> 读取本次写入后的 sequence 与 owner 进程并复核
/// ```
///
/// Windows 同一时刻只允许一个窗口打开剪贴板，因此持锁复核通过到写入完成之间不可能
/// 再有第三方写入，check-then-act 的 TOCTOU 窗口被消除（审计报告 P0-04 竞态 A /
/// 清单 `9549648` P0-05）。
///
/// 返回值：`(写入后的序列号, 成功写入的格式数, 失败格式列表)`。
/// 若本次调用既未清空也未成功写入任何格式，则不要求 owner 复核（内容未被修改）；
/// 但只要产生了修改（或序列号在关闭后仍与期望不符），写入后的 owner 必须是本进程的
/// owner 窗口，否则一律 `NotOwned`（绝不把外部改写的序列号误记成自己的）。
fn clipboard_atomic_write<F>(
    expected_sequence: u32,
    empty_first: bool,
    write: F,
) -> Result<(u32, usize, Vec<u32>), AtomicWriteError>
where
    F: FnOnce() -> (usize, Vec<u32>),
{
    if expected_sequence == 0 {
        return Err(AtomicWriteError::OwnershipUnknown);
    }

    // 确定性会合点：在真正持锁之前制造外部写入（仅测试构建存在）。
    #[cfg(test)]
    run_atomic_write_hook();

    // 必须是“写路径”：以有效 owner 打开，否则 EmptyClipboard 会把 owner 置空
    if !open_clipboard_for_write(4) {
        return Err(AtomicWriteError::Busy);
    }

    unsafe {
        // 持锁复核：此刻其他进程无法打开剪贴板，复核结论在写入前保持有效。
        let current = clipboard_sequence_number();
        if current == 0 {
            let _ = CloseClipboard();
            return Err(AtomicWriteError::OwnershipUnknown);
        }
        if current != expected_sequence {
            let _ = CloseClipboard();
            return Err(AtomicWriteError::NotOwned);
        }

        if empty_first {
            #[cfg(test)]
            TEST_EMPTY_CLIPBOARD_CALLS.fetch_add(1, Ordering::SeqCst);
            if EmptyClipboard().is_err() {
                let _ = CloseClipboard();
                return Err(AtomicWriteError::EmptyFailed);
            }
        }

        let (written, failed) = write();

        // 序列号在 `CloseClipboard` 之后才自增（实测：持锁期间读到的仍是写入前的值），
        // 因此必须在关闭后读取；否则会把写入前的旧序列号登记成自己的，
        // 导致后续归属校验误判为「外部改写」。
        let _ = CloseClipboard();

        let after = clipboard_sequence_number();
        if after == 0 {
            return Err(AtomicWriteError::OwnershipUnknown);
        }

        if empty_first || written > 0 || after != expected_sequence {
            // 归属复核：本次写入的 owner 必须是本进程的 owner 窗口。若在 `CloseClipboard`
            // 与此处之间的极窄窗口内被外部改写，owner 校验会立即发现。
            if clipboard_owner_pid() != std::process::id() {
                return Err(AtomicWriteError::NotOwned);
            }
        }
        Ok((after, written, failed))
    }
}

/// 向剪贴板写入 Unicode 文本的格式写入闭包，返回 `(成功写入数, 失败格式列表)`。
fn write_unicode_text_payload(text: &str) -> (usize, Vec<u32>) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let mut bytes = Vec::with_capacity(wide.len() * 2);
    for unit in &wide {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    if write_clipboard_format(CF_UNICODETEXT, &bytes) {
        (1, Vec::new())
    } else {
        (0, vec![CF_UNICODETEXT])
    }
}

/// 「持锁复核 + 写入」的临时文本原子写入。
///
/// 走 [`clipboard_atomic_write`]：复核序列号、`EmptyClipboard`、`SetClipboardData` 与读取
/// 写入后序列号全部位于**同一次 `OpenClipboard` 持锁期**内（审计报告 P0-04 竞态 A）。
///
/// - `Ok(seq)`：写入成功，`seq` 是本次写入产生的序列号；
/// - `Err(NotOwned)`：持锁复核时序列号已变（外部改写），未做任何修改；
/// - `Err(OwnershipUnknown)`：序列号不可读（返回 0）或剪贴板忙，保守失败；
/// - `Err(WriteFailed)`：打开剪贴板 / `EmptyClipboard` / `SetClipboardData` 失败。
fn set_clipboard_text_checked(
    text: &str,
    expected_sequence: u32,
) -> Result<u32, ClipboardTransactionError> {
    match clipboard_atomic_write(expected_sequence, true, || write_unicode_text_payload(text)) {
        Ok((sequence, _written, failed)) => {
            if failed.is_empty() {
                Ok(sequence)
            } else {
                // 已清空但文本未写进去：剪贴板状态已变且不可控，一律 fail-closed
                Err(ClipboardTransactionError::WriteFailed)
            }
        }
        Err(AtomicWriteError::NotOwned) => Err(ClipboardTransactionError::NotOwned),
        Err(AtomicWriteError::OwnershipUnknown) => Err(ClipboardTransactionError::OwnershipUnknown),
        Err(AtomicWriteError::Busy) | Err(AtomicWriteError::EmptyFailed) => {
            Err(ClipboardTransactionError::WriteFailed)
        }
    }
}

/// 安全地在受保护的环境下临时使用剪贴板注入文本。
///
/// 审计需求 `9549648` §4.1：本函数只是 [`ClipboardTransaction`] 的薄封装，**不再**
/// 手工拼装 capture → check → write → restore：
/// 1. 开启事务（完整快照 + `capture_sequence` 绑定；失败则绝不修改剪贴板）；
/// 2. `write_owned_text` 在统一原子原语的**同一次持锁期**内复核 `capture_sequence`，
///    不一致就零修改失败（消除「快照完成 → 临时写入」之间的竞态，§3.3）；
/// 3. 执行物理操作闭包；
/// 4. 无论成功与否立即按严格 sequence 归属校验恢复（`Drop` 还要再兑底一次）；
/// 5. 恢复本身失败向上传播为 `RestoreFailed`，调用方不得据此认为“已无损还原”。
pub fn with_temporary_clipboard_text<R, F>(text: &str, action: F) -> Result<R, ClipboardSafetyError>
where
    F: FnOnce() -> R,
{
    let mut tx = ClipboardTransaction::begin()?;

    if tx.write_owned_text(text).is_err() {
        // 归属已丢 / 不可判定 / 写入失败：一律零修改放弃，{@code action} 绝不执行。
        // `tx` 在本行末 drop，Drop 内的归属校验会保持当前内容不变。
        return Err(ClipboardSafetyError::WriteFailed);
    }

    let result = action();

    match tx.restore_if_still_owned() {
        ClipboardTransactionOutcome::Committed => Ok(result),
        ClipboardTransactionOutcome::RestorePartialFailure(err) => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_FAILED: RestorePartialFailure({:?})",
                err
            ));
            Err(ClipboardSafetyError::RestoreFailed(err))
        }
        outcome => {
            // 其余均为「已保守放弃覆盖用户当前内容」的结构化终止态，只记状态，
            // 绝不记剪贴板正文；闭包结果本身仍有效。
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_SKIPPED: {:?}",
                outcome
            ));
            Ok(result)
        }
    }
}

/// 安全读取当前剪贴板的纯文本（若剪贴板无纯文本则返回 None）
pub fn get_clipboard_text() -> Option<String> {
    get_clipboard_text_checked().map(|(text, _)| text)
}

/// 读取纯文本并同时返回**同一次持锁期内**读到的剪贴板序列号。
///
/// 读回判定绝不能「先读 sequence 再读文本」：那样两次之间可能发生外部写入，
/// 得到「旧序列号 + 新文本」的错误组合（又一个 check-then-act）。
fn get_clipboard_text_checked() -> Option<(String, u32)> {
    if !open_clipboard_for_read(4) {
        return None;
    }
    unsafe {
        let sequence = clipboard_sequence_number();
        let mut result = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT)
            && !h.0.is_null()
        {
            let ptr = GlobalLock(HGLOBAL(h.0));
            if !ptr.is_null() {
                let u16_ptr = ptr as *const u16;
                let mut len = 0;
                while *u16_ptr.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(u16_ptr, len);
                result = Some(String::from_utf16_lossy(slice));
                let _ = GlobalUnlock(HGLOBAL(h.0));
            }
        }
        let _ = CloseClipboard();
        if sequence == 0 {
            return None;
        }
        result.map(|text| (text, sequence))
    }
}

/// 跨模块共享的「真实剪贴板用例串行锁」（仅测试构建）。
///
/// 本文件内部的用例与其它模块（如 `managers::probe_lease` / `managers::clipboard_probe`）
/// 里同样操作真实剪贴板的用例必须共用同一把锁，否则并行执行时会互相抢剪贴板。
#[cfg(test)]
pub(crate) fn clipboard_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static CLIP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    CLIP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::clipboard_probe::{probe_panel_path, probe_two_panels};
    use std::sync::Mutex;
    use windows::Win32::System::DataExchange::GetClipboardOwner;

    /// 串行化剪贴板用例。即使某个用例 panic，也不要因为锁中毒而掩盖其他用例的真实结果。
    fn clip_test_lock() -> std::sync::MutexGuard<'static, ()> {
        clipboard_test_lock()
    }

    /// 测试夹具：把纯文本写入剪贴板。
    ///
    /// 它不另建一套“旁路”实现，而是走与生产路径**同一个**统一原子原语（只有期望值
    /// 取“当前序列号”——测试用例串行执行，这个读取窗口无关紧要）。
    fn set_raw_clipboard_text(text: &str) -> bool {
        set_raw_clipboard_text_tracked(text).is_some()
    }

    /// 测试夹具：写入纯文本并返回写入后的序列号（失败 / 归属不可判定时为 None）。
    fn set_raw_clipboard_text_tracked(text: &str) -> Option<u32> {
        let expected = clipboard_sequence_number();
        match clipboard_atomic_write(expected, true, || write_unicode_text_payload(text)) {
            Ok((sequence, _written, failed)) if failed.is_empty() => Some(sequence),
            _ => None,
        }
    }

    /// 测试夹具：统一原子原语真正执行 `EmptyClipboard` 的累计次数。
    fn empty_clipboard_calls() -> usize {
        TEST_EMPTY_CLIPBOARD_CALLS.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 测试夹具：当前剪贴板的完整观测快照（文本 / 格式集 / sequence / owner HWND / owner PID）。
    ///
    /// 清单 §3.5 要求每个用例至少断言这些量，故统一封口，避免逐处重写。
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ClipboardObservation {
        text: Option<String>,
        formats: Vec<u32>,
        sequence: u32,
        owner_hwnd: usize,
        owner_pid: u32,
    }

    fn observe_clipboard() -> ClipboardObservation {
        assert!(open_clipboard_for_read(10), "观测必须能只读打开剪贴板");
        let sequence = clipboard_sequence_number();
        let owner = unsafe { GetClipboardOwner() }.unwrap_or_default();
        let owner_hwnd = owner.0 as usize;
        let mut formats = Vec::new();
        unsafe {
            let mut fmt = 0u32;
            loop {
                fmt = EnumClipboardFormats(fmt);
                if fmt == 0 {
                    break;
                }
                formats.push(fmt);
            }
        }
        let text = {
            let mut out = None;
            unsafe {
                if let Ok(h) = GetClipboardData(CF_UNICODETEXT)
                    && !h.0.is_null()
                {
                    let ptr = GlobalLock(HGLOBAL(h.0));
                    if !ptr.is_null() {
                        let u16_ptr = ptr as *const u16;
                        let mut len = 0;
                        while *u16_ptr.add(len) != 0 {
                            len += 1;
                        }
                        let slice = std::slice::from_raw_parts(u16_ptr, len);
                        out = Some(String::from_utf16_lossy(slice));
                        let _ = GlobalUnlock(HGLOBAL(h.0));
                    }
                }
            }
            out
        };
        let _ = unsafe { CloseClipboard() };
        ClipboardObservation {
            text,
            formats,
            sequence,
            owner_hwnd,
            owner_pid: clipboard_owner_pid(),
        }
    }

    /// 测试夹具：阻塞 owner 消息泵，返回“放行”句柄。
    ///
    /// 用于确定性复现清单 §2.3「两次剪贴板更新均已发生、通知尚未处理」的交错。
    fn block_owner_pump_for_test() -> mpsc::Sender<()> {
        let (reached_tx, reached_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        *TEST_OWNER_PUMP_BLOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(OwnerPumpBlock {
            reached_tx,
            release_rx: Mutex::new(release_rx),
        });
        let owner = clipboard_owner().expect("owner 窗口必须可用");
        unsafe {
            PostMessageW(Some(owner), WM_QP_TEST_BLOCK_PUMP, WPARAM(0), LPARAM(0))
                .expect("阻塞消息必须能投递到 owner 窗口");
        }
        reached_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("owner 消息泵必须到达阻塞点");
        release_tx
    }

    #[test]
    fn test_clipboard_safety_temporary_text_and_restore() {
        let _guard = clip_test_lock();
        let initial_text =
            get_clipboard_text().unwrap_or_else(|| "initial_qp_test_data".to_string());
        let _ = set_raw_clipboard_text(&initial_text);

        let temp_injected = "C:\\Windows\\System32";
        let execution_result = with_temporary_clipboard_text(temp_injected, || {
            let inside = get_clipboard_text().unwrap_or_default();
            assert_eq!(inside, temp_injected, "闭包内部剪贴板应为临时注入的文本");
            42
        });

        assert_eq!(execution_result, Ok(42));
        let after_restore = get_clipboard_text().unwrap_or_default();
        assert_eq!(
            after_restore, initial_text,
            "闭包执行结束后必须还原原有剪贴板内容"
        );
    }

    #[test]
    fn test_clipboard_safety_bitmap_dib_preservation() {
        let _guard = clip_test_lock();
        // 构造合法的最简 1x1 24-bit DIB 内存块
        let mut dib_data = [0u8; 40 + 4];
        dib_data[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize = 40
        dib_data[4..8].copy_from_slice(&1i32.to_le_bytes()); // biWidth = 1
        dib_data[8..12].copy_from_slice(&1i32.to_le_bytes()); // biHeight = 1
        dib_data[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes = 1
        dib_data[14..16].copy_from_slice(&24u16.to_le_bytes()); // biBitCount = 24
        dib_data[20..24].copy_from_slice(&4u32.to_le_bytes()); // biSizeImage = 4
        dib_data[40] = 50;
        dib_data[41] = 200;
        dib_data[42] = 123;
        dib_data[43] = 0;

        let original_text = get_clipboard_text();

        assert!(
            open_clipboard_for_write(10),
            "测试初始化必须以合法 owner 打开剪贴板"
        );
        unsafe {
            let _ = EmptyClipboard();
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, dib_data.len()) {
                let ptr = GlobalLock(h);
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(
                        dib_data.as_ptr(),
                        ptr as *mut u8,
                        dib_data.len(),
                    );
                    let _ = GlobalUnlock(h);
                    let _ = SetClipboardData(CF_DIB, Some(HANDLE(h.0)));
                }
            }
            let _ = CloseClipboard();
        }

        let snap_res = ClipboardSnapshot::capture();
        assert!(
            snap_res.is_ok(),
            "含 DIB 的剪贴板必须能被成功安全捕获: {:?}",
            snap_res.as_ref().err()
        );
        let snap = snap_res.unwrap();
        assert!(
            snap.items
                .iter()
                .any(|(f, d)| *f == CF_DIB && d.len() == dib_data.len()),
            "快照中必须含有 CF_DIB 数据"
        );

        let res = with_temporary_clipboard_text("C:\\TestPath", || {
            let t = get_clipboard_text().unwrap_or_default();
            assert_eq!(t, "C:\\TestPath");
        });
        assert!(res.is_ok());

        if open_clipboard_for_read(4) {
            unsafe {
                if let Ok(h) = GetClipboardData(CF_DIB) {
                    assert!(!h.0.is_null(), "恢复后剪贴板必须仍持有有效 CF_DIB");
                    let size = GlobalSize(HGLOBAL(h.0));
                    assert_eq!(size, dib_data.len());
                } else {
                    panic!("恢复后必须能够获取到 CF_DIB");
                }
                let _ = CloseClipboard();
            }
        }

        if let Some(txt) = original_text {
            let _ = set_raw_clipboard_text(&txt);
        }
    }

    /// 单格式写回可注入的失败环节，逐一对应 `HandleOps` 的三个可失败入口。
    #[derive(Clone, Copy, Debug)]
    enum FailOp {
        /// `SetClipboardData` 失败（等价于既有默认注入）
        Set,
        /// `GlobalAlloc` 分配失败
        Alloc,
        /// `GlobalLock` 加锁失败
        Lock,
    }

    impl FailOp {
        fn mode(self) -> u8 {
            match self {
                FailOp::Set => 0,
                FailOp::Alloc => 1,
                FailOp::Lock => 2,
            }
        }
    }

    /// 测试期故障注入开关，保证即使断言失败也不会污染后续用例
    struct FailInjection;

    impl FailInjection {
        fn enable(format: u32) -> Self {
            Self::enable_with(format, FailOp::Set)
        }

        /// 指定失败环节注入：格式 `format` 的写回必须在 `op` 处失败
        fn enable_with(format: u32, op: FailOp) -> Self {
            TEST_FAIL_MODE.store(op.mode(), std::sync::atomic::Ordering::SeqCst);
            TEST_FAIL_FORMAT.store(format, std::sync::atomic::Ordering::SeqCst);
            Self
        }
    }

    /// 测试期「剪贴板被占用」故障注入（同进程内无法真实模拟跨进程独占）
    struct BusyInjection;

    impl BusyInjection {
        fn enable() -> Self {
            TEST_FORCE_BUSY.store(true, std::sync::atomic::Ordering::SeqCst);
            Self
        }
    }

    impl Drop for BusyInjection {
        fn drop(&mut self) {
            TEST_FORCE_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// 测试期「序列号不可读」故障注入：`GetClipboardSequenceNumber()` 返回 0
    struct SeqZeroInjection;

    impl SeqZeroInjection {
        fn enable() -> Self {
            TEST_FORCE_SEQ_ZERO.store(true, std::sync::atomic::Ordering::SeqCst);
            Self
        }
    }

    impl Drop for SeqZeroInjection {
        fn drop(&mut self) {
            TEST_FORCE_SEQ_ZERO.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// 测试期「owner 创建失败」故障注入
    struct OwnerFailInjection;

    impl OwnerFailInjection {
        fn enable() -> Self {
            TEST_FORCE_NO_OWNER.store(true, std::sync::atomic::Ordering::SeqCst);
            Self
        }
    }

    impl Drop for OwnerFailInjection {
        fn drop(&mut self) {
            TEST_FORCE_NO_OWNER.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Drop for FailInjection {
        fn drop(&mut self) {
            TEST_FAIL_FORMAT.store(u32::MAX, std::sync::atomic::Ordering::SeqCst);
            TEST_FAIL_MODE.store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// 构造合法的最小 1x1 24-bit DIB 内存块（40 字节 BITMAPINFOHEADER + 4 字节像素）
    fn minimal_dib_blob() -> [u8; 44] {
        let mut dib_data = [0u8; 44];
        dib_data[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize = 40
        dib_data[4..8].copy_from_slice(&1i32.to_le_bytes()); // biWidth = 1
        dib_data[8..12].copy_from_slice(&1i32.to_le_bytes()); // biHeight = 1
        dib_data[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes = 1
        dib_data[14..16].copy_from_slice(&24u16.to_le_bytes()); // biBitCount = 24
        dib_data[20..24].copy_from_slice(&4u32.to_le_bytes()); // biSizeImage = 4
        dib_data
    }

    /// 写入一个合法的最小 1x1 24-bit DIB（并附带 Unicode 文本），供失败注入用例复用
    fn seed_dib_clipboard() {
        seed_clipboard_formats(&[(CF_DIB, &minimal_dib_blob()[..])]);
    }

    /// P0-01：单格式恢复失败时（分配 / 加锁 / 设置三处故障逐个注入），
    /// restore() 必须返回结构化错误且绝不置 restored；失败后其它格式不得被清掉；
    /// 清除故障后重试必须能补齐（只补写失败格式，不重复清空已恢复内容）。
    #[test]
    fn test_restore_partial_failure_is_reported_and_retryable() {
        let _guard = clip_test_lock();

        for op in [FailOp::Set, FailOp::Alloc, FailOp::Lock] {
            seed_dib_clipboard();

            let mut snap = ClipboardSnapshot::capture().expect("含 DIB 的剪贴板必须可被安全捕获");
            assert!(
                snap.items.iter().any(|(f, _)| *f == CF_DIB),
                "快照必须含 CF_DIB"
            );
            assert!(
                snap.items.iter().any(|(f, _)| *f == CF_UNICODETEXT),
                "快照必须含 CF_UNICODETEXT，用于验证失败时其它格式不被破坏"
            );
            // 注意（审计需求 9549648 §3.4.1）：快照与 `capture_sequence` 绑定后，
            // 不能再「先清掉剪贴板再恢复」——那本身就是一次外部变更，会被判为
            // `Abandoned`（零修改退出），从而再也验证不到部分失败路径。

            {
                let _inject = FailInjection::enable_with(CF_DIB, op);
                let err = snap.restore().expect_err("单格式恢复失败时不得返回成功");
                match err {
                    ClipboardRestoreError::PartialRestore(ref failed) => {
                        assert!(
                            failed.contains(&CF_DIB),
                            "{:?}: 失败列表必须包含 CF_DIB，实际 {:?}",
                            op,
                            failed
                        );
                    }
                    other => panic!("{:?}: 期望 PartialRestore，实际 {:?}", op, other),
                }
                assert!(!snap.restored, "部分失败时绝不能标记为已恢复");

                // 未注入故障的格式必须已写回，且重试时不得被再次清空
                assert_eq!(
                    get_clipboard_text().as_deref(),
                    Some("qp_multiformat_seed"),
                    "{:?}: 失败后其它格式必须仍保留在剪贴板中",
                    op
                );
            }

            // 故障解除后重试只补写缺失格式
            assert_eq!(snap.restore(), Ok(RestoreStatus::Restored));
            assert!(snap.restored, "成功恢复后必须置位 restored");
            // 幂等：重复调用不得再动剪贴板
            assert_eq!(snap.restore(), Ok(RestoreStatus::AlreadyRestored));

            assert!(open_clipboard_for_read(10));
            unsafe {
                let restored = GetClipboardData(CF_DIB);
                assert!(restored.is_ok(), "重试成功后剪贴板必须重新持有 CF_DIB");
                let _ = CloseClipboard();
            }
        }
    }

    /// P0-01：写入临时内容后若第三方（用户）已复制新内容，恢复必须主动放弃覆盖
    #[test]
    fn test_restore_refuses_to_overwrite_newer_user_content() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_snapshot_seed"));

        let mut snap = ClipboardSnapshot::capture().expect("文本剪贴板必须可被安全捕获");
        let seq =
            set_raw_clipboard_text_tracked("C:\\TemporaryInjection").expect("写入临时文本必须成功");
        snap.bind_owned_write("C:\\TemporaryInjection", seq);

        // 模拟用户/第三方在恢复前复制了全新内容
        assert!(set_raw_clipboard_text("qp_user_new_content"));

        assert_eq!(
            snap.restore(),
            Ok(RestoreStatus::Abandoned),
            "序列号变化时必须永久放弃覆盖"
        );
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_user_new_content"),
            "用户新复制的内容绝不能被旧快照覆盖"
        );

        let _ = set_raw_clipboard_text("qp_snapshot_seed");
    }

    /// P0-01：`with_temporary_clipboard_text` 必须把恢复失败向上传播，而不是静默返回 Ok。
    /// 分配 / 加锁 / 设置三处故障逐个注入，均不得假报成功。
    ///
    /// 注入时机（审计需求 `9549648` §3.4.2）：必须注入在**恢复**那一次原子写入上，
    /// 而不是临时写入上 —— 否则失败会在“写入临时文本”阶段就短路，验证不到恢复传播。
    /// 故用原子写入会合点 `skip = 1` 把故障开关安排在第二次原子写入（首次恢复）开头。
    #[test]
    fn test_temporary_clipboard_text_propagates_restore_failure() {
        let _guard = clip_test_lock();

        for op in [FailOp::Set, FailOp::Alloc, FailOp::Lock] {
            assert!(set_raw_clipboard_text("qp_propagate_seed"));

            arm_atomic_write_hook(1, move || {
                TEST_FAIL_MODE.store(op.mode(), std::sync::atomic::Ordering::SeqCst);
                TEST_FAIL_FORMAT.store(CF_UNICODETEXT, std::sync::atomic::Ordering::SeqCst);
            });
            let outcome: Result<u32, ClipboardSafetyError> =
                with_temporary_clipboard_text("C:\\PropagateProbe", || 7);
            TEST_FAIL_FORMAT.store(u32::MAX, std::sync::atomic::Ordering::SeqCst);
            TEST_FAIL_MODE.store(0, std::sync::atomic::Ordering::SeqCst);

            match outcome {
                Err(ClipboardSafetyError::RestoreFailed(err)) => {
                    // 内容未恢复也不能假报成功：调用方拿不到闭包结果
                    assert!(
                        matches!(err, ClipboardRestoreError::PartialRestore(ref v) if v.contains(&CF_UNICODETEXT)),
                        "{:?}: 恢复失败必须是含 CF_UNICODETEXT 的 PartialRestore，实际 {:?}",
                        op,
                        err
                    );
                }
                other => panic!("{:?}: 期望 RestoreFailed 错误，实际 {:?}", op, other),
            }
        }

        let _ = set_raw_clipboard_text("qp_propagate_seed");
    }

    /// P0-01：分配失败 / 加锁失败 / 设置失败三条分支的回退行为与句柄回收
    #[test]
    fn test_single_format_failure_paths_release_handle() {
        let _guard = clip_test_lock();
        static FREE_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        fn counting_free(_handle: HGLOBAL) {
            FREE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        let data = b"quickpath-fault-injection-probe";

        // 1) 分配失败：无句柄可回收，必须直接返回失败
        let alloc_fail = HandleOps {
            alloc: |_| None,
            free: counting_free,
            ..PROD_HANDLE_OPS
        };
        FREE_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
        assert!(!write_clipboard_format_with(CF_DIB, data, &alloc_fail));
        assert_eq!(FREE_CALLS.load(std::sync::atomic::Ordering::SeqCst), 0);

        // 2) 加锁失败：已分配成功，必须回收句柄（否则泄漏）
        let lock_fail = HandleOps {
            lock: |_| std::ptr::null_mut(),
            free: counting_free,
            ..PROD_HANDLE_OPS
        };
        FREE_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
        assert!(!write_clipboard_format_with(CF_DIB, data, &lock_fail));
        assert_eq!(
            FREE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "加锁失败必须释放句柄"
        );

        // 3) 设置失败：拷贝已完成，但所有权未交接，必须回收句柄
        let set_fail = HandleOps {
            set: |_, _| false,
            free: counting_free,
            ..PROD_HANDLE_OPS
        };
        FREE_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
        assert!(!write_clipboard_format_with(CF_DIB, data, &set_fail));
        assert_eq!(
            FREE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "设置失败必须释放句柄"
        );

        // 空数据：无可交接内容，视为成功且不分配句柄
        assert!(write_clipboard_format_with(CF_DIB, &[], &alloc_fail));
    }

    /// 写入一个 CF_HDROP（文件列表）数据块（DROPFILES + 宽字符双 NUL 结尾文件名表）
    fn hdrop_blob(files: &[&str]) -> Vec<u8> {
        let mut blob = vec![0u8; 20];
        blob[0..4].copy_from_slice(&20u32.to_le_bytes()); // pFiles 偏移
        blob[16..20].copy_from_slice(&1i32.to_le_bytes()); // fWide = TRUE
        for file in files {
            for unit in file.encode_utf16() {
                blob.extend_from_slice(&unit.to_le_bytes());
            }
            blob.extend_from_slice(&0u16.to_le_bytes());
        }
        blob.extend_from_slice(&0u16.to_le_bytes());
        blob
    }

    /// 用指定字节块占用多个剪贴板格式，并顺带写入固定测货 Unicode 文本
    fn seed_clipboard_formats(entries: &[(u32, &[u8])]) {
        seed_clipboard_formats_with_text("qp_multiformat_seed", entries);
    }

    /// 同上，但可指定随附的 Unicode 文本（用于「字面文本相同、格式不同」的交错用例）
    fn seed_clipboard_formats_with_text(text: &str, entries: &[(u32, &[u8])]) {
        assert!(
            open_clipboard_for_write(10),
            "测试必须以合法 owner 打开剪贴板"
        );
        unsafe {
            let _ = EmptyClipboard();
            for (format, bytes) in entries {
                let handle = prod_alloc(bytes.len()).expect("分配必须成功");
                let ptr = prod_lock(handle);
                assert!(!ptr.is_null());
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
                prod_unlock(handle);
                assert!(prod_set(*format, handle), "格式 {} 必须写回成功", format);
            }
            let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let bytes = wide.len() * 2;
            let handle = prod_alloc(bytes).expect("分配必须成功");
            let ptr = prod_lock(handle);
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr, bytes);
            prod_unlock(handle);
            assert!(prod_set(CF_UNICODETEXT, handle));
            let _ = CloseClipboard();
        }
    }

    /// 读取指定剪贴板格式的原始字节（用于断言格式内容未被旧快照覆盖）。
    fn read_clipboard_format_bytes(format: u32) -> Option<Vec<u8>> {
        if !open_clipboard_for_read(10) {
            return None;
        }
        let mut out = None;
        unsafe {
            if let Ok(h) = GetClipboardData(format)
                && !h.0.is_null()
            {
                let size = GlobalSize(HGLOBAL(h.0));
                if size > 0 {
                    let ptr = GlobalLock(HGLOBAL(h.0));
                    if !ptr.is_null() {
                        let mut data = vec![0u8; size];
                        std::ptr::copy_nonoverlapping(ptr as *const u8, data.as_mut_ptr(), size);
                        let _ = GlobalUnlock(HGLOBAL(h.0));
                        out = Some(data);
                    }
                }
            }
            let _ = CloseClipboard();
        }
        out
    }

    /// 恢复后重新捕获，断言原快照的每个格式字节完全一致
    fn assert_snapshot_roundtrip(original: &ClipboardSnapshot, label: &str) {
        let after = ClipboardSnapshot::capture()
            .unwrap_or_else(|err| panic!("{}: 恢复后应能再次安全捕获: {:?}", label, err));
        for (format, data) in &original.items {
            let got = after
                .items
                .iter()
                .find(|(f, _)| f == format)
                .map(|(_, d)| d);
            assert_eq!(
                got,
                Some(data),
                "{}: 格式 {} 必须在恢复后字节完全一致",
                label,
                format
            );
        }
    }

    /// DIB 规范化容忍比对：`BITMAPINFOHEADER` 关键字段 + 总大小 + 像素数据必须一致。
    /// 比「只要存在就算过」严格，同时容忍 Windows 对头部字段的规范化。
    fn assert_dib_equivalent(got: &[u8], want: &[u8], label: &str) {
        assert!(
            got.len() >= 40 && want.len() >= 40,
            "{}: DIB 必须至少含 40 字节 BITMAPINFOHEADER",
            label
        );
        // biSize / biWidth / biHeight / biPlanes / biBitCount
        for (offset, len) in [(0usize, 4usize), (4, 4), (8, 4), (12, 2), (14, 2)] {
            assert_eq!(
                &got[offset..offset + len],
                &want[offset..offset + len],
                "{}: DIB 头字段 @{} 必须一致",
                label,
                offset
            );
        }
        assert_eq!(got.len(), want.len(), "{}: DIB 总大小必须一致", label);
        assert_eq!(&got[40..], &want[40..], "{}: DIB 像素数据必须一致", label);
    }

    /// 逐格式断言恢复后内容保真：DIB 走 `assert_dib_equivalent`（容忍头部规范化），
    /// 其余格式必须字节完全一致。任何格式丢失都直接报错。
    fn assert_mixed_roundtrip(original: &ClipboardSnapshot, label: &str) {
        let after = ClipboardSnapshot::capture()
            .unwrap_or_else(|err| panic!("{}: 恢复后应能再次安全捕获: {:?}", label, err));
        for (format, data) in &original.items {
            let got = after
                .items
                .iter()
                .find(|(f, _)| f == format)
                .map(|(_, d)| d)
                .unwrap_or_else(|| panic!("{}: 格式 {} 在恢复后丢失", label, format));
            if *format == CF_DIB {
                assert_dib_equivalent(got, data, label);
            } else {
                assert_eq!(
                    got, data,
                    "{}: 格式 {} 必须在恢复后字节完全一致",
                    label, format
                );
            }
        }
    }

    /// P0-01：文件列表（CF_HDROP）+ 注册格式 + Unicode 文本混合剪贴板必须完整往返
    #[test]
    fn test_hdrop_and_registered_format_survive_temporary_text_injection() {
        let _guard = clip_test_lock();

        let hdrop = hdrop_blob(&[r"C:\QuickPath\a.txt", r"D:\QuickPath\b.txt"]);
        let registered = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "QuickPathProbeFormat"
            ))
        };
        assert!(registered != 0, "注册剪贴板格式必须成功");
        let registered_bytes = b"quickpath-registered-format-probe".to_vec();

        seed_clipboard_formats(&[(CF_HDROP, &hdrop), (registered, &registered_bytes)]);

        let before =
            ClipboardSnapshot::capture().expect("含 CF_HDROP 与注册格式的剪贴板必须可安全捕获");
        assert!(
            before.items.iter().any(|(f, _)| *f == CF_HDROP),
            "快照必须含 CF_HDROP"
        );
        assert!(
            before.items.iter().any(|(f, _)| *f == registered),
            "快照必须含注册格式"
        );

        with_temporary_clipboard_text("C:\\TemporaryInjection", || {
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some("C:\\TemporaryInjection"),
                "闭包内部应为临时注入文本"
            );
        })
        .expect("临时注入与恢复必须成功");

        assert_snapshot_roundtrip(&before, "CF_HDROP+注册格式+文本");
    }

    /// 审计报告 §10-2 + §12 剪贴板清单：Unicode、HTML、RTF、CF_HDROP、DIB 与自定义
    /// 注册 HGLOBAL 格式同处一次剪贴板会话，经真实「临时文本注入 + 恢复」后逐格式保真往返。
    ///
    /// 断言强度：
    /// - 每个格式都必须存在于快照中（防止种子阶段静默丢格式）；
    /// - 恢复后每个格式都必须字节一致（DIB 按 `assert_dib_equivalent` 比对关键头字段、
    ///   总大小与像素数据，而不是「只要存在就算过」）；
    /// - 恢复后剪贴板必须不再持有临时注入文本。
    #[test]
    fn test_mixed_formats_survive_temporary_text_injection() {
        let _guard = clip_test_lock();

        let html_format = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "HTML Format"
            ))
        };
        let rtf_format = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "Rich Text Format"
            ))
        };
        let custom_format = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "QuickPathMixedFormatProbe"
            ))
        };
        assert!(
            html_format != 0 && rtf_format != 0 && custom_format != 0,
            "HTML/RTF/自定义格式注册必须成功"
        );

        let html = "<html><body>QuickPath HTML 中文</body></html>"
            .as_bytes()
            .to_vec();
        let rtf = r"{\rtf1\ansi QuickPath RTF 中文}".as_bytes().to_vec();
        let custom = b"quickpath-custom-hglobal-probe".to_vec();
        let hdrop = hdrop_blob(&[
            r"C:\QuickPath\one.txt",
            r"D:\QuickPath\two.bin",
            r"E:\QuickPath\three.log",
        ]);
        let dib = minimal_dib_blob();

        // seed_clipboard_formats 会额外写入 CF_UNICODETEXT，故六类格式同处一份剪贴板
        seed_clipboard_formats(&[
            (html_format, &html),
            (rtf_format, &rtf),
            (custom_format, &custom),
            (CF_HDROP, &hdrop),
            (CF_DIB, &dib[..]),
        ]);

        let before = ClipboardSnapshot::capture().expect("混合格式剪贴板必须能被安全完整捕获");
        for fmt in [
            CF_UNICODETEXT,
            html_format,
            rtf_format,
            CF_HDROP,
            CF_DIB,
            custom_format,
        ] {
            assert!(
                before.items.iter().any(|(f, _)| *f == fmt),
                "快照必须含格式 {}",
                fmt
            );
        }

        with_temporary_clipboard_text("C:\\TemporaryInjection", || {
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some("C:\\TemporaryInjection"),
                "闭包内部应为临时注入文本"
            );
        })
        .expect("混合格式下的临时注入与恢复必须成功");

        assert_mixed_roundtrip(&before, "混合格式(Unicode/HTML/RTF/HDROP/DIB/自定义)");

        assert_ne!(
            get_clipboard_text().as_deref(),
            Some("C:\\TemporaryInjection"),
            "恢复后剪贴板不得再持有临时注入文本"
        );
    }

    /// P0-01：剪贴板被其他进程/线程独占时，捕获/恢复必须返回结构化 Busy，且绝不置 restored
    #[test]
    fn test_clipboard_busy_is_reported_and_never_claims_success() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_busy_seed"));

        let mut snap = ClipboardSnapshot::capture().expect("空闲时必须可捕获");

        {
            let _busy = BusyInjection::enable();
            assert_eq!(
                ClipboardSnapshot::capture().err(),
                Some(ClipboardSafetyError::Busy),
                "被占用时捕获必须报 Busy"
            );
            assert_eq!(
                snap.restore(),
                Err(ClipboardRestoreError::Busy),
                "被占用时恢复必须报 Busy，绝不得报告成功"
            );
            assert!(!snap.restored, "被占用时绝不能标记为已恢复");
        }

        // 占用解除后必须能正常完成恢复
        assert_eq!(snap.restore(), Ok(RestoreStatus::Restored));
        let _ = set_raw_clipboard_text("qp_busy_seed");
    }

    /// 审计报告 §12 剪贴板清单：owner 创建失败时，所有写路径必须 fail-closed，
    /// 调用方收到明确失败（绝不假报成功），且原剪贴板内容保持不变。
    #[test]
    fn test_owner_creation_failure_keeps_clipboard_unchanged() {
        let _guard = clip_test_lock();

        assert!(set_raw_clipboard_text("qp_owner_fail_seed"));
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_owner_fail_seed"),
            "前置条件：种子文本必须写入成功"
        );

        {
            let _no_owner = OwnerFailInjection::enable();

            assert!(clipboard_owner().is_none(), "注入后 owner 必须不可用");
            assert!(
                !open_clipboard_for_write(4),
                "owner 不可用时写路径打开剪贴板必须失败"
            );

            let outcome: Result<u32, ClipboardSafetyError> =
                with_temporary_clipboard_text("C:\\OwnerFailProbe", || 9);
            assert_eq!(
                outcome,
                Err(ClipboardSafetyError::WriteFailed),
                "owner 创建失败时临时写入必须明确失败"
            );

            // 关键断言：原剪贴板内容不得被破坏
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some("qp_owner_fail_seed"),
                "owner 创建失败时原剪贴板必须保持不变"
            );
        }

        // 注入解除后写路径恢复可用
        assert!(set_raw_clipboard_text("qp_owner_fail_seed"));
    }

    /// P0-01：所有写路径必须以「有效窗口」为 owner 打开剪贴板；
    /// 且 `EmptyClipboard` 后 owner 必须生效、`SetClipboardData` 必须真正成功。
    ///
    /// 审计报告 S-02：本用例经 `clipboard_owner()` 走真实的
    /// `RegisterClassW` + `CreateWindowExW` 路径，因此同时充当 S-02 的端到端回归守卫
    /// （若 `CreateWindowExW` 的实例句柄与注册时不配对导致创建失败，此处会直接失败）。
    #[test]
    fn test_clipboard_owner_window_makes_writes_succeed() {
        let _guard = clip_test_lock();

        let owner = clipboard_owner().expect("必须能够创建进程级剪贴板 owner 窗口");
        assert!(!owner.0.is_null(), "owner 句柄不得为空");
        unsafe {
            assert!(IsWindow(Some(owner)).as_bool(), "owner 必须是存活窗口");
        }

        assert!(
            open_clipboard_with_retry(Some(owner), 10),
            "以合法 owner 必须能打开剪贴板"
        );
        unsafe {
            assert!(EmptyClipboard().is_ok(), "以合法 owner 清空剪贴板必须成功");
            assert_eq!(
                GetClipboardOwner().ok(),
                Some(owner),
                "EmptyClipboard 后 owner 必须是我们传入的窗口（而不是 NULL）"
            );

            let wide: Vec<u16> = "qp_owner_probe".encode_utf16().chain(Some(0)).collect();
            let bytes = wide.len() * 2;
            let handle = prod_alloc(bytes).expect("分配必须成功");
            let ptr = prod_lock(handle);
            assert!(!ptr.is_null());
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr, bytes);
            prod_unlock(handle);
            assert!(
                prod_set(CF_UNICODETEXT, handle),
                "以合法 owner 写入必须成功（P0-01 的核心断言）"
            );
            let _ = CloseClipboard();
        }

        assert_eq!(get_clipboard_text().as_deref(), Some("qp_owner_probe"));
    }

    /// 审计报告 S-02：注册窗口类与创建窗口必须使用**同一个** `hinstance`。
    ///
    /// 真实调用 `RegisterClassW` + `CreateWindowExW` 并捕获 `GetLastError`：
    /// 1. `GetModuleHandleW(None)` 必须成功且句柄非空（不得静默退化为 NULL）；
    /// 2. `RegisterClassW` 成功返回非零 ATOM，或失败时错误码必须明确为
    ///    `ERROR_CLASS_ALREADY_EXISTS`（而非 0 / 其他模糊错误）；
    /// 3. 以同一 `hinstance` 创建窗口必须返回非空 HWND 且 `IsWindow` 为真；
    /// 4. 以该 HWND 为 owner 的 `OpenClipboard` + `EmptyClipboard` + `SetClipboardData`
    ///    必须成功，且 `GetClipboardOwner()` 等于该 HWND。
    #[test]
    fn test_owner_class_registration_and_window_creation_share_hinstance() {
        let _guard = clip_test_lock();

        let module = unsafe { GetModuleHandleW(None) }.expect("GetModuleHandleW(None) 必须成功");
        assert!(!module.0.is_null(), "模块实例句柄不得为空");
        let hinstance = HINSTANCE(module.0);

        // 独立类名，避免与生产 owner 类互相干扰（生产类可能已被注册）。
        let class_name = w!("QuickPath_ClipboardOwner_TestClass");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(clipboard_owner_proc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };

        let atom = unsafe { RegisterClassW(&wc) };
        if atom == 0 {
            let err = unsafe { GetLastError() };
            assert_eq!(
                err, ERROR_CLASS_ALREADY_EXISTS,
                "RegisterClassW 失败时错误码必须明确为 ERROR_CLASS_ALREADY_EXISTS，实际 {}",
                err.0
            );
        }

        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("QuickPath Clipboard Owner (test)"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
        };

        let hwnd = match created {
            Ok(h) => h,
            Err(e) => panic!(
                "以与注册一致的 hinstance 创建窗口必须成功：GetLastError={} HRESULT={:?}",
                unsafe { GetLastError().0 },
                e.code()
            ),
        };
        assert!(!hwnd.0.is_null(), "CreateWindowExW 必须返回非空 HWND");
        assert!(
            unsafe { IsWindow(Some(hwnd)).as_bool() },
            "新建 owner 窗口必须是存活窗口"
        );

        // 端到端：以该 HWND 为 owner，清空后 owner 必须生效且能写入数据。
        assert!(
            open_clipboard_with_retry(Some(hwnd), 10),
            "以合法 owner 必须能打开剪贴板"
        );
        unsafe {
            assert!(EmptyClipboard().is_ok(), "以合法 owner 清空剪贴板必须成功");
            assert_eq!(
                GetClipboardOwner().ok(),
                Some(hwnd),
                "EmptyClipboard 后 GetClipboardOwner 必须等于该 HWND"
            );

            let wide: Vec<u16> = "qp_s02_probe".encode_utf16().chain(Some(0)).collect();
            let bytes = wide.len() * 2;
            let handle = prod_alloc(bytes).expect("分配必须成功");
            let ptr = prod_lock(handle);
            assert!(!ptr.is_null());
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr, bytes);
            prod_unlock(handle);
            assert!(
                prod_set(CF_UNICODETEXT, handle),
                "以合法 owner 写入必须成功"
            );
            let _ = CloseClipboard();
        }

        assert_eq!(get_clipboard_text().as_deref(), Some("qp_s02_probe"));

        // 清理测试窗口，避免残留。
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    /// 审计报告 R-06：owner 线程退出协议 —— 启动 → 停止 → 再启动。
    ///
    /// 断言：（1）初始 owner 存活；（2）`shutdown_clipboard_owner()` 通过确认通道
    /// 确认线程在超时内退出并销毁窗口；（3）关闭后可重建出**新的**有效窗口；
    /// （4）重建后临时文本注入/恢复语义仍然可用。
    #[test]
    fn test_owner_shutdown_and_recreate_cycle() {
        let _guard = clip_test_lock();

        let first = clipboard_owner().expect("必须能够创建进程级剪贴板 owner 窗口");
        assert!(
            unsafe { IsWindow(Some(first)).as_bool() },
            "初始 owner 必须是存活窗口"
        );

        // 退出协议必须确认线程在超时内退出（确认通道收到退出信号），并销毁窗口。
        assert!(
            shutdown_clipboard_owner(),
            "shutdown 必须确认 owner 线程在有界超时内退出"
        );
        assert!(
            !unsafe { IsWindow(Some(first)).as_bool() },
            "退出协议必须销毁原 owner 窗口"
        );

        // 再启动：必须重建出新的、有效的 owner 窗口。
        let second = clipboard_owner().expect("关闭后必须能够重建 owner 窗口");
        assert!(
            unsafe { IsWindow(Some(second)).as_bool() },
            "重建的 owner 必须是存活窗口"
        );
        assert_ne!(
            first.0 as usize, second.0 as usize,
            "重建必须得到新的窗口句柄（旧句柄已销毁）"
        );

        // 重建后临时文本注入/恢复语义必须仍然可用。
        let initial_text =
            get_clipboard_text().unwrap_or_else(|| "initial_qp_owner_cycle".to_string());
        let _ = set_raw_clipboard_text(&initial_text);
        let temp = "C:\\Windows\\System32";
        let result = with_temporary_clipboard_text(temp, || {
            assert_eq!(
                get_clipboard_text().unwrap_or_default(),
                temp,
                "重建后临时注入必须生效"
            );
            7
        });
        assert_eq!(result, Ok(7));
        assert_eq!(
            get_clipboard_text().unwrap_or_default(),
            initial_text,
            "重建后恢复语义必须仍然有效"
        );
    }

    /// 审计报告 R-06：重复 `shutdown_clipboard_owner()` 必须幂等、不 panic、不阻塞。
    #[test]
    fn test_owner_shutdown_is_idempotent_when_absent() {
        let _guard = clip_test_lock();

        let owner = clipboard_owner().expect("必须能够创建进程级剪贴板 owner 窗口");
        assert!(unsafe { IsWindow(Some(owner)).as_bool() });

        let started = std::time::Instant::now();
        assert!(shutdown_clipboard_owner(), "首次关闭必须成功确认线程退出");
        assert!(
            shutdown_clipboard_owner(),
            "无 owner 时再次关闭必须是 no-op 成功"
        );
        assert!(shutdown_clipboard_owner(), "第三次关闭同样必须幂等成功");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "重复关闭必须立即返回，不得阻塞；实际耗时 {:?}",
            started.elapsed()
        );
        assert!(
            !unsafe { IsWindow(Some(owner)).as_bool() },
            "关闭后原 owner 窗口不得存活"
        );
    }

    /// owner 创建期会合点守卫：无论用例如何结束，均清空注入的会合点，避免污染后续用例。
    struct OwnerHooksGuard;

    impl Drop for OwnerHooksGuard {
        fn drop(&mut self) {
            *TEST_OWNER_HOOKS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }

    /// 一次「就绪等待超时 → 取消握手」的确定性执行结果。
    struct ReadyTimeoutRun {
        /// `create_clipboard_owner()` 的返回值（超时路径必须为 None）；
        /// 以原始句柄回传以避开 `HWND` 非 `Send` 的限制。
        result: Option<usize>,
        /// owner 线程实际创建的窗口原始句柄（取消先于创建命中时为 None）
        created_hwnd: Option<usize>,
    }

    /// 在指定会合阶段驱动一次「超时取消」时序：
    /// 1. owner 线程在会合阶段阻塞（到达后由 `reached_rx` 上报）；
    /// 2. 用例主线程确认超时路径已置位取消标记（`cancel_rx` 上报）；
    /// 3. 放行 owner 线程，验证其观察到取消后干净退出。
    ///
    /// 全程以通道会合代替 `sleep` 猜时序，故结果确定：就绪超时由被阻塞的线程保证，
    /// 取消必先于放行置位；唯一的真实等待是 50ms 的就绪超时（远小于默认 3s）。
    fn run_ready_timeout_cancel(stage: OwnerCreationStage) -> ReadyTimeoutRun {
        let _hooks_guard = OwnerHooksGuard;

        // 清空既有 owner，确保 create 真正进入创建分支。
        assert!(shutdown_clipboard_owner(), "前置清空 owner 必须成功");

        let (reached_tx, reached_rx) = mpsc::channel::<OwnerCreationStage>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (cancel_tx, cancel_rx) = mpsc::channel::<()>();
        let hooks = Arc::new(OwnerTestHooks {
            stage,
            reached_tx,
            release_rx: Mutex::new(release_rx),
            cancel_tx,
            created_hwnd: Mutex::new(None),
            ready_timeout: Duration::from_millis(50),
        });
        *TEST_OWNER_HOOKS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(hooks.clone());

        let create_handle = thread::spawn(|| create_clipboard_owner().map(|hwnd| hwnd.0 as usize));

        let reached = reached_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("owner 线程必须到达目标会合阶段");
        assert_eq!(reached, stage, "owner 线程应在目标阶段会合");

        cancel_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("就绪等待超时后必须请求取消");

        // 审计报告 R-07：取消后的「确认线程退出」必须在有界超时内真正**确认**（而非
        // 退化到超时/detach 分支）。若创建方持 `OWNER` 等待，owner 线程的退出清理会与之
        // 互锁，这里会实测到接近 `OWNER_SHUTDOWN_TIMEOUT` 的耗时。
        let cancel_observed = std::time::Instant::now();
        release_tx.send(()).expect("放行 owner 线程必须成功");

        let result = create_handle.join().expect("创建线程不得 panic");
        let confirm_elapsed = cancel_observed.elapsed();
        assert!(
            confirm_elapsed < OWNER_SHUTDOWN_TIMEOUT,
            "R-07：取消后必须在有界超时内确认线程退出，实测 {:?}（若创建方持 OWNER 等待，\
             退出清理会与之互锁并退化到 {:?} 超时）",
            confirm_elapsed,
            OWNER_SHUTDOWN_TIMEOUT
        );

        let created_hwnd = *hooks
            .created_hwnd
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        ReadyTimeoutRun {
            result,
            created_hwnd,
        }
    }

    /// 审计报告 §4.1「超时取消」：就绪等待超时后必须先置取消标记并**确认** owner 线程
    /// 退出（而不是静默 detach）；OWNER 中不得残留状态，随后再次创建仍能成功。
    #[test]
    fn test_owner_ready_timeout_cancels_and_confirms_thread_exit() {
        let _guard = clip_test_lock();
        let run = run_ready_timeout_cancel(OwnerCreationStage::AfterClassRegistered);

        assert!(
            run.result.is_none(),
            "就绪等待超时后 create_clipboard_owner 必须返回 None"
        );
        assert!(
            run.created_hwnd.is_none(),
            "取消先于创建命中时，owner 线程不得创建任何窗口"
        );
        assert!(
            OWNER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none(),
            "取消确认退出后 OWNER 中不得残留任何状态"
        );

        // 不因残留状态而永久失败：随后再次创建必须成功且可正常关闭。
        let recreated = clipboard_owner().expect("取消确认后必须能重新创建 owner");
        assert!(
            unsafe { IsWindow(Some(recreated)).as_bool() },
            "重建的 owner 必须是存活窗口"
        );
        assert!(shutdown_clipboard_owner(), "重建的 owner 必须能正常关闭");
    }

    /// 审计报告 §4.1「迟到 ready」：ready 在等待超时**之后**才到达。owner 线程观察到
    /// 取消后必须回收已创建窗口、不进入消息泵，并被确认退出；create 返回 None。
    #[test]
    fn test_owner_ready_timeout_late_ready_after_window_is_reclaimed() {
        let _guard = clip_test_lock();
        let run = run_ready_timeout_cancel(OwnerCreationStage::AfterWindowCreated);

        assert!(
            run.result.is_none(),
            "就绪等待超时后 create_clipboard_owner 必须返回 None"
        );
        let raw = run.created_hwnd.expect("该时序下 owner 线程应已创建窗口");
        assert!(
            !unsafe { IsWindow(Some(HWND(raw as *mut _))).as_bool() },
            "迟到 ready + 取消必须回收已创建窗口，绝不留下无主窗口"
        );
        assert!(
            OWNER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none(),
            "取消确认退出后 OWNER 中不得残留任何状态"
        );

        let recreated = clipboard_owner().expect("取消确认后必须能重新创建 owner");
        assert!(
            unsafe { IsWindow(Some(recreated)).as_bool() },
            "重建的 owner 必须是存活窗口"
        );
        assert!(shutdown_clipboard_owner(), "重建的 owner 必须能正常关闭");
    }

    /// 审计报告 §4.1「取消与创建并发」：取消与 `CreateWindowExW` 并发时，两种时序都
    /// 不得留下无主窗口 —— 要么窗口创建后被正常关闭回收（`IsWindow == false`），
    /// 要么取消先命中而根本不创建。
    #[test]
    fn test_owner_ready_timeout_cancel_races_window_creation_without_orphan() {
        let _guard = clip_test_lock();

        // 时序一：取消先于创建命中 —— 根本不创建窗口。
        let before = run_ready_timeout_cancel(OwnerCreationStage::AfterClassRegistered);
        assert!(before.result.is_none(), "时序一：create 必须返回 None");
        assert!(
            before.created_hwnd.is_none(),
            "时序一：取消先命中时不得创建窗口"
        );

        // 时序二：窗口已创建后才命中取消 —— 必须被正常关闭回收。
        let after = run_ready_timeout_cancel(OwnerCreationStage::AfterWindowCreated);
        assert!(after.result.is_none(), "时序二：create 必须返回 None");
        let raw = after.created_hwnd.expect("时序二：owner 线程应已创建窗口");
        assert!(
            !unsafe { IsWindow(Some(HWND(raw as *mut _))).as_bool() },
            "时序二：已创建窗口必须被回收（IsWindow == false），不得成为无主窗口"
        );

        assert!(
            OWNER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none(),
            "两种时序下 OWNER 均不得残留状态"
        );
    }

    /// P0-03：构建一个真实存在的临时目录（事务链路需取回“真实目录”才算成功）。
    fn make_probe_dir(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("quickpath_p003_probe_{tag}"));
        std::fs::create_dir_all(&dir).expect("测试目录必须可创建");
        dir.to_string_lossy().trim_end_matches('\\').to_string()
    }

    /// P0-03：两条管理器链的共享夹具：链名 + 两个真实临时目录。
    ///
    /// `probe_panel_path` 只负责「清空 → 请求写入 → 读回 → 校验」，
    /// 管理器本身只负责“发什么消息”；两条链各用一份夹具驱动同一套共享流程。
    struct ChainFixture {
        label: &'static str,
        src: String,
        trg: String,
    }

    impl ChainFixture {
        fn new(label: &'static str, tag: &str) -> Self {
            Self {
                label,
                src: make_probe_dir(&format!("{tag}_src")),
                trg: make_probe_dir(&format!("{tag}_trg")),
            }
        }
    }

    /// 用例 1：第一次管理器写入后、第二次清空前注入用户复制
    fn chain_case1_blocks_second_clear_after_user_copy(f: &ChainFixture) {
        let (label, src_path, trg_path) = (f.label, f.src.as_str(), f.trg.as_str());
        // ---- 用例 1：第一次管理器写入后、第二次清空前注入用户复制 ----
        {
            let seed = format!("qp_p003_{label}_seed1");
            assert!(set_raw_clipboard_text(&seed), "{label}: 播种剪贴板必须成功");
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

            let first = probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(src_path);
                    Some(1)
                },
            )
            .expect("首次写入不应触发归属冲突");
            assert!(first.delivered, "{label}: 首次探测必须视为对端已应答");

            // 用户在扫描未结束时复制了新内容
            let user_new = format!("qp_p003_{label}_user_new_1");
            assert!(set_raw_clipboard_text(&user_new));

            // 第二次清空前的写前校验必须拒绝写入（且不得执行 request）
            let err = probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(trg_path);
                    Some(1)
                },
            )
            .expect_err("检出外部改写后必须拒绝后续写入");
            assert!(
                matches!(err, ClipboardTransactionError::NotOwned),
                "{label}: 期望 NotOwned，实际 {:?}",
                err
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(user_new.as_str()),
                "{label}: 用户新复制的内容必须保持不变"
            );

            let outcome = tx.restore_if_still_owned();
            assert_eq!(
                outcome,
                ClipboardTransactionOutcome::SkippedForeignChange,
                "{label}: 恢复阶段也必须放弃覆盖"
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(user_new.as_str()),
                "{label}: 用户新内容绝不能被旧快照覆盖"
            );
        }
    }

    /// 用例 2：最后一次管理器写入后、恢复前注入用户复制
    fn chain_case2_skips_restore_after_late_user_copy(f: &ChainFixture) {
        let (label, src_path, trg_path) = (f.label, f.src.as_str(), f.trg.as_str());
        // ---- 用例 2：最后一次管理器写入后、恢复前注入用户复制 ----
        {
            let seed = format!("qp_p003_{label}_seed2");
            assert!(set_raw_clipboard_text(&seed));
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(src_path);
                    Some(1)
                },
            )
            .expect("首次探测必须成功");
            probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(trg_path);
                    Some(1)
                },
            )
            .expect("第二次探测必须成功");

            let user_new = format!("qp_p003_{label}_user_new_2");
            assert!(set_raw_clipboard_text(&user_new));

            let outcome = tx.restore_if_still_owned();
            assert_eq!(
                outcome,
                ClipboardTransactionOutcome::SkippedForeignChange,
                "{label}: 恢复前发现外部改写必须返回 SkippedForeignChange"
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(user_new.as_str()),
                "{label}: 绝不恢复旧快照"
            );
        }
    }

    /// 用例 3：用户复制内容与临时文本恰好相同，仍须按 sequence 识别外部改写
    fn chain_case3_detects_sequence_change_with_identical_text(f: &ChainFixture) {
        let (label, src_path, _trg_path) = (f.label, f.src.as_str(), f.trg.as_str());
        // ---- 用例 3：用户复制内容与临时文本恰好相同，仍须按 sequence 识别外部改写 ----
        {
            let seed = format!("qp_p003_{label}_seed3");
            assert!(set_raw_clipboard_text(&seed));
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            let probe = probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(src_path);
                    Some(1)
                },
            )
            .expect("首次探测必须成功");
            assert!(probe.delivered);

            // 用户复制的内容与临时文本完全相同：序列号已变，仍必须识别为外部改写
            assert!(set_raw_clipboard_text(src_path));

            let outcome = tx.restore_if_still_owned();
            assert_eq!(
                outcome,
                ClipboardTransactionOutcome::SkippedForeignChange,
                "{label}: 文本恰好相同也必须按 sequence 识别外部改写"
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(src_path),
                "{label}: 用户新内容必须保持不变"
            );
        }
    }

    /// 用例 4：无并发改写时，六类格式必须逐字节保真恢复
    fn chain_case4_restores_mixed_formats(f: &ChainFixture) {
        let (label, src_path, _trg_path) = (f.label, f.src.as_str(), f.trg.as_str());
        // ---- 用例 4：无并发改写时，六类格式必须逐字节保真恢复 ----
        {
            let html_format = unsafe {
                windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                    "HTML Format"
                ))
            };
            let rtf_format = unsafe {
                windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                    "Rich Text Format"
                ))
            };
            let custom_format = unsafe {
                windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                    "QuickPathP003Format"
                ))
            };
            assert!(
                html_format != 0 && rtf_format != 0 && custom_format != 0,
                "{label}: HTML/RTF/自定义格式注册必须成功"
            );

            let html = "<html><body>QuickPath P003 HTML 中文</body></html>"
                .as_bytes()
                .to_vec();
            let rtf = r"{\rtf1\ansi QuickPath P003 RTF 中文}".as_bytes().to_vec();
            let custom = b"quickpath-p003-custom-hglobal".to_vec();
            let hdrop = hdrop_blob(&[r"C:\QuickPath\p003\a.txt", r"D:\QuickPath\p003\b.bin"]);
            let dib = minimal_dib_blob();

            // seed_clipboard_formats 会额外写入 CF_UNICODETEXT
            seed_clipboard_formats(&[
                (html_format, &html),
                (rtf_format, &rtf),
                (custom_format, &custom),
                (CF_HDROP, &hdrop),
                (CF_DIB, &dib[..]),
            ]);

            let before = ClipboardSnapshot::capture().expect("混合格式剪贴板必须可安全捕获");
            for fmt in [html_format, rtf_format, CF_HDROP, CF_DIB, custom_format] {
                assert!(
                    before.items.iter().any(|(f, _)| *f == fmt),
                    "{label}: 快照必须含格式 {}",
                    fmt
                );
            }

            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            let probe = probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(src_path);
                    Some(1)
                },
            )
            .expect("无并发改写时探测必须成功");
            assert!(probe.delivered, "{label}");

            let outcome = tx.restore_if_still_owned();
            assert_eq!(
                outcome,
                ClipboardTransactionOutcome::Committed,
                "{label}: 无并发改写时必须正常恢复"
            );
            assert_mixed_roundtrip(&before, &format!("{label}: 事务链路"));
        }
    }

    /// 用例 5：剪贴板忙 / 读回失败 / 序列号不可读 / 管理器超时均不得覆盖当前内容
    fn chain_case5_never_overwrites_when_ownership_unknown(f: &ChainFixture) {
        let (label, _src_path, _trg_path) = (f.label, f.src.as_str(), f.trg.as_str());

        // ---- 用例 5(a)：开启事务时剪贴板忙 → 直接失败且完全不碰剪贴板 ----
        {
            let seed = format!("qp_p003_{label}_busy_seed");
            assert!(set_raw_clipboard_text(&seed));
            {
                let _busy = BusyInjection::enable();
                assert_eq!(
                    ClipboardTransaction::begin().err(),
                    Some(ClipboardSafetyError::Busy),
                    "{label}: 占用时开启事务必须报 Busy"
                );
            }
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(seed.as_str()),
                "{label}: 开启事务失败时原剪贴板必须保持不变"
            );
        }

        // ---- 用例 5(b)：写入时剪贴板忙 → 归属未知 → ReadFailed 且不覆盖 ----
        {
            let seed = format!("qp_p003_{label}_busy_write_seed");
            assert!(set_raw_clipboard_text(&seed));
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            {
                let _busy = BusyInjection::enable();
                assert!(
                    tx.write_owned_text("").is_err(),
                    "{label}: 占用时临时写入必须失败"
                );
            }
            let current = format!("qp_p003_{label}_busy_current");
            assert!(set_raw_clipboard_text(&current));
            assert_eq!(
                tx.restore_if_still_owned(),
                ClipboardTransactionOutcome::SkippedUnknownOwner,
                "{label}: 归属未知时必须保守放弃恢复"
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(current.as_str()),
                "{label}: 归属未知时绝不能用旧快照覆盖"
            );
        }

        // ---- 用例 5(c)：读回失败 → 归属未知 → ReadFailed 且不覆盖 ----
        {
            let seed = format!("qp_p003_{label}_readfail_seed");
            assert!(set_raw_clipboard_text(&seed));
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            tx.write_owned_text("").expect("临时写入必须成功");
            {
                let _busy = BusyInjection::enable();
                let marker = tx.request_marker();
                assert!(
                    tx.note_observed_owned_text_proven(marker, |_| true)
                        .is_err(),
                    "{label}: 读回失败必须按归属未知失败"
                );
            }
            let current = format!("qp_p003_{label}_readfail_current");
            assert!(set_raw_clipboard_text(&current));
            assert_eq!(
                tx.restore_if_still_owned(),
                ClipboardTransactionOutcome::SkippedUnknownOwner,
                "{label}"
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(current.as_str()),
                "{label}"
            );
        }

        // ---- 用例 5(d)：序列号不可读 → 归属未知 → ReadFailed 且不覆盖 ----
        {
            let seed = format!("qp_p003_{label}_seqzero_seed");
            assert!(set_raw_clipboard_text(&seed));
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            tx.write_owned_text("").expect("临时写入必须成功");
            {
                let _zero = SeqZeroInjection::enable();
                assert!(
                    tx.ensure_still_owned().is_err(),
                    "{label}: 序列号不可读时必须判为归属未知"
                );
            }
            let current = format!("qp_p003_{label}_seqzero_current");
            assert!(set_raw_clipboard_text(&current));
            assert_eq!(
                tx.restore_if_still_owned(),
                ClipboardTransactionOutcome::SkippedUnknownOwner,
                "{label}"
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(current.as_str()),
                "{label}"
            );
        }

        // ---- 用例 5(e)：管理器超时 / 窗口退出 → 归属未变 → 必须正常恢复用户原内容 ----
        {
            let seed = format!("qp_p003_{label}_timeout_seed");
            assert!(set_raw_clipboard_text(&seed));
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            let probe = probe_panel_path(&mut tx, |_| true, || None).expect("超时不应产生归属冲突");
            assert!(
                !probe.delivered && probe.path.is_none(),
                "{label}: 超时不得产生结果"
            );
            let outcome = tx.restore_if_still_owned();
            assert_eq!(
                outcome,
                ClipboardTransactionOutcome::Committed,
                "{label}: 归属未变时必须正常恢复"
            );
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(seed.as_str()),
                "{label}: 用户原内容必须原样还回，临时清空不得残留"
            );
        }
    }

    /// 用例 6：正常成功路径取回正确目录，且不残留管理器临时文本
    fn chain_case6_returns_correct_dirs_and_leaves_no_temp_text(f: &ChainFixture) {
        let (label, src_path, trg_path) = (f.label, f.src.as_str(), f.trg.as_str());
        // ---- 用例 6：正常成功路径取回正确目录，且不残留管理器临时文本 ----
        {
            let seed = format!("qp_p003_{label}_success_seed");
            assert!(set_raw_clipboard_text(&seed));
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

            let src = probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(src_path);
                    Some(1)
                },
            )
            .expect("首次探测必须成功");
            let trg = probe_panel_path(
                &mut tx,
                |_| true,
                || {
                    let _ = set_raw_clipboard_text(trg_path);
                    Some(1)
                },
            )
            .expect("第二次探测必须成功");

            assert_eq!(
                src.path.as_deref(),
                Some(src_path.trim_end_matches('\\')),
                "{label}: 源目录必须正确"
            );
            assert_eq!(
                trg.path.as_deref(),
                Some(trg_path.trim_end_matches('\\')),
                "{label}: 目标目录必须正确"
            );

            let outcome = tx.restore_if_still_owned();
            assert_eq!(outcome, ClipboardTransactionOutcome::Committed, "{label}");
            assert_eq!(
                get_clipboard_text().as_deref(),
                Some(seed.as_str()),
                "{label}: 扫描结束后剪贴板必须是用户原内容"
            );
            assert_ne!(
                get_clipboard_text().as_deref(),
                Some(trg_path.trim_end_matches('\\')),
                "{label}: 不得残留管理器临时文本"
            );
        }
    }

    /// 报告 §2.5：Total Commander 链路用例 1 —— 二次清空前检出用户复制并停止写入。
    #[test]
    fn test_totalcmd_chain_case1_blocks_second_clear_after_user_copy() {
        let _guard = clip_test_lock();
        chain_case1_blocks_second_clear_after_user_copy(&ChainFixture::new("totalcmd", "tc"));
    }

    /// 报告 §2.5：Total Commander 链路用例 2 —— 恢复前检出用户复制，放弃恢复。
    #[test]
    fn test_totalcmd_chain_case2_skips_restore_after_late_user_copy() {
        let _guard = clip_test_lock();
        chain_case2_skips_restore_after_late_user_copy(&ChainFixture::new("totalcmd", "tc"));
    }

    /// 报告 §2.5：Total Commander 链路用例 3 —— 文本恰好相同也按 sequence 识别外部改写。
    #[test]
    fn test_totalcmd_chain_case3_detects_sequence_change_with_identical_text() {
        let _guard = clip_test_lock();
        chain_case3_detects_sequence_change_with_identical_text(&ChainFixture::new(
            "totalcmd", "tc",
        ));
    }

    /// 报告 §2.5：Total Commander 链路用例 4 —— 六类格式逐字节保真恢复。
    #[test]
    fn test_totalcmd_chain_case4_restores_mixed_formats() {
        let _guard = clip_test_lock();
        chain_case4_restores_mixed_formats(&ChainFixture::new("totalcmd", "tc"));
    }

    /// 报告 §2.5：Total Commander 链路用例 5 —— 忙/读回失败/序列号不可读/超时均不覆盖。
    #[test]
    fn test_totalcmd_chain_case5_never_overwrites_when_ownership_unknown() {
        let _guard = clip_test_lock();
        chain_case5_never_overwrites_when_ownership_unknown(&ChainFixture::new("totalcmd", "tc"));
    }

    /// 报告 §2.5：Total Commander 链路用例 6 —— 取回正确目录且不残留临时文本。
    #[test]
    fn test_totalcmd_chain_case6_returns_correct_dirs_and_leaves_no_temp_text() {
        let _guard = clip_test_lock();
        chain_case6_returns_correct_dirs_and_leaves_no_temp_text(&ChainFixture::new(
            "totalcmd", "tc",
        ));
    }

    /// 报告 §2.5：XYplorer 链路用例 1 —— 二次清空前检出用户复制并停止写入。
    #[test]
    fn test_xyplorer_chain_case1_blocks_second_clear_after_user_copy() {
        let _guard = clip_test_lock();
        chain_case1_blocks_second_clear_after_user_copy(&ChainFixture::new("xyplorer", "xy"));
    }

    /// 报告 §2.5：XYplorer 链路用例 2 —— 恢复前检出用户复制，放弃恢复。
    #[test]
    fn test_xyplorer_chain_case2_skips_restore_after_late_user_copy() {
        let _guard = clip_test_lock();
        chain_case2_skips_restore_after_late_user_copy(&ChainFixture::new("xyplorer", "xy"));
    }

    /// 报告 §2.5：XYplorer 链路用例 3 —— 文本恰好相同也按 sequence 识别外部改写。
    #[test]
    fn test_xyplorer_chain_case3_detects_sequence_change_with_identical_text() {
        let _guard = clip_test_lock();
        chain_case3_detects_sequence_change_with_identical_text(&ChainFixture::new(
            "xyplorer", "xy",
        ));
    }

    /// 报告 §2.5：XYplorer 链路用例 4 —— 六类格式逐字节保真恢复。
    #[test]
    fn test_xyplorer_chain_case4_restores_mixed_formats() {
        let _guard = clip_test_lock();
        chain_case4_restores_mixed_formats(&ChainFixture::new("xyplorer", "xy"));
    }

    /// 报告 §2.5：XYplorer 链路用例 5 —— 忙/读回失败/序列号不可读/超时均不覆盖。
    #[test]
    fn test_xyplorer_chain_case5_never_overwrites_when_ownership_unknown() {
        let _guard = clip_test_lock();
        chain_case5_never_overwrites_when_ownership_unknown(&ChainFixture::new("xyplorer", "xy"));
    }

    /// 报告 §2.5：XYplorer 链路用例 6 —— 取回正确目录且不残留临时文本。
    #[test]
    fn test_xyplorer_chain_case6_returns_correct_dirs_and_leaves_no_temp_text() {
        let _guard = clip_test_lock();
        chain_case6_returns_correct_dirs_and_leaves_no_temp_text(&ChainFixture::new(
            "xyplorer", "xy",
        ));
    }

    /// P0-03：恢复动作本身失败时必须回报 `RestoreFailed`（携带底层错误），且结局幂等。
    #[test]
    fn test_transaction_reports_restore_failure() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p003_tx_restore_fail_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
        tx.write_owned_text("").expect("临时写入必须成功");

        {
            let _inject = FailInjection::enable(CF_UNICODETEXT);
            match tx.restore_if_still_owned() {
                ClipboardTransactionOutcome::RestorePartialFailure(err) => {
                    assert!(
                        matches!(err, ClipboardRestoreError::PartialRestore(ref v) if v.contains(&CF_UNICODETEXT)),
                        "恢复失败必须是含 CF_UNICODETEXT 的 PartialRestore，实际 {:?}",
                        err
                    );
                }
                other => panic!("期望 RestorePartialFailure，实际 {:?}", other),
            }
        }

        // 结局幂等：再次调用不得重新动作
        assert!(matches!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::RestorePartialFailure(_)
        ));

        let _ = set_raw_clipboard_text("qp_p003_tx_restore_fail_seed");
    }

    /// P0-03：RAII 兜底 —— 调用方忘记显式恢复时，`Drop` 也必须恢复用户原内容。
    #[test]
    fn test_transaction_drop_restores_when_caller_forgets() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p003_drop_seed"));
        {
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            tx.write_owned_text("").expect("临时写入必须成功");
            // 故意不调用 restore_if_still_owned，验证 Drop 兜底
        }
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p003_drop_seed"),
            "Drop 兜底必须恢复用户原内容"
        );
    }

    /// P0-03：RAII 兜底同样走严格归属校验 —— 外部改写时 `Drop` 绝不覆盖用户新内容。
    #[test]
    fn test_transaction_drop_skips_restore_after_foreign_change() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p003_drop2_seed"));
        {
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            tx.write_owned_text("").expect("临时写入必须成功");
            assert!(set_raw_clipboard_text("qp_p003_drop2_user"));
        }
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p003_drop2_user"),
            "Drop 兜底检出外部改写时绝不覆盖"
        );
    }

    /// 审计报告 P0-04 竞态 A：写前校验通过后、真正写入之前发生外部改写，
    /// 持锁复核必须检出并拒绝写入（绝不 EmptyClipboard 用户刚复制的内容）。
    #[test]
    fn test_write_owned_text_rechecks_under_clipboard_lock_and_fails_closed() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p004a_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
        tx.write_owned_text("qp_p004a_first")
            .expect("首次写入必须成功");

        // 在「写前校验」与「持锁复核」之间注入用户复制（等价竞态 A 的交错）
        let user_text = "qp_p004a_user_copy".to_string();
        arm_atomic_write_hook(0, move || {
            assert!(set_raw_clipboard_text(&user_text));
        });

        let err = tx
            .write_owned_text("qp_p004a_second")
            .expect_err("持锁复核必须检出竞态 A 的外部改写");
        assert_eq!(
            err,
            ClipboardTransactionError::NotOwned,
            "必须返回 NotOwned"
        );
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p004a_user_copy"),
            "用户刚复制的内容绝不能被 EmptyClipboard 清掉"
        );

        let _ = set_raw_clipboard_text("");
    }

    /// 审计报告 P-05：首个面板超时后必须短路，不再发起第二个面板请求。
    #[test]
    fn test_probe_two_panels_short_circuits_when_first_times_out() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p005_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        let second_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = second_called.clone();
        let (first, second) = probe_two_panels(
            &mut tx,
            |_| true,
            || None, // 首个面板超时
            move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Some(1)
            },
        )
        .expect("超时不应产生归属冲突");

        assert!(!first.delivered, "首个面板必须记为未投递");
        assert!(second.is_none(), "首个面板超时后不得探测第二个面板");
        assert!(
            !second_called.load(std::sync::atomic::Ordering::SeqCst),
            "第二个面板的 request 闭包绝不能被调用（否则会多付一次 500ms）"
        );
        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::Committed,
            "归属未变时必须正常恢复"
        );
        assert_eq!(get_clipboard_text().as_deref(), Some("qp_p005_seed"));
    }

    /// 审计报告 P0-06：确定性并发会合测试。
    ///
    /// 把首个请求暂停在「已写空、未收到回写」处，此时启动第二个请求：它必须被
    /// **进程级独占 lease** 拒绝（`AlreadyRunning`），且剪贴板序列号不变（零触碰）。
    /// 这直接复现报告描述的「两次并发探测各自快照 → 双双放弃恢复 → 原内容丢失」场景。
    #[test]
    fn test_second_probe_is_rejected_with_zero_clipboard_touch() {
        use crate::managers::explicit_probe::{
            ExplicitProbeOutcome, ScriptedManager, TriggerSnapshot, probe_with_snapshot,
        };
        use crate::managers::probe_lease::ProbeLease;
        use std::time::{Duration, Instant};

        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p006_original"));

        crate::managers::clipboard_probe::test_gate::arm();
        let first = std::thread::spawn(move || {
            let _lease = ProbeLease::try_acquire().expect("首请求必须拿到独占 lease");
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            // request 闭包永不执行：请求在 gate 处就被挂起
            let _ = probe_panel_path(&mut tx, |_| false, || None);
            let _ = tx.restore_if_still_owned();
        });

        let reached =
            crate::managers::clipboard_probe::test_gate::wait_until_paused(Duration::from_secs(10));
        // 无论是否到达会合点，都必须释放 gate，否则线程会永久阻塞
        if !reached {
            crate::managers::clipboard_probe::test_gate::release();
            let _ = first.join();
            panic!("首请求未能在超时内到达「已写空、未收到回写」会合点");
        }

        let sequence_at_pause = clipboard_sequence_number();
        assert_ne!(sequence_at_pause, 0, "会合点处剪贴板序列号必须可读");

        let snapshot = TriggerSnapshot {
            hwnd_raw: 0,
            pid: 0,
            manager: ScriptedManager::TotalCmd,
            captured_at: Instant::now(),
            request_id: 1,
        };
        assert_eq!(
            probe_with_snapshot(&snapshot, true),
            ExplicitProbeOutcome::AlreadyRunning,
            "首请求仍持有 lease 时第二个请求必须被拒绝"
        );
        assert_eq!(
            clipboard_sequence_number(),
            sequence_at_pause,
            "被拒绝的第二个请求不得触碰剪贴板（零写入、零清空）"
        );

        crate::managers::clipboard_probe::test_gate::release();
        first.join().expect("首请求线程不得 panic");
        let released = ProbeLease::try_acquire();
        assert!(released.is_some(), "首请求结束后必须释放 lease");
        drop(released);
    }

    /// 审计报告 P0-04 竞态 B：请求窗口内出现非目标来源的剪贴板更新
    /// （用户在管理器写入前复制），绝不允许无条件认领最终状态。
    #[test]
    fn test_probe_rejects_foreign_update_before_manager_write() {
        let _guard = clip_test_lock();
        let dir = make_probe_dir("p004b_src");
        let own_pid = std::process::id();
        let foreign_pid = own_pid.wrapping_add(1);
        assert!(set_raw_clipboard_text("qp_p004b_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        let probe_dir = dir.clone();
        let err = probe_panel_path(
            &mut tx,
            move |pid| pid == own_pid,
            move || {
                // 用户复制（合成事件：同进程的真实写入无法与“管理器”区分 pid，
                // 故用注入事件确定性地表达“非目标来源的一次更新”）
                inject_clipboard_update_event_for_test(foreign_pid);
                // 管理器随后写入路径
                let _ = set_raw_clipboard_text(&probe_dir);
                Some(1)
            },
        )
        .expect_err("请求期间出现外部更新必须判为 NotOwned");
        assert_eq!(err, ClipboardTransactionError::NotOwned);

        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::SkippedForeignChange,
            "检出外部更新后必须跳过旧快照恢复"
        );
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some(dir.as_str()),
            "不得写回旧快照覆盖当前内容"
        );
    }

    /// 审计报告 P0-04 竞态 B 回归：本次事务自己的临时写入会投递**延迟**的
    /// `WM_CLIPBOARDUPDATE`，其 owner 就是本进程；它绝不能被当成「外部改写」。
    /// 这里用「永不匹配」的 `is_expected_source` 把「只接受目标管理器」推到极端，
    /// 暴露该误判：若误判，管理器已正确写入时合法扫描仍会被拒。
    #[test]
    fn test_probe_does_not_treat_own_updates_as_foreign() {
        let _guard = clip_test_lock();
        let dir = make_probe_dir("p004b_own");
        assert!(set_raw_clipboard_text("qp_p004b_own_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        let probe_dir = dir.clone();
        let panel = probe_panel_path(
            &mut tx,
            |_| false, // 极端：目标管理器身份永不匹配
            move || {
                let _ = set_raw_clipboard_text(&probe_dir);
                Some(1)
            },
        )
        .expect("本进程自身的写入事件绝不能被判为外部改写");
        assert_eq!(panel.path.as_deref(), Some(dir.trim_end_matches('\\')));
        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::Committed
        );
        assert_eq!(get_clipboard_text().as_deref(), Some("qp_p004b_own_seed"));
    }

    /// 审计报告 P0-04 竞态 B（正面）：目标管理器写入且无第三方更新时，
    /// 必须能正常认领并读回路径。
    #[test]
    fn test_probe_accepts_manager_write_when_provenance_is_clean() {
        let _guard = clip_test_lock();
        let dir = make_probe_dir("p004b_ok");
        let own_pid = std::process::id();
        assert!(set_raw_clipboard_text("qp_p004b_ok_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        let probe_dir = dir.clone();
        let panel = probe_panel_path(
            &mut tx,
            move |pid| pid == own_pid,
            move || {
                let _ = set_raw_clipboard_text(&probe_dir);
                Some(1)
            },
        )
        .expect("无第三方更新时必须成功认领");
        assert_eq!(panel.path.as_deref(), Some(dir.trim_end_matches('\\')));
        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::Committed
        );
        assert_eq!(get_clipboard_text().as_deref(), Some("qp_p004b_ok_seed"));
    }

    /// 审计报告 P0-04 竞态 B：管理器写入后、回读前用户从**其他进程**复制，
    /// 最终 owner 不再属于目标管理器 → 必须拒绝认领并保留用户内容。
    #[test]
    fn test_probe_rejects_foreign_final_owner_after_manager_write() {
        let _guard = clip_test_lock();
        let dir = make_probe_dir("p004b_late");
        let own_pid = std::process::id();
        assert!(set_raw_clipboard_text("qp_p004b_late_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        let probe_dir = dir.clone();
        let err = probe_panel_path(
            &mut tx,
            move |pid| pid == own_pid,
            move || {
                // 管理器写入路径……
                let _ = set_raw_clipboard_text(&probe_dir);
                // ……用户在回读前从独立进程复制
                let _ = clip_write_from_external_process("qp_p004b_late_user");
                Some(1)
            },
        )
        .expect_err("最终 owner 已非目标管理器时必须拒绝认领");
        assert!(
            matches!(
                err,
                ClipboardTransactionError::NotOwned | ClipboardTransactionError::OwnershipUnknown
            ),
            "必须是 NotOwned 或归属未知（均 fail-closed），实际 {:?}",
            err
        );
        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::SkippedForeignChange
        );
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p004b_late_user"),
            "用户跨进程复制的内容必须原样保留"
        );
    }

    /// 审计报告 P0-04 竞态 B：事件环溢出时来源链不可证明 → 必须保守拒绝。
    #[test]
    fn test_clipboard_update_ring_overflow_is_unprovable() {
        let _guard = clip_test_lock();
        let marker = clipboard_update_marker();
        for _ in 0..=CLIPBOARD_EVENT_CAPACITY {
            inject_clipboard_update_event_for_test(1234);
        }
        assert!(
            clipboard_updates_since(marker).is_none(),
            "事件溢出有界环后必须判定为来源链不可证明"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 真实运行时 / 跨进程证据（2026-10-14 补；对应整改报告 §5.1 可自动化子集）
    //
    // 与前面用例的差别：「管理器写入」与「用户并发复制」均由**独立 OS 进程**完成
    // （系统自带 `cmd.exe` + `clip.exe`），剪贴板内容与 `GetClipboardSequenceNumber`
    // 全部是系统真实状态，进程内不 mock 任何剪贴板行为。
    //
    // 目的：在本机未安装 Total Commander / XYplorer 的前提下，尽可能逼近报告 §2.6
    // 第 4 条「真实 Windows 11 上复现两个竞态窗口」。
    // **仍未覆盖**（必须真机人工执行）：真实 totalcmd.exe / xyplorer.exe 回写路径的时序、
    // 真实管理器卡顿/假死、人工操作的随机节奏 —— 见 `Document/真机验收清单_竞态与生命周期_v1.2.3.md` §6/§8.2。
    // ══════════════════════════════════════════════════════════════════════════

    /// 用 `powershell` / `pwsh` 的 `Set-Clipboard` 从**独立进程**精确写入剪贴板。
    ///
    /// 与 `set_raw_clipboard_text` 的关键区别：写入发生在**另一个进程**，因此
    /// `GetClipboardSequenceNumber` 的变化来自真实跨进程写，更贴近真实管理器行为。
    ///
    /// 为何不用 `cmd echo text|clip`：`echo` 会附加 CRLF，读回文本与预期不符
    /// （本文件第一版跨进程夹具就是这样被自已绊倒的，断言里能看到 `...user_b\r\n`）。
    /// `Set-Clipboard -Value` 写入的是精确字符串（已实测：14 字符写入读回 14 字符）。
    fn clip_write_from_external_process(text: &str) -> bool {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        assert!(
            !text.contains(['\'', '"', '`', '\n', '\r']),
            "跨进程写入的探针文本受控，不得包含引号或换行"
        );
        let script = format!("Set-Clipboard -Value '{text}'");
        // PS 5.1 起自带 Set-Clipboard；优先 pwsh，退化到系统自带 powershell。
        // `CREATE_NO_WINDOW`：必须在**后台**运行，否则每轮 `cargo test` 都会在桌面
        // 闪出控制台窗口（用户明确要求「后台运行，不要弹出」）。
        for exe in ["pwsh", "powershell"] {
            let ok = Command::new(exe)
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .creation_flags(crate::managers::ipc::CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return true;
            }
        }
        false
    }

    /// 回归守卫：跨进程剪贴板夹具必须以后台无窗方式运行，
    /// 否则每次 `cargo test` 都会在桌面闪出 pwsh / powershell 控制台窗口。
    #[test]
    fn test_external_clipboard_fixture_never_pops_console_window() {
        const SOURCE: &str = include_str!("clipboard.rs");
        // 用 concat! 拆开字面量：一是避免守卫自身源码把断言命中，
        // 二是只认「实际调用表达式」而不是夹具注释里提到的常量名。
        let needle = concat!(
            ".creation_flags(crate::managers::ipc::",
            "CREATE_NO_WINDOW)"
        );
        let start = SOURCE
            .find("fn clip_write_from_external_process")
            .expect("跨进程剪贴板夹具必须存在");
        let rest = &SOURCE[start..];
        let end = rest.find("\n    /// ").expect("夹具后面应紧跟文档注释");
        assert!(
            rest[..end].contains(needle),
            "跨进程剪贴板夹具必须用 CREATE_NO_WINDOW 后台运行，否则会在桌面闪控制台窗口"
        );
    }

    /// 报告 §5.1 用例 1（C-06）：跨进程真实复现「第一次管理器写入后、第二次清空前用户复制」——
    /// 第二次写入必须被拒绝，用户新内容原样保留，最终结局为 `SkippedForeignChange`。
    #[test]
    fn test_xproc_case1_user_copy_after_first_manager_write_blocks_second_clear() {
        let _guard = clip_test_lock();
        let d1 = make_probe_dir("xproc_case1_src");
        let d2 = make_probe_dir("xproc_case1_trg");

        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        // 第一次探测：由独立进程扮演管理器写入真实目录路径
        let p1 = crate::managers::clipboard_probe::probe_panel_path(
            &mut tx,
            |_| true,
            || clip_write_from_external_process(&d1).then_some(0),
        )
        .expect("跨进程管理器写入后的读回必须成功");
        assert_eq!(
            p1.path.as_deref(),
            Some(d1.as_str()),
            "必须取回真实目录路径"
        );

        // 竞态窗口：用户在第二次清空之前从独立进程复制了新内容
        assert!(
            clip_write_from_external_process("qp_p003_xproc_user_a"),
            "跨进程并发复制必须成功"
        );

        // 第二次写入前必须检出外部改写并拒绝
        let err = crate::managers::clipboard_probe::probe_panel_path(
            &mut tx,
            |_| true,
            || {
                let _ = clip_write_from_external_process(&d2);
                Some(0)
            },
        )
        .expect_err("第二次清空前检出外部改写必须拒绝写入");
        assert_eq!(err, ClipboardTransactionError::NotOwned, "必须是 NotOwned");

        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::SkippedForeignChange,
            "最终结局必须是跳过恢复，绝不写回旧快照"
        );
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p003_xproc_user_a"),
            "用户跨进程复制的新内容必须原样保留"
        );
    }

    /// 报告 §5.1 用例 2（C-07）：跨进程真实复现「最后一次管理器写入后、恢复前用户复制」——
    /// 恢复阶段必须跳过旧快照。
    #[test]
    fn test_xproc_case2_user_copy_after_last_manager_write_skips_restore() {
        let _guard = clip_test_lock();
        let d1 = make_probe_dir("xproc_case2_src");

        assert!(
            clip_write_from_external_process("qp_p003_xproc_case2_seed"),
            "种子写入必须成功"
        );

        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
        let p1 = crate::managers::clipboard_probe::probe_panel_path(
            &mut tx,
            |_| true,
            || clip_write_from_external_process(&d1).then_some(0),
        )
        .expect("跨进程管理器写入后的读回必须成功");
        assert_eq!(p1.path.as_deref(), Some(d1.as_str()));

        // 竞态窗口：最后一次写入之后、恢复之前
        assert!(
            clip_write_from_external_process("qp_p003_xproc_user_b"),
            "跨进程并发复制必须成功"
        );

        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::SkippedForeignChange,
            "恢复前检出外部改写必须跳过恢复"
        );
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p003_xproc_user_b"),
            "用户新内容必须原样保留（旧快照绝不写回）"
        );
    }

    /// 报告 §5.1 C-02：跨进程真实「管理器写入」（会清空剪贴板）之后，六类格式必须逐格式恢复。
    #[test]
    fn test_xproc_manager_write_restores_all_seeded_formats() {
        let _guard = clip_test_lock();

        let html_format = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "HTML Format"
            ))
        };
        let rtf_format = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "Rich Text Format"
            ))
        };
        let custom_format = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "QuickPathMixedFormatProbe"
            ))
        };
        assert!(html_format != 0 && rtf_format != 0 && custom_format != 0);

        let html = "<html><body>QuickPath 跨进程 HTML</body></html>"
            .as_bytes()
            .to_vec();
        let rtf = r"{\rtf1\ansi QuickPath 跨进程 RTF}".as_bytes().to_vec();
        let custom = b"quickpath-xproc-hglobal-probe".to_vec();
        let hdrop = hdrop_blob(&[r"C:\QuickPath\x1.txt", r"D:\QuickPath\x2.bin"]);
        let dib = minimal_dib_blob();

        seed_clipboard_formats(&[
            (html_format, &html),
            (rtf_format, &rtf),
            (custom_format, &custom),
            (CF_HDROP, &hdrop),
            (CF_DIB, &dib[..]),
        ]);
        let before = ClipboardSnapshot::capture().expect("混合格式剪贴板必须能被安全完整捕获");

        let target = make_probe_dir("xproc_mixed");
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
        let panel = crate::managers::clipboard_probe::probe_panel_path(
            &mut tx,
            |_| true,
            || clip_write_from_external_process(&target).then_some(0),
        )
        .expect("跨进程管理器写入后的读回必须成功");
        assert_eq!(
            panel.path.as_deref(),
            Some(target.as_str()),
            "必须取回真实目录"
        );

        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::Committed,
            "无并发改写时必须正常恢复"
        );
        assert_mixed_roundtrip(&before, "跨进程真实管理器写入后");
    }

    /// 报告 §5.1 C-01：真实检查 owner 隐藏窗口存在，且 QuickPath 写入剪贴板时
    /// `GetClipboardOwner()` 必须就是该 owner 窗口（而不是 NULL）。
    #[test]
    fn test_owner_window_is_real_clipboard_owner_during_write() {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::DataExchange::GetClipboardOwner;

        let _guard = clip_test_lock();
        let owner = clipboard_owner().expect("owner 窗口必须可用");
        assert!(
            unsafe { IsWindow(Some(owner)).as_bool() },
            "owner 窗口必须存活"
        );

        assert!(
            set_raw_clipboard_text("qp_p003_owner_probe"),
            "写入必须成功"
        );
        let actual = unsafe { GetClipboardOwner() }.expect("写入后必须能取到剪贴板所有者");
        assert_ne!(actual, HWND::default(), "写入后剪贴板不得无主（NULL）");
        assert_eq!(
            actual, owner,
            "QuickPath 写入剪贴板后 GetClipboardOwner() 必须是我们的 owner 窗口"
        );

        // 收尾：不把探针文本留给后续用例
        assert!(set_raw_clipboard_text(""));
    }

    /// 报告 §5.1 C-03：**真实**剪贴板忙（另一线程真实持有 `OpenClipboard`）时，
    /// 写入必须失败、结局必须为 `ReadFailed`（放弃恢复），且绝不覆盖用户原有内容。
    ///
    /// 与 `test_clipboard_busy_is_reported_and_never_claims_success` 的区别：后者用
    /// `TEST_FORCE_BUSY` 注入，本用例是真实的系统级占用，无任何注入。
    #[test]
    fn test_real_clipboard_busy_never_clobbers() {
        let _guard = clip_test_lock();
        assert!(
            clip_write_from_external_process("qp_p003_busy_original"),
            "种子写入必须成功"
        );
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        // 另一线程真实占住剪贴板，一直持有到主线程放行
        let (locked_tx, locked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            let mut held = false;
            for _ in 0..300 {
                if unsafe { OpenClipboard(None) }.is_ok() {
                    held = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert!(held, "占用线程必须能打开剪贴板");
            let _ = locked_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            let _ = unsafe { CloseClipboard() };
        });

        locked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("必须观察到剪贴板已被真实占用");

        assert!(
            tx.write_owned_text("").is_err(),
            "真实剪贴板忙时临时写入必须失败（不得假成功）"
        );
        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::SkippedUnknownOwner,
            "归属不可判定时必须放弃恢复"
        );

        let _ = release_tx.send(());
        holder.join().expect("占用线程必须正常结束");

        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p003_busy_original"),
            "真实剪贴板忙时绝不能覆盖用户原有内容"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // 审计需求 `9549648` §3.5：P0-05 确定性会合用例
    //
    // 全部使用显式会合点（`arm_atomic_write_hook` / 阻塞消息泵 / 通道），不靠 `sleep`
    // 猜时序；每条断言都覆盖：最终文本、格式集合、sequence、owner HWND、owner PID、
    // 返回状态，以及是否执行过 `EmptyClipboard`。
    // ══════════════════════════════════════════════════════════════════════

    /// P0-05 §3.5 用例 1：`capture()` 通过后、原子原语获得剪贴板锁之前，外部进程写新内容
    /// （等价旧的 `still_owned()` 通过 → `restore_attempt()` 拿到锁 的交错）。
    /// 恢复必须**零修改退出**：不 EmptyClipboard、不写回旧快照的任何一个格式。
    #[test]
    fn test_restore_zero_modification_when_foreign_write_precedes_lock() {
        let _guard = clip_test_lock();
        let html = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "QuickPathT1Format"
            ))
        };
        assert!(html != 0);
        seed_clipboard_formats(&[(html, b"t1-old-html")]);

        let mut snap = ClipboardSnapshot::capture().expect("含注册格式的剪贴板必须可安全捕获");
        let before = observe_clipboard();

        // 在「检查通过」与「真正持锁清空」之间让外部写入发生。
        // 注意：注入写入自身也会执行一次 EmptyClipboard，故基线在**注入之后**采集。
        let baseline = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let baseline_hook = baseline.clone();
        let external = "qp_p005_t1_external".to_string();
        arm_atomic_write_hook(0, move || {
            assert!(set_raw_clipboard_text(&external));
            baseline_hook.store(empty_clipboard_calls(), std::sync::atomic::Ordering::SeqCst);
        });

        assert_eq!(
            snap.restore(),
            Ok(RestoreStatus::Abandoned),
            "持锁复核检出外部改写必须判为永久放弃"
        );

        let after = observe_clipboard();
        assert_eq!(
            after.text.as_deref(),
            Some("qp_p005_t1_external"),
            "外部新内容必须原样保留"
        );
        assert_ne!(after.sequence, before.sequence, "sequence 必须反映外部写入");
        let empty_baseline = baseline.load(std::sync::atomic::Ordering::SeqCst);
        assert_ne!(empty_baseline, usize::MAX, "会合点必须已执行");
        assert_eq!(
            empty_clipboard_calls(),
            empty_baseline,
            "放弃路径绝不能执行 EmptyClipboard"
        );
        assert!(
            !after.formats.contains(&html),
            "旧快照的 HTML 格式绝不得写回"
        );
        let _ = set_raw_clipboard_text("qp_p005_t1_cleanup");
    }

    /// P0-05 §3.5 用例 2：`capture()` 完成、WPS 临时文本写入之前外部进程写入 ——
    /// 临时写入必须失败（闭包不执行、不发送 Ctrl+V），并保留用户新内容。
    #[test]
    fn test_temporary_write_fails_when_foreign_write_precedes_write_lock() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p005_t2_seed"));

        let baseline = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let baseline_hook = baseline.clone();
        let external = "qp_p005_t2_external".to_string();
        arm_atomic_write_hook(0, move || {
            assert!(set_raw_clipboard_text(&external));
            baseline_hook.store(empty_clipboard_calls(), std::sync::atomic::Ordering::SeqCst);
        });

        let action_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = action_ran.clone();
        let outcome: Result<u32, ClipboardSafetyError> =
            with_temporary_clipboard_text("C:\\p005_t2", move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                7
            });

        assert_eq!(
            outcome,
            Err(ClipboardSafetyError::WriteFailed),
            "外部改写后临时写入必须明确失败"
        );
        assert!(
            !action_ran.load(std::sync::atomic::Ordering::SeqCst),
            "临时写入失败时闭包绝不得执行（也就不可能发送 Ctrl+V）"
        );
        let after = observe_clipboard();
        assert_eq!(
            after.text.as_deref(),
            Some("qp_p005_t2_external"),
            "用户新内容必须原样保留"
        );
        let empty_baseline = baseline.load(std::sync::atomic::Ordering::SeqCst);
        assert_ne!(empty_baseline, usize::MAX, "会合点必须已执行");
        assert_eq!(
            empty_clipboard_calls(),
            empty_baseline,
            "失败路径必须零修改（不得清空剪贴板）"
        );
    }

    /// P0-05 §3.5 用例 3：第一次恢复部分失败后、第二次重试前外部进程写入 ——
    /// 后续绝不得再写入任何格式；快照必须永久放弃，且放弃后 Drop 不得补写。
    #[test]
    fn test_restore_retry_reverifies_and_abandons_on_foreign_write() {
        let _guard = clip_test_lock();
        let html = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "QuickPathT3Format"
            ))
        };
        seed_clipboard_formats(&[(html, b"t3-html")]);
        let failure_empty_baseline = empty_clipboard_calls();

        let baseline = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let baseline_hook = baseline.clone();
        let external = "qp_p005_t3_external".to_string();
        // 首次恢复（第 1 次原子写入）注入 CF_UNICODETEXT 失败；
        // 重试（第 2 次原子写入）开头让外部写入发生。
        let _inject = FailInjection::enable(CF_UNICODETEXT);
        arm_atomic_write_hook(1, move || {
            // 会合点里的外部写入本身也是 CF_UNICODETEXT，必须临时撤下故障注入
            // （否则“外部写入”会被自己的注入弄失败，交错就不成立）。
            TEST_FAIL_FORMAT.store(u32::MAX, std::sync::atomic::Ordering::SeqCst);
            assert!(set_raw_clipboard_text(&external));
            TEST_FAIL_FORMAT.store(CF_UNICODETEXT, std::sync::atomic::Ordering::SeqCst);
            baseline_hook.store(empty_clipboard_calls(), std::sync::atomic::Ordering::SeqCst);
        });

        {
            let mut snap = ClipboardSnapshot::capture().expect("必须可安全捕获");
            assert_eq!(
                snap.restore(),
                Ok(RestoreStatus::Abandoned),
                "重试前检出外部改写必须永久放弃"
            );
            let after = observe_clipboard();
            assert_eq!(
                after.text.as_deref(),
                Some("qp_p005_t3_external"),
                "外部新内容必须原样保留"
            );
            let empty_baseline = baseline.load(std::sync::atomic::Ordering::SeqCst);
            assert_ne!(empty_baseline, usize::MAX, "会合点必须已执行");
            // 首次恢复确实清空过一次（+1），会合点里的外部写入自己又清空一次（+1）；
            // 重试（empty_first=false）与 Drop 绝不得再清空。
            assert_eq!(
                empty_baseline,
                failure_empty_baseline + 2,
                "只允许首次恢复与会合点注入各一次 EmptyClipboard"
            );
            assert_eq!(
                empty_clipboard_calls(),
                empty_baseline,
                "放弃后（含重试与 Drop）绝不得再执行 EmptyClipboard"
            );
            assert!(
                !after.formats.contains(&html),
                "放弃后不得向用户新剪贴板追加剩余格式"
            );
        }

        assert_eq!(
            empty_clipboard_calls(),
            baseline.load(std::sync::atomic::Ordering::SeqCst),
            "snap 已 drop 仍不得补写"
        );
        let _ = set_raw_clipboard_text("qp_p005_t3_cleanup");
    }

    /// P0-05 §3.5 用例 4：用户复制**字面相同**的 Unicode 文本，但 HTML / 自定义格式不同 ——
    /// 旧快照不得覆盖新格式（已删除「文本相同即可继续恢复」的宽松放行）。
    #[test]
    fn test_restore_refuses_same_text_with_different_formats() {
        let _guard = clip_test_lock();
        let html = unsafe {
            windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!(
                "QuickPathT4Format"
            ))
        };
        assert!(html != 0);

        seed_clipboard_formats_with_text("qp_p005_t4_text", &[(html, b"t4-old-html")]);
        let mut snap = ClipboardSnapshot::capture().expect("必须可安全捕获");

        // 临时写入的文本与随后用户复制的文本字面完全相同
        let seq = set_raw_clipboard_text_tracked("qp_p005_t4_text").expect("临时写入必须成功");
        snap.bind_owned_write("qp_p005_t4_text", seq);

        // 用户复制：Unicode 文本一模一样，但携带完全不同的 HTML 格式
        seed_clipboard_formats_with_text("qp_p005_t4_text", &[(html, b"t4-user-html")]);

        assert_eq!(
            snap.restore(),
            Ok(RestoreStatus::Abandoned),
            "字面文本相同也必须按 sequence 判定为外部改写"
        );
        let after = observe_clipboard();
        assert_eq!(after.text.as_deref(), Some("qp_p005_t4_text"));
        assert_eq!(
            read_clipboard_format_bytes(html).as_deref(),
            Some(&b"t4-user-html"[..]),
            "用户新格式绝不得被旧快照覆盖"
        );
    }

    /// P0-05 §3.5 用例 5：事务放弃后触发 `Drop` —— 不得发生第二次恢复。
    #[test]
    fn test_transaction_drop_after_abandon_never_writes_again() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_p005_t5_seed"));

        let empty_before;
        {
            let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
            tx.write_owned_text("").expect("临时写入必须成功");
            // 外部改写 → 放弃恢复
            assert!(set_raw_clipboard_text("qp_p005_t5_user"));
            assert_eq!(
                tx.restore_if_still_owned(),
                ClipboardTransactionOutcome::SkippedForeignChange,
                "外部改写必须跳过恢复"
            );
            empty_before = empty_clipboard_calls();
        }

        assert_eq!(
            empty_clipboard_calls(),
            empty_before,
            "放弃后 Drop 绝不得再执行 EmptyClipboard"
        );
        let after = observe_clipboard();
        assert_eq!(
            after.text.as_deref(),
            Some("qp_p005_t5_user"),
            "放弃后 Drop 绝不得覆盖用户新内容"
        );
        assert_eq!(
            after.owner_pid,
            std::process::id(),
            "最终 owner 必须是测试进程自己（而不是遗留的悬浮 owner）"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // 审计需求 `9549648` §2.5：P0-04-B 延迟处理两次通知的生产入口用例
    //
    // 本机未安装 Total Commander / XYplorer（且用户不授权安装），故“管理器写入”
    // 由**独立 OS 进程**（`Set-Clipboard`）完成；“管理器身份”不参与判定（来源断言
    // 传 `|_| true` 最宽松闭包），拒绝必须**只能**来自「更新历史不可证明」。
    // 两条链路调用的都是同一套生产入口：`probe_two_panels` → `probe_panel_path` →
    // `ClipboardTransaction::note_observed_owned_text_proven`。
    // ══════════════════════════════════════════════════════════════════════

    /// 阻塞 owner 消息泵 → 用户（外部进程 A）先写 → 管理器（外部进程 B）后写 →
    /// 两次写入完成后再处理通知。事务必须拒绝认领，不恢复旧快照、不发起第二面板请求。
    fn delayed_two_notifications_rejects_claim(label: &str, tag: &str) {
        let dir = make_probe_dir(tag);
        let seed = format!("qp_p004b_delay_{tag}_seed");
        assert!(set_raw_clipboard_text(&seed));

        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");
        let second_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_flag = second_called.clone();
        let probe_dir = dir.clone();

        let err = probe_two_panels(
            &mut tx,
            // 最关键的设计：来源判定放到最宽松。若拒绝来自 PID 不匹配，说明判据仍然
            // 依赖不可靠的“处理时 owner”，本用例就失去了意义。
            |_| true,
            move || {
                let marker = clipboard_update_marker();
                let release = block_owner_pump_for_test();
                assert!(
                    clip_write_from_external_process("qp_p004b_delay_user"),
                    "{label}: 外部进程用户写入必须成功"
                );
                assert!(
                    clip_write_from_external_process(&probe_dir),
                    "{label}: 外部进程管理器写入必须成功"
                );
                // 两次写入都完成后才放行消息泵，并等两条通知都被登记
                drop(release);
                assert!(
                    wait_for_events_after(marker, 2, Duration::from_secs(10)),
                    "{label}: 延迟的两条通知必须都被登记"
                );
                Some(1)
            },
            move || {
                second_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Some(1)
            },
        )
        .expect_err("延迟处理两次通知时必须拒绝认领");

        assert!(
            matches!(
                err,
                ClipboardTransactionError::OwnershipUnknown | ClipboardTransactionError::NotOwned
            ),
            "{label}: 必须是 fail-closed 错误，实际 {:?}",
            err
        );
        assert!(
            !second_called.load(std::sync::atomic::Ordering::SeqCst),
            "{label}: 失败路径不得发起第二面板请求"
        );

        let empty_before = empty_clipboard_calls();
        let outcome = tx.restore_if_still_owned();
        assert!(
            matches!(
                outcome,
                ClipboardTransactionOutcome::AbortedEventHistoryUnprovable
                    | ClipboardTransactionOutcome::SkippedForeignChange
                    | ClipboardTransactionOutcome::SkippedUnknownOwner
            ),
            "{label}: 必须留下结构化终止态，实际 {:?}",
            outcome
        );
        assert_eq!(
            empty_clipboard_calls(),
            empty_before,
            "{label}: 失败路径不得恢复旧快照（不执行 EmptyClipboard）"
        );
        let after = observe_clipboard();
        assert_eq!(
            after.text.as_deref(),
            Some(dir.as_str()),
            "{label}: 不得写回旧快照覆盖当前内容"
        );
        assert!(
            after.sequence != 0,
            "{label}: 必须能读到有效的剪贴板 sequence"
        );
        assert!(
            after.formats.contains(&CF_UNICODETEXT),
            "{label}: 当前剪贴板必须仍含管理器写入的文本格式"
        );
        // owner HWND/PID 已在 `ClipboardObservation` 中采集；外部进程可能已退出，
        // 此时 owner 为 NULL（PID 0）——这正是“处理时采样无法回溯历史”的直接证据，
        // 不能用它做归属证明。
        assert_eq!(
            after.owner_hwnd == 0,
            after.owner_pid == 0,
            "{label}: owner HWND 与 PID 必须一致（NULL ⇔ 0）"
        );
    }

    /// 清单 §2.5 用例 2：同一交错覆盖 Total Commander 链路。
    #[test]
    fn test_totalcmd_delayed_two_notifications_rejects_claim() {
        let _guard = clip_test_lock();
        delayed_two_notifications_rejects_claim("totalcmd", "p004b_delay_tc");
    }

    /// 清单 §2.5 用例 2：同一交错覆盖 XYplorer 链路。
    #[test]
    fn test_xyplorer_delayed_two_notifications_rejects_claim() {
        let _guard = clip_test_lock();
        delayed_two_notifications_rejects_claim("xyplorer", "p004b_delay_xy");
    }

    /// 清单 §2.5 用例 1 的“干净对照”：同样阻塞消息泵，但只有一次外部写入（无交错），
    /// 必须正常认领 —— 否则 fail-closed 就成了“永远失败”，反而失去探测价值。
    #[test]
    fn test_delayed_single_notification_still_claims() {
        let _guard = clip_test_lock();
        let dir = make_probe_dir("p004b_delay_single");
        assert!(set_raw_clipboard_text("qp_p004b_delay_single_seed"));
        let mut tx = ClipboardTransaction::begin().expect("空闲剪贴板必须能开启事务");

        let probe_dir = dir.clone();
        let panel = probe_panel_path(
            &mut tx,
            |_| true,
            move || {
                let marker = clipboard_update_marker();
                let release = block_owner_pump_for_test();
                assert!(clip_write_from_external_process(&probe_dir));
                drop(release);
                assert!(wait_for_events_after(marker, 1, Duration::from_secs(10)));
                Some(1)
            },
        )
        .expect("只有一次外部写入时必须能正常认领");
        assert_eq!(panel.path.as_deref(), Some(dir.as_str()));
        assert_eq!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::Committed
        );
        assert_eq!(
            get_clipboard_text().as_deref(),
            Some("qp_p004b_delay_single_seed")
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // 审计需求 `9549648` §4.3：架构守卫
    // ══════════════════════════════════════════════════════════════════════

    fn collect_rs_files(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// §4.3：阻止生产代码绕过统一原语形成新的 `OpenClipboard -> EmptyClipboard` 路径。
    ///
    /// 规则：
    /// 1. 除 `src/win32/clipboard.rs` 之外，任何源文件不得直接调用剪贴板的清空/写入 Win32 API；
    /// 2. `clipboard.rs` 生产区（`mod tests` 之前、剔除注释行后）里：
    ///    `EmptyClipboard(` / `OpenClipboard(` / `SetClipboardData(` 各只允许出现**一处**
    ///    （分别在 `clipboard_atomic_write` / `open_clipboard_with_retry` / `prod_set`）。
    #[test]
    fn test_architecture_guard_single_atomic_clipboard_write_primitive() {
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let clipboard_path = src_root.join("win32").join("clipboard.rs");
        let forbidden = ["EmptyClipboard(", "SetClipboardData(", "OpenClipboard("];

        let mut files = Vec::new();
        collect_rs_files(&src_root, &mut files);
        assert!(files.len() > 20, "必须能枚举到源码文件");
        for file in &files {
            if *file == clipboard_path {
                continue;
            }
            let source = std::fs::read_to_string(file).expect("源码必须可读");
            let code = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for needle in forbidden {
                assert!(
                    !code.contains(needle),
                    "{} 不得直接调用 `{}`：必须走统一原子原语",
                    file.display(),
                    needle
                );
            }
        }

        let source = std::fs::read_to_string(&clipboard_path).expect("clipboard.rs 必须可读");
        let production_end = source
            .find("\nmod tests {")
            .expect("必须存在测试模块以便切分生产区");
        let production = source[..production_end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            production.matches("EmptyClipboard(").count(),
            1,
            "EmptyClipboard 只允许出现在统一原子原语 clipboard_atomic_write 中"
        );
        assert_eq!(
            production.matches("OpenClipboard(").count(),
            1,
            "OpenClipboard 只允许出现在 open_clipboard_with_retry 中"
        );
        assert_eq!(
            production.matches("SetClipboardData(").count(),
            1,
            "SetClipboardData 只允许出现在 prod_set 中"
        );
        assert!(
            production.contains("fn clipboard_atomic_write<"),
            "统一原子原语必须存在"
        );
    }
}
