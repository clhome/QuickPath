#![allow(dead_code)]

use std::sync::Once;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GHND, GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, IsWindow, MSG, RegisterClassW,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
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

/// 读取系统剪贴板序列号（每次剪贴板内容变化都会自增）
fn clipboard_sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

impl std::error::Error for ClipboardSafetyError {}

/// 测试期强制「剪贴板被占用」注入（同进程内无法真实模拟跨进程独占）
#[cfg(test)]
static TEST_FORCE_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

/// 进程级剪贴板 owner 窗口句柄（懒创建，跨线程可用）
static CLIPBOARD_OWNER: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static OWNER_INIT: Once = Once::new();

unsafe extern "system" fn clipboard_owner_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    // owner 窗口永不可见、不需要处理任何业务消息（本模块不使用延迟渲染），
    // 仅作为 `OpenClipboard` 的合法 owner 存在，交由默认窗口过程处理即可。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 懒创建并返回进程唯一的隐藏 owner 窗口句柄。
///
/// **审计报告 P0-01**：Win32 明确约定「以 `NULL` 打开剪贴板后 `EmptyClipboard` 会把
/// owner 置为 `NULL`，从而导致 `SetClipboardData` 失败」。因此所有会调用
/// `EmptyClipboard` / `SetClipboardData` 的写路径都必须以有效 HWND 打开剪贴板。
/// 这里在专用线程上创建一个隐藏顶层窗口并跑消息泵，使句柄在进程存活期内始终有效。
fn clipboard_owner() -> Option<HWND> {
    OWNER_INIT.call_once(|| {
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        let spawned = thread::Builder::new()
            .name("QuickPath-ClipboardOwner".to_string())
            .spawn(move || unsafe {
                let class_name = w!("QuickPath_ClipboardOwner_Class");
                let hinstance = GetModuleHandleW(None)
                    .map(|h| HINSTANCE(h.0))
                    .unwrap_or_default();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(clipboard_owner_proc),
                    hInstance: hinstance,
                    lpszClassName: class_name,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);

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
                    None,
                    None,
                );

                match created {
                    Ok(hwnd) if !hwnd.0.is_null() => {
                        let _ = tx.send(hwnd.0 as usize);
                        let mut msg = MSG::default();
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                    _ => {
                        let _ = tx.send(0);
                    }
                }
            });

        if spawned.is_err() {
            return;
        }
        if let Ok(raw) = rx.recv_timeout(Duration::from_secs(3))
            && raw != 0
        {
            CLIPBOARD_OWNER.store(raw as *mut _, Ordering::SeqCst);
        }
    });

    let raw = CLIPBOARD_OWNER.load(Ordering::SeqCst);
    if raw.is_null() {
        return None;
    }
    let hwnd = HWND(raw);
    unsafe { IsWindow(Some(hwnd)).as_bool().then_some(hwnd) }
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
        }
    }

    /// 记录本次向剪贴板写入临时内容后的归属信息（序列号 + 精确文本），
    /// 供 `restore()` 判断“这份临时内容是否仍归本次操作所有”。
    pub fn note_temporary_write(&mut self, text: &str, sequence: u32) {
        self.owned_seq = Some(sequence);
        self.owned_text = Some(text.to_string());
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
        // 端到端用例沿用的“该格式写回必失败”注入，等价于 SetClipboardData 失败
        let ops = HandleOps {
            set: |_, _| false,
            ..PROD_HANDLE_OPS
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

    /// 测试期故障注入开关，保证即使断言失败也不会污染后续用例
    struct FailInjection;

    impl FailInjection {
        fn enable(format: u32) -> Self {
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

    impl Drop for FailInjection {
        fn drop(&mut self) {
            TEST_FAIL_FORMAT.store(u32::MAX, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// 写入一个合法的最小 1x1 24-bit DIB，供失败注入用例复用
    fn seed_dib_clipboard() {
        let mut dib_data = [0u8; 44];
        dib_data[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize = 40
        dib_data[4..8].copy_from_slice(&1i32.to_le_bytes()); // biWidth = 1
        dib_data[8..12].copy_from_slice(&1i32.to_le_bytes()); // biHeight = 1
        dib_data[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes = 1
        dib_data[14..16].copy_from_slice(&24u16.to_le_bytes()); // biBitCount = 24
        dib_data[20..24].copy_from_slice(&4u32.to_le_bytes()); // biSizeImage = 4

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
                    assert!(
                        SetClipboardData(CF_DIB, Some(HANDLE(h.0))).is_ok(),
                        "种入 CF_DIB 必须成功"
                    );
                }
            }
            let _ = CloseClipboard();
        }
    }

    /// P0-01：单格式恢复失败时，restore() 必须返回结构化错误且绝不置 restored；
    /// 清除故障后重试必须能补齐（只补写失败格式，不重复清空已恢复内容）。
    #[test]
    fn test_restore_partial_failure_is_reported_and_retryable() {
        let _guard = clip_test_lock();
        seed_dib_clipboard();

        let mut snap = ClipboardSnapshot::capture().expect("含 DIB 的剪贴板必须可被安全捕获");
        assert!(!snap.items.is_empty(), "快照必须至少含一个格式");
        // 清掉当前剪贴板，确保后续断言只能来自快照恢复
        assert!(set_raw_clipboard_text("qp_restore_probe"));

        {
            let _inject = FailInjection::enable(CF_DIB);
            let err = snap.restore().expect_err("单格式恢复失败时不得返回成功");
            match err {
                ClipboardRestoreError::PartialRestore(ref failed) => {
                    assert!(
                        failed.contains(&CF_DIB),
                        "失败列表必须包含 CF_DIB，实际 {:?}",
                        failed
                    );
                }
                other => panic!("期望 PartialRestore，实际 {:?}", other),
            }
            assert!(!snap.restored, "部分失败时绝不能标记为已恢复");
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

    /// P0-01：`with_temporary_clipboard_text` 必须把恢复失败向上传播，而不是静默返回 Ok
    #[test]
    fn test_temporary_clipboard_text_propagates_restore_failure() {
        let _guard = clip_test_lock();
        assert!(set_raw_clipboard_text("qp_propagate_seed"));

        let _inject = FailInjection::enable(CF_UNICODETEXT);
        let outcome: Result<u32, ClipboardSafetyError> =
            with_temporary_clipboard_text("C:\\PropagateProbe", || 7);

        match outcome {
            Err(ClipboardSafetyError::RestoreFailed(err)) => {
                assert!(
                    matches!(err, ClipboardRestoreError::PartialRestore(_)),
                    "恢复失败必须是结构化的 PartialRestore，实际 {:?}",
                    err
                );
                // 内容未恢复也不能假报成功：调用方拿不到闭包结果
                assert!(
                    matches!(err, ClipboardRestoreError::PartialRestore(ref v) if v.contains(&CF_UNICODETEXT))
                );
            }
            other => panic!("期望 RestoreFailed 错误，实际 {:?}", other),
        }

        drop(_inject);
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

    /// P0-01：所有写路径必须以「有效窗口」为 owner 打开剪贴板；
    /// 且 `EmptyClipboard` 后 owner 必须生效、`SetClipboardData` 必须真正成功。
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
}
