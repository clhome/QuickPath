#![allow(dead_code)]

use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GHND, GMEM_MOVEABLE,
};

/// 剪贴板标准格式常量定义
pub const CF_TEXT: u32 = 1;
pub const CF_BITMAP: u32 = 2;
pub const CF_DIB: u32 = 8;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_HDROP: u32 = 15;
pub const CF_DIBV5: u32 = 17;

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
}

impl std::fmt::Display for ClipboardSafetyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(f, "剪贴板被其他进程占用，打开超时"),
            Self::BackupIncomplete(msg) => write!(f, "剪贴板备份未完整完成: {}", msg),
            Self::WriteFailed => write!(f, "临时写入剪贴板文本失败"),
        }
    }
}

impl std::error::Error for ClipboardSafetyError {}

/// 带指数退避的剪贴板独占打开函数
fn open_clipboard_with_retry(hwnd: Option<HWND>, max_retries: u32) -> bool {
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

/// 剪贴板快照数据，存储全量枚举格式的原始内存块副本
pub struct ClipboardSnapshot {
    /// 各格式数据 (format_id, raw_bytes)
    pub items: Vec<(u32, Vec<u8>)>,
    /// 是否在备份时原本就为空
    pub was_empty: bool,
    /// 是否已完成还原
    pub restored: bool,
}

impl ClipboardSnapshot {
    /// 完整捕获当前系统剪贴板的所有格式数据
    /// 若剪贴板被占用或关键格式无法保真备份，返回 Err 坚决不碰剪贴板
    pub fn capture() -> Result<Self, ClipboardSafetyError> {
        if !open_clipboard_with_retry(None, 4) {
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
                return Ok(Self {
                    items,
                    was_empty,
                    restored: false,
                });
            }

            was_empty = false;
            let mut fmt = 0u32;
            let mut has_hdrop = false;
            let mut hdrop_captured = false;
            let mut has_unicode = false;
            let mut unicode_captured = false;

            loop {
                fmt = EnumClipboardFormats(fmt);
                if fmt == 0 {
                    break;
                }

                if fmt == CF_HDROP {
                    has_hdrop = true;
                }
                if fmt == CF_UNICODETEXT {
                    has_unicode = true;
                }

                // 读取当前格式的数据句柄
                if let Ok(h) = GetClipboardData(fmt) {
                    if !h.0.is_null() {
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
                                std::ptr::copy_nonoverlapping(ptr as *const u8, data.as_mut_ptr(), size);
                                let _ = GlobalUnlock(HGLOBAL(h.0));

                                if fmt == CF_HDROP {
                                    hdrop_captured = true;
                                }
                                if fmt == CF_UNICODETEXT {
                                    unicode_captured = true;
                                }

                                total_bytes += size;
                                items.push((fmt, data));
                            }
                        }
                    }
                }
            }

            let _ = CloseClipboard();

            // 核心保真校验：如果系统明确存在文件列表或 Unicode 文本，但我们未能克隆到内存，视为不保真，拒绝破坏
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
        }

        Ok(Self {
            items,
            was_empty,
            restored: false,
        })
    }

    /// 还原剪贴板至快照状态
    pub fn restore(&mut self) -> bool {
        if self.restored {
            return true;
        }

        if !open_clipboard_with_retry(None, 4) {
            return false;
        }

        unsafe {
            let _ = EmptyClipboard();

            if !self.was_empty {
                for (fmt, data) in &self.items {
                    if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, data.len()) {
                        if !h.0.is_null() {
                            let ptr = GlobalLock(h);
                            if !ptr.is_null() {
                                std::ptr::copy_nonoverlapping(
                                    data.as_ptr(),
                                    ptr as *mut u8,
                                    data.len(),
                                );
                                let _ = GlobalUnlock(h);
                                let _ = SetClipboardData(*fmt, Some(HANDLE(h.0)));
                            }
                        }
                    }
                }
            }

            let _ = CloseClipboard();
        }

        self.restored = true;
        true
    }
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

    /// 显式手动还原
    pub fn restore_now(&mut self) -> bool {
        if let Some(ref mut snap) = self.snapshot {
            snap.restore()
        } else {
            true
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

/// 临时向系统剪贴板写入 Unicode 文本
fn set_raw_clipboard_text(text: &str) -> bool {
    if !open_clipboard_with_retry(None, 4) {
        return false;
    }

    unsafe {
        let _ = EmptyClipboard();
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes = wide.len() * 2;
        let mut ok = false;

        if let Ok(h) = GlobalAlloc(GHND, bytes) {
            if !h.0.is_null() {
                let ptr = GlobalLock(h);
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
                    let _ = GlobalUnlock(h);
                    let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0)));
                    ok = true;
                }
            }
        }

        let _ = CloseClipboard();
        ok
    }
}

/// 安全地在受保护的环境下临时使用剪贴板注入文本：
/// 1. 尝试完整快照当前剪贴板（含文件列表 CF_HDROP、位图、文本等）；
/// 2. 若快照失败（被占/不可保真），直接返回 Err，绝不修改剪贴板；
/// 3. 设置临时文本，执行物理操作闭包；
/// 4. 闭包结束后（无论成功或提前早退），RAII 守卫立刻无损还原剪贴板原有内容。
pub fn with_temporary_clipboard_text<R, F>(text: &str, action: F) -> Result<R, ClipboardSafetyError>
where
    F: FnOnce() -> R,
{
    let snapshot = ClipboardSnapshot::capture()?;
    let mut guard = ClipboardRestoreGuard::new(snapshot);

    if !set_raw_clipboard_text(text) {
        // 写入失败时，guard drop 会自动恢复原内容
        return Err(ClipboardSafetyError::WriteFailed);
    }

    let result = action();

    // 动作执行完毕，立即显式还原
    guard.restore_now();

    Ok(result)
}

/// 安全读取当前剪贴板的纯文本（若剪贴板无纯文本则返回 None）
pub fn get_clipboard_text() -> Option<String> {
    if !open_clipboard_with_retry(None, 4) {
        return None;
    }
    unsafe {
        let mut result = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            if !h.0.is_null() {
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
        }
        let _ = CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_safety_temporary_text_and_restore() {
        let initial_text = get_clipboard_text().unwrap_or_else(|| "initial_qp_test_data".to_string());
        let _ = set_raw_clipboard_text(&initial_text);

        let temp_injected = "C:\\Windows\\System32";
        let execution_result = with_temporary_clipboard_text(temp_injected, || {
            let inside = get_clipboard_text().unwrap_or_default();
            assert_eq!(inside, temp_injected, "闭包内部剪贴板应为临时注入的文本");
            42
        });

        assert_eq!(execution_result, Ok(42));
        let after_restore = get_clipboard_text().unwrap_or_default();
        assert_eq!(after_restore, initial_text, "闭包执行结束后必须无损还原原有剪贴板内容");
    }
}


