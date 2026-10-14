#![allow(dead_code)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM,
    WPARAM,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GHND, GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowThreadProcessId, IsWindow, MSG, PostMessageW, PostQuitMessage, PostThreadMessageW,
    RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_QUIT,
    WNDCLASSW,
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
    /// 恢复前发现剪贴板已被第三方改写，主动放弃覆盖以保护用户刚复制的新内容
    SkippedForeignChange,
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

static OWNER: Mutex<Option<ClipboardOwnerHandle>> = Mutex::new(None);
static OWNER_GENERATION: AtomicU64 = AtomicU64::new(0);

/// owner 线程退出确认的有界等待时长；超时即放弃 `join`（detach），绝不永久阻塞宿主线程。
const OWNER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

unsafe extern "system" fn clipboard_owner_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    match msg {
        // 审计报告 R-06：显式退出协议。收到 `WM_CLOSE` 时销毁窗口，`DestroyWindow`
        // 会在同一线程上同步派发 `WM_DESTROY`。
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            windows::Win32::Foundation::LRESULT(0)
        }
        // `WM_DESTROY` 时投递 `WM_QUIT`，使 `GetMessageW` 返回 0、消息泵自然结束。
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
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
    let mut guard = OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // 双重检查：等待锁期间可能已由其他线程创建。
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

    let generation = OWNER_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    let (ready_tx, ready_rx) = mpsc::channel::<usize>();
    let (done_tx, done_rx) = mpsc::channel::<()>();

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
                    let _ = ready_tx.send(hwnd_raw);

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

    let hwnd_raw = match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(raw) => raw,
        Err(err) => {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_OWNER: 等待 owner 窗口就绪超时（{}），owner 不可用",
                err
            ));
            return None;
        }
    };

    if hwnd_raw == 0 {
        return None;
    }

    let hwnd = HWND(hwnd_raw as *mut _);
    let thread_id = unsafe { GetWindowThreadProcessId(hwnd, None) };
    *guard = Some(ClipboardOwnerHandle {
        hwnd_raw,
        thread_id,
        generation,
        done_rx,
        thread: Some(thread),
    });

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
    /// 是否因检出第三方改写而主动放弃，一旦放弃不再做任何补救
    abandoned: bool,
    /// 写入临时内容后的剪贴板序列号（并发覆盖防护）
    owned_seq: Option<u32>,
    /// 写入临时内容的精确文本（序列号变化时作为归属终审依据）
    owned_text: Option<String>,
    /// 严格归属模式：仅按序列号判定持有，禁用「序列号变了但文本仍相等即视为持有」的
    /// fail-open 回退。仅由 `note_temporary_write_strict`（剪贴板事务 API）开启，
    /// 既有 `ClipboardRestoreGuard` 路径保持原语义不变。
    strict_ownership: bool,
}

impl ClipboardSnapshot {
    /// 完整捕获当前系统剪贴板的所有格式数据
    /// 若剪贴板被占用或关键格式无法保真备份，返回 Err 坚决不碰剪贴板
    pub fn capture() -> Result<Self, ClipboardSafetyError> {
        if !open_clipboard_for_read(4) {
            return Err(ClipboardSafetyError::Busy);
        }

        let mut items = Vec::new();
        let mut total_bytes = 0usize;
        let was_empty;

        unsafe {
            let count = CountClipboardFormats();
            if count == 0 {
                was_empty = true;
                let _ = CloseClipboard();
                return Ok(Self::empty(items, was_empty));
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

        Ok(Self::empty(items, was_empty))
    }

    fn empty(items: Vec<(u32, Vec<u8>)>, was_empty: bool) -> Self {
        Self {
            items,
            was_empty,
            restored: false,
            restored_formats: Vec::new(),
            emptied: false,
            abandoned: false,
            owned_seq: None,
            owned_text: None,
            strict_ownership: false,
        }
    }

    /// 记录本次向剪贴板写入临时内容后的归属信息（序列号 + 精确文本），
    /// 供 `restore()` 判断“这份临时内容是否仍归本次操作所有”。
    pub fn note_temporary_write(&mut self, text: &str, sequence: u32) {
        self.owned_seq = Some(sequence);
        self.owned_text = Some(text.to_string());
    }

    /// 记录临时写入的归属信息，并开启**严格 sequence 判定**（剪贴板事务 API 专用）。
    ///
    /// 与 `note_temporary_write` 的唯一区别：序列号变化时不再用「文本相等」兜底，
    /// 从而满足审计报告 §2.5「用户复制的内容与临时文本恰好相同时仍须按 sequence
    /// 识别外部改写」。既有守卫路径不得调用本方法，以免既有语义漂移。
    pub fn note_temporary_write_strict(&mut self, text: &str, sequence: u32) {
        self.note_temporary_write(text, sequence);
        self.strict_ownership = true;
    }

    /// 剪贴板当前是否仍由本次临时写入持有。
    ///
    /// 序列号相同 → 期间无人改写，判定为持有；
    /// 序列号变化 → 再比对 Unicode 文本是否仍精确等于本次注入文本：
    /// 相等（多为剪贴板管理器附加自有格式）仍视为持有，不等则第三方已写入新内容，
    /// 此时必须拒绝覆盖，否则会静默吃掉用户刚复制的东西。
    fn still_owned(&self) -> bool {
        let (Some(seq), Some(text)) = (self.owned_seq.as_ref(), self.owned_text.as_ref()) else {
            // 未写入过临时内容（纯快照 → 原样恢复）
            return true;
        };
        if clipboard_sequence_number() == *seq {
            return true;
        }
        // 严格模式（事务 API）：序列号已变即视为外部改写，即便当前文本与本次临时文本
        // 恰好相等也绝不视为持有（报告 §2.5）。
        if self.strict_ownership {
            return false;
        }
        // 兼容模式（既有守卫路径，语义不漂移）：文本仍精确相等时视为持有，
        // 多为剪贴板管理器附加自有格式导致序列号变化。
        matches!(get_clipboard_text(), Some(cur) if cur == *text)
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
    /// 语义保证：
    /// 1. 逐格式检查 `GlobalAlloc` / `GlobalLock` / `SetClipboardData` 返回值，记录每个格式是否写回成功；
    /// 2. 只有全部格式成功才返回 `Ok(Restored)` 并置 `restored = true`，失败返回结构化错误；
    /// 3. 失败只做有限次重试，且重试仅补写未成功格式（不再 EmptyClipboard，避免已恢复格式被清掉）；
    /// 4. 恢复前校验剪贴板归属，若已被第三方改写则返回 `SkippedForeignChange` 且绝不覆盖。
    pub fn restore(&mut self) -> Result<RestoreStatus, ClipboardRestoreError> {
        if self.restored {
            return Ok(RestoreStatus::AlreadyRestored);
        }
        if self.abandoned {
            return Ok(RestoreStatus::SkippedForeignChange);
        }
        if !self.still_owned() {
            // 主动放弃：同时置 restored 让 Drop 不再补救（补救即会覆盖用户新内容）
            self.abandoned = true;
            self.restored = true;
            return Ok(RestoreStatus::SkippedForeignChange);
        }

        let mut last_error = None;
        for attempt in 0..RESTORE_ATTEMPTS {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(30 * u64::from(attempt)));
            }
            match self.restore_attempt() {
                Ok(true) => {
                    self.restored = true;
                    return Ok(RestoreStatus::Restored);
                }
                Ok(false) => {
                    last_error = Some(ClipboardRestoreError::PartialRestore(
                        self.pending_formats(),
                    ));
                }
                Err(err) => last_error = Some(err),
            }
        }

        Err(last_error.unwrap_or(ClipboardRestoreError::Busy))
    }

    /// 单次恢复尝试：清空（仅首次）后逐格式写回，返回是否全部成功
    fn restore_attempt(&mut self) -> Result<bool, ClipboardRestoreError> {
        // 必须是“写路径”：以有效 owner 打开，否则 EmptyClipboard 会把 owner 置空
        if !open_clipboard_for_write(4) {
            return Err(ClipboardRestoreError::Busy);
        }

        unsafe {
            if !self.emptied {
                if EmptyClipboard().is_err() {
                    let _ = CloseClipboard();
                    return Err(ClipboardRestoreError::EmptyFailed);
                }
                self.emptied = true;
            }

            let mut all_ok = true;
            for (fmt, data) in &self.items {
                if self.restored_formats.contains(fmt) {
                    continue;
                }
                if write_clipboard_format(*fmt, data) {
                    self.restored_formats.push(*fmt);
                } else {
                    all_ok = false;
                }
            }

            let _ = CloseClipboard();
            Ok(all_ok)
        }
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

/// RAII 守卫：确保离开作用域时无论如何自动执行剪贴板还原
pub struct ClipboardRestoreGuard {
    snapshot: Option<ClipboardSnapshot>,
}

impl ClipboardRestoreGuard {
    pub fn new(snapshot: ClipboardSnapshot) -> Self {
        Self {
            snapshot: Some(snapshot),
        }
    }

    /// 记录本次写入临时内容后的归属信息（转发给内部快照）
    pub fn note_temporary_write(&mut self, text: &str, sequence: u32) {
        if let Some(ref mut snap) = self.snapshot {
            snap.note_temporary_write(text, sequence);
        }
    }

    /// 显式手动还原
    pub fn restore_now(&mut self) -> Result<RestoreStatus, ClipboardRestoreError> {
        match self.snapshot {
            Some(ref mut snap) => snap.restore(),
            None => Ok(RestoreStatus::AlreadyRestored),
        }
    }
}

impl Drop for ClipboardRestoreGuard {
    fn drop(&mut self) {
        if let Some(mut snap) = self.snapshot.take() {
            let _ = snap.restore();
        }
    }
}

/// 剪贴板「临时占用事务」的最终结局（恰好四态，供调用方观测与记日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardTransactionOutcome {
    /// 用户原剪贴板内容已完整恢复
    Restored,
    /// 检出外部改写（用户/第三方已复制新内容），主动放弃恢复以保护其新内容
    SkippedForeignChange,
    /// 剪贴板归属不可判定（序列号不可读 / 读回失败 / 剪贴板忙），保守放弃恢复
    ReadFailed,
    /// 归属校验通过，但恢复动作本身失败（携带底层错误）
    RestoreFailed(ClipboardRestoreError),
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
/// 收口成一套带**严格 sequence 归属校验**的 API（审计报告 P0-03）。
///
/// 与 `ClipboardRestoreGuard`（上一次已验收的守卫路径）并存，语义区别：
/// - 归属判定严格按 `GetClipboardSequenceNumber`，序列号变化即视为外部改写，
///   不使用「文本相等仍视为持有」的 fail-open 回退；
/// - 归属不可判定时保守放弃恢复（`ReadFailed`），绝不用旧快照覆盖当前内容；
/// - 恢复前与每次写入前都做归属校验，检出外部改写立即返回 `Err` 让调用方停止写入；
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
    /// 最终结局；一旦确定即幂等返回，`Drop` 不会再重复动作
    outcome: Option<ClipboardTransactionOutcome>,
}

impl ClipboardTransaction {
    /// 开启事务：完整快照当前剪贴板。快照失败（`Busy` / `BackupIncomplete`）直接返回
    /// `Err`，绝不触碰剪贴板内容。
    pub fn begin() -> Result<Self, ClipboardSafetyError> {
        let snapshot = ClipboardSnapshot::capture()?;
        let seq = clipboard_sequence_number();
        Ok(Self {
            snapshot: Some(snapshot),
            // 序列号不可读（返回 0）时不建立基准，后续写入成功会重新建立
            owned_seq: (seq != 0).then_some(seq),
            owned_text: None,
            lost_ownership: false,
            ownership_unknown: seq == 0,
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
    /// 若此前已有本事务认可的写入，先做归属校验；被外部改写则立即返回 `Err`，
    /// 绝不覆盖用户刚复制的新内容。
    pub fn write_owned_text(&mut self, text: &str) -> Result<(), ClipboardTransactionError> {
        self.ensure_still_owned()?;

        let Some(seq) = set_raw_clipboard_text_tracked(text) else {
            // 写入失败可能已清空剪贴板（归属未知），保守放弃恢复
            self.ownership_unknown = true;
            return Err(ClipboardTransactionError::WriteFailed);
        };
        if seq == 0 {
            // 写入成功但序列号不可读：无法建立归属基准，保守放弃恢复
            self.ownership_unknown = true;
            return Err(ClipboardTransactionError::OwnershipUnknown);
        }

        self.owned_seq = Some(seq);
        self.owned_text = Some(text.to_string());
        Ok(())
    }

    /// 请求管理器写入路径后调用：读回文本并登记当时的序列号作为新的归属基准。
    ///
    /// 读回失败或序列号不可读时返回 `None` 并把归属标记为不可判定（后续绝不恢复）。
    pub fn note_observed_owned_text(&mut self) -> Option<String> {
        let seq = clipboard_sequence_number();
        match get_clipboard_text() {
            Some(text) if seq != 0 => {
                self.owned_seq = Some(seq);
                self.owned_text = Some(text.clone());
                Some(text)
            }
            _ => {
                self.ownership_unknown = true;
                None
            }
        }
    }

    /// 最终恢复：先按严格 sequence 校验归属，校验通过才恢复。
    ///
    /// 结局恰好四态（见 [`ClipboardTransactionOutcome`]）。幂等：重复调用返回同一结局。
    pub fn restore_if_still_owned(&mut self) -> ClipboardTransactionOutcome {
        if let Some(outcome) = self.outcome.clone() {
            return outcome;
        }

        let outcome = match self.refresh_ownership() {
            // 被外部改写：绝不 EmptyClipboard、绝不写回旧快照
            OwnershipState::Lost => ClipboardTransactionOutcome::SkippedForeignChange,
            // 归属未知：不得用旧快照覆盖当前内容
            OwnershipState::Unknown => ClipboardTransactionOutcome::ReadFailed,
            // 归属未变（含管理器超时/窗口退出：剪贴板仍是本事务的临时写入，
            // 用户原始内容依然安全地被快照保存）→ 正常还回去
            OwnershipState::Owned => self.perform_restore(),
        };
        self.outcome = Some(outcome.clone());
        outcome
    }

    /// 执行真实恢复：以严格归属模式驱动快照恢复，恢复动作内部再做一次同源 sequence 校验。
    fn perform_restore(&mut self) -> ClipboardTransactionOutcome {
        let owned = self.owned_seq.zip(self.owned_text.clone());
        let Some(snapshot) = self.snapshot.as_mut() else {
            return ClipboardTransactionOutcome::Restored;
        };
        if let Some((seq, text)) = owned {
            snapshot.note_temporary_write_strict(&text, seq);
        }
        match snapshot.restore() {
            Ok(RestoreStatus::Restored | RestoreStatus::AlreadyRestored) => {
                ClipboardTransactionOutcome::Restored
            }
            Ok(RestoreStatus::SkippedForeignChange) => {
                ClipboardTransactionOutcome::SkippedForeignChange
            }
            Err(err) => ClipboardTransactionOutcome::RestoreFailed(err),
        }
    }
}

impl Drop for ClipboardTransaction {
    fn drop(&mut self) {
        // 显式恢复已失败过：与既有守卫一致，Drop 再补一次（只补写未成功格式，
        // 且仍走严格归属校验，被外部改写时绝不过写）。
        if matches!(
            self.outcome,
            Some(ClipboardTransactionOutcome::RestoreFailed(_))
        ) {
            if let Some(snapshot) = self.snapshot.as_mut() {
                let _ = snapshot.restore();
            }
            return;
        }
        // RAII 兜底：调用方未显式恢复（如提前 return）时，走完全同一套归属校验
        let _ = self.restore_if_still_owned();
    }
}

/// 临时向系统剪贴板写入 Unicode 文本，并返回写入后的剪贴板序列号
fn set_raw_clipboard_text_tracked(text: &str) -> Option<u32> {
    // 必须是“写路径”：以有效 owner 打开，否则 EmptyClipboard 会把 owner 置空
    if !open_clipboard_for_write(4) {
        return None;
    }

    unsafe {
        if EmptyClipboard().is_err() {
            let _ = CloseClipboard();
            return None;
        }

        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes = wide.len() * 2;
        let mut ok = false;

        if let Ok(h) = GlobalAlloc(GHND, bytes)
            && !h.0.is_null()
        {
            let ptr = GlobalLock(h);
            if ptr.is_null() {
                let _ = GlobalFree(Some(h));
            } else {
                std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
                let _ = GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0))).is_ok() {
                    ok = true;
                } else {
                    let _ = GlobalFree(Some(h));
                }
            }
        }

        let _ = CloseClipboard();
        ok.then(clipboard_sequence_number)
    }
}

/// 临时向系统剪贴板写入 Unicode 文本
fn set_raw_clipboard_text(text: &str) -> bool {
    set_raw_clipboard_text_tracked(text).is_some()
}

/// 清空剪贴板文本内容。
///
/// 供第三方文件管理器探测使用：每次探测前先清空，
/// 避免“对端超时/未响应”时把上一次的陈旧路径当成本次结果读回来。
pub fn clear_clipboard_text() -> bool {
    set_raw_clipboard_text("")
}

/// 安全地在受保护的环境下临时使用剪贴板注入文本：
/// 1. 尝试完整快照当前剪贴板（含文件列表 CF_HDROP、位图、文本等）；
/// 2. 若快照失败（被占/不可保真），直接返回 Err，绝不修改剪贴板；
/// 3. 设置临时文本并登记归属（序列号 + 精确文本），执行物理操作闭包；
/// 4. 闭包结束后（无论成功或提前早退）立即还原，Drop 阶段还会再做一次补救；
/// 5. 恢复失败向上传播为 `RestoreFailed`，调用方不得据此认为“已无损还原”。
pub fn with_temporary_clipboard_text<R, F>(text: &str, action: F) -> Result<R, ClipboardSafetyError>
where
    F: FnOnce() -> R,
{
    let snapshot = ClipboardSnapshot::capture()?;
    let mut guard = ClipboardRestoreGuard::new(snapshot);

    let Some(sequence) = set_raw_clipboard_text_tracked(text) else {
        // 写入失败时，guard drop 会自动恢复原内容
        return Err(ClipboardSafetyError::WriteFailed);
    };
    guard.note_temporary_write(text, sequence);

    let result = action();

    // 动作执行完毕，立即显式还原，并逐一类别处理三种结局
    match guard.restore_now() {
        Ok(RestoreStatus::Restored | RestoreStatus::AlreadyRestored) => {}
        Ok(RestoreStatus::SkippedForeignChange) => {
            crate::dialog::detector::log_debug(
                "CLIPBOARD_RESTORE: 检出第三方已改写剪贴板，放弃覆盖其新内容",
            );
        }
        Err(err) => {
            crate::dialog::detector::log_debug(&format!("CLIPBOARD_RESTORE_FAILED: {:?}", err));
            return Err(ClipboardSafetyError::RestoreFailed(err));
        }
    }

    Ok(result)
}

/// 安全读取当前剪贴板的纯文本（若剪贴板无纯文本则返回 None）
pub fn get_clipboard_text() -> Option<String> {
    if !open_clipboard_for_read(4) {
        return None;
    }
    unsafe {
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
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::clipboard_probe::probe_panel_path;
    use std::sync::Mutex;
    use windows::Win32::System::DataExchange::GetClipboardOwner;

    static CLIP_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 串行化剪贴板用例。即使某个用例 panic，也不要因为锁中毒而掩盖其他用例的真实结果。
    fn clip_test_lock() -> std::sync::MutexGuard<'static, ()> {
        CLIP_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
            // 清掉当前剪贴板，确保后续断言只能来自快照恢复
            assert!(set_raw_clipboard_text("qp_restore_probe"));

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
        snap.note_temporary_write("C:\\TemporaryInjection", seq);

        // 模拟用户/第三方在恢复前复制了全新内容
        assert!(set_raw_clipboard_text("qp_user_new_content"));

        assert_eq!(
            snap.restore(),
            Ok(RestoreStatus::SkippedForeignChange),
            "序列号变化且内容不再匹配时必须放弃覆盖"
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
    #[test]
    fn test_temporary_clipboard_text_propagates_restore_failure() {
        let _guard = clip_test_lock();

        for op in [FailOp::Set, FailOp::Alloc, FailOp::Lock] {
            assert!(set_raw_clipboard_text("qp_propagate_seed"));

            let inject = FailInjection::enable_with(CF_UNICODETEXT, op);
            let outcome: Result<u32, ClipboardSafetyError> =
                with_temporary_clipboard_text("C:\\PropagateProbe", || 7);
            drop(inject);

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

    /// 用指定字节块占用多个剪贴板格式，并顺带写入一段 Unicode 文本
    fn seed_clipboard_formats(entries: &[(u32, &[u8])]) {
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
            let wide: Vec<u16> = "qp_multiformat_seed"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let bytes = wide.len() * 2;
            let handle = prod_alloc(bytes).expect("分配必须成功");
            let ptr = prod_lock(handle);
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr, bytes);
            prod_unlock(handle);
            assert!(prod_set(CF_UNICODETEXT, handle));
            let _ = CloseClipboard();
        }
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

            let first = probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(src_path);
                Some(1)
            })
            .expect("首次写入不应触发归属冲突");
            assert!(first.delivered, "{label}: 首次探测必须视为对端已应答");

            // 用户在扫描未结束时复制了新内容
            let user_new = format!("qp_p003_{label}_user_new_1");
            assert!(set_raw_clipboard_text(&user_new));

            // 第二次清空前的写前校验必须拒绝写入（且不得执行 request）
            let err = probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(trg_path);
                Some(1)
            })
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
            probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(src_path);
                Some(1)
            })
            .expect("首次探测必须成功");
            probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(trg_path);
                Some(1)
            })
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
            let probe = probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(src_path);
                Some(1)
            })
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
            let probe = probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(src_path);
                Some(1)
            })
            .expect("无并发改写时探测必须成功");
            assert!(probe.delivered, "{label}");

            let outcome = tx.restore_if_still_owned();
            assert_eq!(
                outcome,
                ClipboardTransactionOutcome::Restored,
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
                ClipboardTransactionOutcome::ReadFailed,
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
                assert_eq!(
                    tx.note_observed_owned_text(),
                    None,
                    "{label}: 读回失败必须返回 None"
                );
            }
            let current = format!("qp_p003_{label}_readfail_current");
            assert!(set_raw_clipboard_text(&current));
            assert_eq!(
                tx.restore_if_still_owned(),
                ClipboardTransactionOutcome::ReadFailed,
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
                ClipboardTransactionOutcome::ReadFailed,
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
            let probe = probe_panel_path(&mut tx, || None).expect("超时不应产生归属冲突");
            assert!(
                !probe.delivered && probe.path.is_none(),
                "{label}: 超时不得产生结果"
            );
            let outcome = tx.restore_if_still_owned();
            assert_eq!(
                outcome,
                ClipboardTransactionOutcome::Restored,
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

            let src = probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(src_path);
                Some(1)
            })
            .expect("首次探测必须成功");
            let trg = probe_panel_path(&mut tx, || {
                let _ = set_raw_clipboard_text(trg_path);
                Some(1)
            })
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
            assert_eq!(outcome, ClipboardTransactionOutcome::Restored, "{label}");
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
                ClipboardTransactionOutcome::RestoreFailed(err) => {
                    assert!(
                        matches!(err, ClipboardRestoreError::PartialRestore(ref v) if v.contains(&CF_UNICODETEXT)),
                        "恢复失败必须是含 CF_UNICODETEXT 的 PartialRestore，实际 {:?}",
                        err
                    );
                }
                other => panic!("期望 RestoreFailed，实际 {:?}", other),
            }
        }

        // 结局幂等：再次调用不得重新动作
        assert!(matches!(
            tx.restore_if_still_owned(),
            ClipboardTransactionOutcome::RestoreFailed(_)
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
}
