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
            if (has_bitmap || has_dib) && !items.iter().any(|(f, _)| *f == CF_DIB) {
                if let Ok(h) = GetClipboardData(CF_DIB) {
                    if !h.0.is_null() {
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
                if let Ok(h) = GetClipboardData(f) {
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
                }

                // 3. 【Fail-Closed 核心防御】：如果当前格式未成功捕获，检查是否为已被源格式覆盖的系统合成格式
                if !captured_this_format {
                    let is_synthesized_covered = (unicode_captured && (f == CF_TEXT || f == CF_OEMTEXT))
                        || (dib_captured && (f == CF_BITMAP || f == CF_DIBV5));

                    if !is_synthesized_covered {
                        // 检测到无法保真复制的非内存或专有格式，为确保用户数据 100% 零损失，坚决拒绝改写
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
    use std::sync::Mutex;

    static CLIP_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_clipboard_safety_temporary_text_and_restore() {
        let _guard = CLIP_TEST_LOCK.lock().unwrap();
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

    #[test]
    fn test_clipboard_safety_bitmap_dib_preservation() {
        let _guard = CLIP_TEST_LOCK.lock().unwrap();
        // 构造合法的最简 1x1 24-bit DIB 内存块
        let mut dib_data = [0u8; 40 + 4];
        dib_data[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize = 40
        dib_data[4..8].copy_from_slice(&1i32.to_le_bytes());  // biWidth = 1
        dib_data[8..12].copy_from_slice(&1i32.to_le_bytes()); // biHeight = 1
        dib_data[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes = 1
        dib_data[14..16].copy_from_slice(&24u16.to_le_bytes()); // biBitCount = 24
        dib_data[20..24].copy_from_slice(&4u32.to_le_bytes()); // biSizeImage = 4
        dib_data[40] = 50;
        dib_data[41] = 200;
        dib_data[42] = 123;
        dib_data[43] = 0;

        let original_text = get_clipboard_text();

        assert!(open_clipboard_with_retry(None, 10), "测试初始化必须成功打开剪贴板");
        unsafe {
            let _ = EmptyClipboard();
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, dib_data.len()) {
                let ptr = GlobalLock(h);
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(dib_data.as_ptr(), ptr as *mut u8, dib_data.len());
                    let _ = GlobalUnlock(h);
                    let _ = SetClipboardData(CF_DIB, Some(HANDLE(h.0)));
                }
            }
            let _ = CloseClipboard();
        }

        let snap_res = ClipboardSnapshot::capture();
        assert!(snap_res.is_ok(), "含 DIB 的剪贴板必须能被成功安全捕获: {:?}", snap_res.as_ref().err());
        let snap = snap_res.unwrap();
        assert!(snap.items.iter().any(|(f, d)| *f == CF_DIB && d.len() == dib_data.len()), "快照中必须含有 CF_DIB 数据");

        let res = with_temporary_clipboard_text("C:\\TestPath", || {
            let t = get_clipboard_text().unwrap_or_default();
            assert_eq!(t, "C:\\TestPath");
        });
        assert!(res.is_ok());

        if open_clipboard_with_retry(None, 4) {
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
}


