#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_VM_READ, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::{
    ES_READONLY, EnumChildWindows, GA_PARENT, GW_OWNER, GWL_STYLE, GetAncestor, GetClassNameW,
    GetDlgCtrlID, GetSystemMetrics, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsChild, IsWindow, IsWindowVisible, IsZoomed, SM_CXSCREEN,
    SM_CYSCREEN, WS_POPUP, WS_VISIBLE,
};
use windows::core::BOOL;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    StandardWin32,
    WpsOffice,
}

static DEBUG_MODE_ENABLED: AtomicBool = AtomicBool::new(false);

/// 检查是否启用调试模式（默认关闭，重启或重新打开自动关闭）
pub fn is_debug_mode_enabled() -> bool {
    DEBUG_MODE_ENABLED.load(Ordering::SeqCst)
}

/// 切换调试模式状态
pub fn set_debug_mode_enabled(enabled: bool) {
    let prev = DEBUG_MODE_ENABLED.swap(enabled, Ordering::SeqCst);
    if enabled && !prev {
        log_debug("=== QuickPath Debug Mode Enabled by User ===");
    }
}

/// 单一日志文件名（审计报告 P2-04：不再同时写 exe 目录与 `%TEMP%` 两份）
const DEBUG_LOG_FILE_NAME: &str = "quickpath_debug.log";
/// 单个日志文件的最大字节数（超过即轮换，只保留一份备份 → 磁盘占用上限约 4 MB）
const DEBUG_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// 轮换备份的保留期（超过即删除）
const DEBUG_LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);

/// 选定的唯一日志文件路径（懒解析：优先 exe 同级便携模式，其次 `%LOCALAPPDATA%`，最后 `%TEMP%`）
static DEBUG_LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
/// 串行化“检查大小 → 轮换 → 追加”，避免多线程同时轮换产生竞态
static DEBUG_LOG_LOCK: Mutex<()> = Mutex::new(());

/// 解析并缓存唯一日志文件路径；首次解析时顺带做一次容量/保留期治理。
fn debug_log_path() -> Option<&'static PathBuf> {
    DEBUG_LOG_PATH
        .get_or_init(|| {
            let path = resolve_debug_log_path()?;
            enforce_log_retention(&path);
            Some(path)
        })
        .as_ref()
}

fn resolve_debug_log_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        candidates.push(dir);
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("QuickPath").join("logs"));
    }
    candidates.push(std::env::temp_dir().join("QuickPath").join("logs"));

    for dir in candidates {
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let path = dir.join(DEBUG_LOG_FILE_NAME);
        // 真正尝试一次 append 打开：成功才算这个目录可写
        if std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .is_ok()
        {
            return Some(path);
        }
    }
    None
}

fn log_backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".1");
    path.with_file_name(name)
}

/// 轮换：把当前文件改名为 `.1`（覆盖旧备份），磁盘占用因此有上限。
fn rotate_log(path: &Path) {
    let backup = log_backup_path(path);
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::rename(path, &backup);
}

/// 容量与保留期治理：当前文件超限先轮换；过期备份删除。
fn enforce_log_retention(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() >= DEBUG_LOG_MAX_BYTES
    {
        rotate_log(path);
    }
    let backup = log_backup_path(path);
    if let Ok(meta) = std::fs::metadata(&backup) {
        let expired = meta
            .modified()
            .map(|m| {
                m.elapsed()
                    .map(|e| e > DEBUG_LOG_RETENTION)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if expired {
            let _ = std::fs::remove_file(&backup);
        }
    }
}

fn debug_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() % 86400;
    let hours = (secs / 3600 + 8) % 24; // UTC+8
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        hours,
        mins,
        s,
        dur.subsec_millis()
    )
}

/// 大小写不敏感的 ASCII 子串替换（保留原始大小写与字节边界）
fn replace_ascii_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let hay = haystack.to_ascii_lowercase();
    let nee = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut start = 0usize;
    while let Some(pos) = hay[start..].find(&nee) {
        let abs = start + pos;
        out.push_str(&haystack[start..abs]);
        out.push_str(replacement);
        start = abs + needle.len();
    }
    out.push_str(&haystack[start..]);
    out
}

/// 默认脱敏：把用户个人目录与用户名替换为占位符，保留其余可诊断结构。
///
/// 顺序不可调换：必须先替换 `USERPROFILE`，才能把 `C:\Users\<me>\...` 整体折叠为
/// `%USERPROFILE%`，否则后续盘符处理会把系统盘 `C:` 留下形成半脱敏。
fn sanitize_with(msg: &str, profile: Option<&str>, username: Option<&str>) -> String {
    let mut out = msg.to_string();
    if let Some(profile) = profile.filter(|p| !p.is_empty()) {
        out = replace_ascii_case_insensitive(&out, profile, "%USERPROFILE%");
    }
    if let Some(username) = username.filter(|u| !u.is_empty()) {
        out = replace_ascii_case_insensitive(&out, username, "<user>");
    }
    out
}

/// 解析 UNC 前缀后的主机/共享名，返回 `(host_start, host_end, share_end)`（字节下标）。
///
/// 覆盖普通 `\\host\share\...` 与长路径 `\\?\UNC\host\share\...` 两种形态；
/// 形如 `\\?\C:\...` 的设备路径**不**按 UNC 处理（返回 `None`），交由盘符分支处理。
fn parse_unc(bytes: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    // 调用方已确认 `bytes[start..start + 2] == b"\\\\"`。
    let host_start = if bytes.get(start + 2) == Some(&b'?') && bytes.get(start + 3) == Some(&b'\\')
    {
        if bytes.len() < start + 8
            || !bytes[start + 4..start + 7].eq_ignore_ascii_case(b"UNC")
            || bytes[start + 7] != b'\\'
        {
            return None;
        }
        start + 8
    } else {
        start + 2
    };

    let host_end = bytes[host_start..].iter().position(|&b| b == b'\\')? + host_start;
    if host_end == host_start {
        return None;
    }
    let share_start = host_end + 1;
    if share_start >= bytes.len() {
        return None;
    }
    let share_end = bytes[share_start..]
        .iter()
        .position(|&b| b == b'\\')
        .map(|pos| pos + share_start)
        .unwrap_or(bytes.len());
    if share_end == share_start {
        return None;
    }
    Some((host_start, host_end, share_end))
}

/// 解析盘符前缀 `X:\` / `x:/`，返回 `(消费到的字节下标, 大写盘符)`。
///
/// 盘符前必须是字符串开头或非字母数字字符，避免把 `http://` 中的 `p:/` 误判为盘符。
fn parse_drive_letter(bytes: &[u8], index: usize) -> Option<(usize, u8)> {
    let letter = *bytes.get(index)?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    if bytes.get(index + 1) != Some(&b':') {
        return None;
    }
    match bytes.get(index + 2) {
        Some(&b'\\') | Some(&b'/') => {}
        _ => return None,
    }
    if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
        return None;
    }
    Some((index + 2, letter.to_ascii_uppercase()))
}

/// 路径类脱敏（纯函数，`system_drive` 由调用方注入以便单测）。
///
/// | 模式 | 替换 | 理由 |
/// | --- | --- | --- |
/// | `\\host\share\...`、`\\?\UNC\host\share\...` | `\\<unc>\<share>\...` | 主机/共享名是网络拓扑标识；保留后续相对目录便于诊断 |
/// | 非系统盘符 `X:\...`（X ≠ 系统盘） | `%DRIVE_X%:\...` | 盘符可能指向含用户数据的盘；保留相对结构 |
/// | 系统盘 `C:\...`（含非用户目录） | **保留** | `C:\Windows\...`、`C:\Program Files\...` 是诊断关键信息且不含用户身份；用户目录已由 `%USERPROFILE%`/`<user>` 覆盖 |
fn sanitize_paths_with(msg: &str, system_drive: Option<&str>) -> String {
    // 系统盘不可知（环境变量缺失）时按最普遍的 `C:` 处理：宁可保留
    // `C:\Windows\...` 这类诊断信息，也不要把系统盘整盘抹掉。
    let system_letter = system_drive
        .and_then(|drive| drive.trim_start().bytes().next())
        .filter(|b| b.is_ascii_alphabetic())
        .map(|b| b.to_ascii_uppercase())
        .unwrap_or(b'C');

    let bytes = msg.as_bytes();
    let mut out = String::with_capacity(msg.len());
    let mut pending = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b'\\')
            && let Some((host_start, _host_end, share_end)) = parse_unc(bytes, i)
        {
            out.push_str(&msg[pending..i]);
            out.push_str(&msg[i..host_start]);
            out.push_str("<unc>\\<share>");
            pending = share_end;
            i = share_end;
            continue;
        }
        if let Some((end, letter)) = parse_drive_letter(bytes, i)
            && letter != system_letter
        {
            out.push_str(&msg[pending..i]);
            out.push_str(&format!("%DRIVE_{}%:", letter as char));
            pending = end;
            i = end;
            continue;
        }
        i += 1;
    }
    out.push_str(&msg[pending..]);
    out
}

/// 完整脱敏（纯函数）：先做既有的个人目录/用户名替换，再做 UNC 与非系统盘符脱敏。
fn sanitize_full_with(
    msg: &str,
    profile: Option<&str>,
    username: Option<&str>,
    system_drive: Option<&str>,
) -> String {
    sanitize_paths_with(&sanitize_with(msg, profile, username), system_drive)
}

/// 读取环境变量后脱敏（生产路径）
fn sanitize_debug_message(msg: &str) -> String {
    let profile = std::env::var_os("USERPROFILE").map(|v| v.to_string_lossy().to_string());
    let username = std::env::var_os("USERNAME").map(|v| v.to_string_lossy().to_string());
    let system_drive = std::env::var_os("SystemDrive").map(|v| v.to_string_lossy().to_string());
    sanitize_full_with(
        msg,
        profile.as_deref(),
        username.as_deref(),
        system_drive.as_deref(),
    )
}

/// 写入调试日志。
///
/// 治理约束（审计报告 P2-04）：
/// 1. **单一目录**：只写一个文件，不再同时写 exe 目录与 `%TEMP%`；
/// 2. **大小上限 + 轮换**：超过 2 MB 轮换为 `.1`，只保留一份备份；
/// 3. **保留期**：轮换备份超过 7 天自动删除；
/// 4. **默认脱敏**：用户个人目录与用户名写入前替换为占位符。
pub fn log_debug(msg: &str) {
    if !is_debug_mode_enabled() {
        return;
    }
    let Some(path) = debug_log_path() else {
        return;
    };

    let line = format!("[{}] {}\n", debug_timestamp(), sanitize_debug_message(msg));

    let _guard = DEBUG_LOG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() >= DEBUG_LOG_MAX_BYTES
    {
        rotate_log(path);
    }

    use std::io::Write;
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// 返回当前调试日志文件的绝对路径字符串；日志路径不可用（无任何可写目录）时返回 `None`。
pub fn debug_log_path_display() -> Option<String> {
    debug_log_path().map(|path| path.to_string_lossy().into_owned())
}

/// 删除给定日志文件与其 `.1` 备份；返回无法删除的条目描述（空表示全部成功或本就不存在）。
fn remove_log_files(path: &Path) -> Vec<String> {
    let backup = log_backup_path(path);
    let mut failures = Vec::new();
    for target in [path, backup.as_path()] {
        match std::fs::remove_file(target) {
            Ok(()) => {}
            // 文件本就不存在视为“已清理”，不算失败（幂等）。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failures.push(format!("{} ({})", target.display(), e)),
        }
    }
    failures
}

/// 清理当前调试日志与 `.1` 备份。
///
/// 与 `log_debug` 的「检查大小 → 轮换 → 追加」共用同一把 `DEBUG_LOG_LOCK`，
/// 因此不会与写入/轮换交错（否则可能删掉刚轮换出的备份，或与追加并发造成残留）。
/// 失败时输出结构化诊断并返回 `false`，绝不 panic。
pub fn clear_debug_logs() -> bool {
    let Some(path) = debug_log_path() else {
        return false;
    };

    // 只在锁内做删除，出锁后再写日志：`log_debug` 会再次取同一把锁，持锁调用会自锁。
    let failures = {
        let _guard = DEBUG_LOG_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_log_files(path)
    };

    if failures.is_empty() {
        // 成功不写日志，避免清理后立刻重建日志文件。
        true
    } else {
        log_debug(&format!("CLEAR_LOG_FAILED: {}", failures.join("; ")));
        false
    }
}

#[derive(Debug, Clone)]
pub struct FileDialogInfo {
    pub hwnd: HWND,
    pub process_id: u32,
    pub process_name: String,
    pub window_title: String,
    pub rect: RECT,
    pub file_name_edit_hwnd: Option<HWND>,
    pub kind: DialogKind,
}

struct ChildEnumContext {
    has_list_view: bool,
    has_file_name_combo: bool,
    has_toolbar_or_breadcrumb: bool,
    edit_hwnds: Vec<HWND>,
}

const EDT1: i32 = 0x480;
const CMB13: i32 = 0x47C;

struct StandardChildEnumContext {
    dialog_hwnd: HWND,
    has_list_view: bool,
    has_file_name_combo: bool,
    edit_count: usize,
    file_name_edit: Option<(u8, HWND)>,
}

/// 标准识别只依赖真实类名、可见状态、列表和有效文件名输入，不以标题放行。
fn is_standard_file_dialog(
    class_name: &str,
    visible: bool,
    has_list_view: bool,
    has_file_name_edit: bool,
) -> bool {
    class_name == "#32770" && visible && has_list_view && has_file_name_edit
}

fn file_name_edit_priority(
    usable: bool,
    edit_id: i32,
    has_combo_ancestor: bool,
    has_file_name_combo_id: bool,
    has_address_or_search_ancestor: bool,
) -> Option<u8> {
    if !usable || has_address_or_search_ancestor {
        None
    } else if edit_id == EDT1 {
        Some(0)
    } else if has_file_name_combo_id {
        Some(1)
    } else if has_combo_ancestor {
        Some(2)
    } else {
        None
    }
}

// 根窗口的可见性由检测入口检查；逐层检查子控件样式也便于隐藏测试窗口枚举。
fn is_visible_dialog_child(dialog_hwnd: HWND, child: HWND) -> bool {
    unsafe {
        if !IsWindow(Some(dialog_hwnd)).as_bool() || !IsChild(dialog_hwnd, child).as_bool() {
            return false;
        }
        let mut current = child;
        while current != dialog_hwnd {
            if current.0.is_null()
                || (GetWindowLongW(current, GWL_STYLE) as u32 & WS_VISIBLE.0) == 0
            {
                return false;
            }
            current = GetAncestor(current, GA_PARENT);
        }
        true
    }
}

fn collect_standard_children(dialog_hwnd: HWND) -> StandardChildEnumContext {
    let mut ctx = StandardChildEnumContext {
        dialog_hwnd,
        has_list_view: false,
        has_file_name_combo: false,
        edit_count: 0,
        file_name_edit: None,
    };
    unsafe {
        if IsWindow(Some(dialog_hwnd)).as_bool() {
            let _ = EnumChildWindows(
                Some(dialog_hwnd),
                Some(enum_standard_children_proc),
                LPARAM(&mut ctx as *mut _ as isize),
            );
        }
    }
    ctx
}

/// 检查进程名是否属于 WPS Office 套件（兼容各版本、多进程命名）
pub fn is_wps_process_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("wps")
        || lower.contains("kso")
        || lower.contains("kingsoft")
        || lower == "et.exe"
        || lower == "wpp.exe"
}

/// 检查是否为 WPS Office 的主程序窗口或文档编辑主窗口（一票否决排除非对话框）
pub fn is_wps_main_window(hwnd: HWND, class_name: &str, title: &str) -> bool {
    let lower_title = title.to_lowercase();
    let lower_class = class_name.to_lowercase();

    // 1. 经典主文档/主框架类名排除
    if lower_class == "opusapp"
        || lower_class == "xlmain"
        || lower_class == "pp9frameclass"
        || lower_class == "kxpdfmainwindow"
    {
        return true;
    }

    // 2. 标题为主程序启动台或主文档标题排除：
    // - 精确为 "WPS Office" 或 "WPS"
    // - 结尾包含 " - wps office" 或 " - wps pdf" 或 " - wps 文字" 或 " - wps 表格" 或 " - wps 演示" 等
    // - 包含 "wps office" / "wps pdf" 且不属于明确的文件对话框标题
    if title == "WPS Office"
        || title == "WPS"
        || lower_title.ends_with(" - wps office")
        || lower_title.ends_with(" - wps pdf")
        || lower_title.ends_with(" - wps 文字")
        || lower_title.ends_with(" - wps 表格")
        || lower_title.ends_with(" - wps 演示")
        || (lower_title.contains("wps office") && !is_file_dialog_title(title))
        || (lower_title.contains("wps pdf") && !is_file_dialog_title(title))
    {
        return true;
    }

    // 3. 状态与尺寸判定：若窗口为最大化状态，且标题不包含明确文件对话框语义，绝非文件对话框
    unsafe {
        if IsWindow(Some(hwnd)).as_bool()
            && IsZoomed(hwnd).as_bool()
            && !is_file_dialog_title(title)
        {
            return true;
        }
    }

    false
}

/// 向上查找文件对话框顶层根窗口句柄
pub fn find_dialog_root(raw_hwnd: HWND) -> HWND {
    unsafe {
        // 1. 优先获取顶层根窗口 GA_ROOT（将所有嵌入子控件或原生子部件提升至顶层）
        let root = windows::Win32::UI::WindowsAndMessaging::GetAncestor(
            raw_hwnd,
            windows::Win32::UI::WindowsAndMessaging::GA_ROOT,
        );
        if !root.0.is_null() && IsWindow(Some(root)).as_bool() {
            let root_class = get_window_class_name(root);
            if root_class == "#32770"
                || root_class == "KcfdFileDialog"
                || root_class == "Qt5QWindowIcon"
                || root_class.contains("Kcfd")
            {
                return root;
            }
        }

        // 2. 若当前窗口本身就是常见对话框类名
        let class = get_window_class_name(raw_hwnd);
        if class == "#32770"
            || class == "KcfdFileDialog"
            || class == "Qt5QWindowIcon"
            || class.contains("Kcfd")
        {
            return raw_hwnd;
        }

        // 3. 逐层向上查找父窗口（最多 10 层，适应模态嵌入子对话框）
        let mut curr = raw_hwnd;
        for _ in 0..10 {
            let parent = windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                curr,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            );
            if parent.0.is_null() || !IsWindow(Some(parent)).as_bool() || parent == curr {
                break;
            }
            let p_class = get_window_class_name(parent);
            if p_class == "#32770"
                || p_class == "KcfdFileDialog"
                || p_class == "Qt5QWindowIcon"
                || p_class.contains("Kcfd")
            {
                return parent;
            }
            curr = parent;
        }

        if !root.0.is_null() && IsWindow(Some(root)).as_bool() {
            root
        } else {
            raw_hwnd
        }
    }
}

/// 检查窗口标题是否属于常见的文件选择/保存对话框标题（支持中英文、大小写不敏感）
pub fn is_file_dialog_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    title.contains("另存为")
        || title.contains("打开")
        || lower.contains("save as")
        || lower.contains("open")
}

/// 探测 WPS Office 专用的文件对话框（涵盖自绘 Qt5QWindowIcon、KcfdFileDialog 与换肤定制的 #32770）
fn detect_wps_dialog(raw_hwnd: HWND, pid: u32, process_name: String) -> Option<FileDialogInfo> {
    unsafe {
        let hwnd = find_dialog_root(raw_hwnd);

        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }

        let class_name = get_window_class_name(hwnd);
        let window_title = get_window_title(hwnd);

        // 1. 核心防御：若当前窗口判定为 WPS 主窗口，一票否决，直接拦截
        if is_wps_main_window(hwnd, &class_name, &window_title) {
            log_debug(&format!(
                "detect_wps REJECT: hwnd=0x{:X} is WPS main window (cls='{}' title='{}')",
                hwnd.0 as usize, class_name, window_title
            ));
            return None;
        }

        // 枚举子控件以确认特征
        let mut ctx = ChildEnumContext {
            has_list_view: false,
            has_file_name_combo: false,
            has_toolbar_or_breadcrumb: false,
            edit_hwnds: Vec::new(),
        };

        let _ = EnumChildWindows(
            Some(hwnd),
            Some(enum_children_proc),
            LPARAM(&mut ctx as *mut _ as isize),
        );

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;

        // WPS 核心特征精准匹配：
        // 规则 1：Kcfd 自绘专用类名（WPS 企业版自绘对话框，如 KcfdFileDialog）
        let is_kcfd = class_name == "KcfdFileDialog" || class_name.contains("Kcfd");

        // 规则 2：标准模态对话框 #32770（需具备子控件特征）
        let is_32770 = class_name == "#32770";
        let has_dialog_feature = ctx.has_list_view
            || ctx.has_file_name_combo
            || ctx.has_toolbar_or_breadcrumb
            || !ctx.edit_hwnds.is_empty();

        // 规则 3：Qt5QWindowIcon（WPS 个人版自绘对话框）精准判别
        let is_qt5 = class_name == "Qt5QWindowIcon";
        let is_dialog_title = !window_title.is_empty() && is_file_dialog_title(&window_title);

        let is_qt5_dialog = if is_qt5 {
            if is_dialog_title {
                // 分支 3.1：标题明确带有“另存为/打开/Save As/Open”，且已排除主窗口
                width >= 300 && height >= 200
            } else if window_title.is_empty() {
                // 分支 3.2：自绘无标题弹窗。必须严格验证弹窗属性以防误判无标题的主窗口
                let owner = GetWindow(hwnd, GW_OWNER);
                let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                let is_popup = (style & WS_POPUP.0) != 0;
                let has_owner = if let Ok(owner_hwnd) = owner {
                    !owner_hwnd.0.is_null() && IsWindow(Some(owner_hwnd)).as_bool()
                } else {
                    false
                };
                let is_zoomed = IsZoomed(hwnd).as_bool();

                let screen_w = GetSystemMetrics(SM_CXSCREEN);
                let screen_h = GetSystemMetrics(SM_CYSCREEN);
                let not_full_screen = (screen_w <= 0 || width < screen_w - 60)
                    && (screen_h <= 0 || height < screen_h - 100);

                // 必须满足：拥有 Owner 或具有 WS_POPUP 弹窗样式，且非最大化、非全屏尺寸、符合合理对话框区间
                (is_popup || has_owner)
                    && !is_zoomed
                    && not_full_screen
                    && (400..=1600).contains(&width)
                    && (250..=1100).contains(&height)
            } else {
                false
            }
        } else {
            false
        };

        let is_valid = is_kcfd
            || is_qt5_dialog
            || (is_32770 && has_dialog_feature)
            || (is_dialog_title && (has_dialog_feature || is_32770));

        if !is_valid {
            log_debug(&format!(
                "detect_wps REJECT: hwnd=0x{:X} cls='{}' title='{}' w={} h={} kcfd={} qt5_dlg={} 32770={}",
                hwnd.0 as usize,
                class_name,
                window_title,
                width,
                height,
                is_kcfd,
                is_qt5_dialog,
                is_32770
            ));
            return None;
        }

        // 优先使用枚举到的 Edit 控件；自绘窗口无 Win32 Edit 则为 None（后续走 UIA）
        let file_name_edit = ctx.edit_hwnds.first().copied();
        let effective_title = if window_title.is_empty() {
            "另存为".to_string()
        } else {
            window_title
        };

        Some(FileDialogInfo {
            hwnd,
            process_id: pid,
            process_name,
            window_title: effective_title,
            rect,
            file_name_edit_hwnd: file_name_edit,
            kind: DialogKind::WpsOffice,
        })
    }
}

/// 探测标准 Win32 (#32770) 文件对话框，兼容现代和经典文件名控件。
fn detect_standard_dialog(
    raw_hwnd: HWND,
    pid: u32,
    process_name: String,
) -> Option<FileDialogInfo> {
    unsafe {
        let hwnd = find_dialog_root(raw_hwnd);

        if !IsWindow(Some(hwnd)).as_bool() || !IsWindowVisible(hwnd).as_bool() {
            log_debug(&format!(
                "detect_std REJECT: hwnd=0x{:X} not visible",
                hwnd.0 as usize
            ));
            return None;
        }

        let class_name = get_window_class_name(hwnd);
        // 大多数标准与通用文件对话框的类名均为 #32770
        if class_name != "#32770" {
            return None;
        }

        let ctx = collect_standard_children(hwnd);
        let file_name_edit = ctx.file_name_edit.map(|(_, edit)| edit);

        // 列表与有效文件名 Edit 缺一不可；搜索框、地址栏和未就绪控件不能补足特征。
        if !is_standard_file_dialog(
            &class_name,
            true,
            ctx.has_list_view,
            file_name_edit.is_some(),
        ) {
            log_debug(&format!(
                "detect_std REJECT: hwnd=0x{:X} list={} combo={} edits={}",
                hwnd.0 as usize, ctx.has_list_view, ctx.has_file_name_combo, ctx.edit_count
            ));
            return None;
        }

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let window_title = get_window_title(hwnd);

        Some(FileDialogInfo {
            hwnd,
            process_id: pid,
            process_name,
            window_title,
            rect,
            file_name_edit_hwnd: file_name_edit,
            kind: DialogKind::StandardWin32,
        })
    }
}

/// 探测指定窗口是否为标准或通用的“打开/另存为”文件对话框（含 WPS Office）
pub fn detect_file_dialog(raw_hwnd: HWND) -> Option<FileDialogInfo> {
    unsafe {
        if !IsWindow(Some(raw_hwnd)).as_bool() {
            return None;
        }

        let (pid, process_name) = get_window_process_info(raw_hwnd);
        let class_name = get_window_class_name(raw_hwnd);
        let title = get_window_title(raw_hwnd);
        let root = find_dialog_root(raw_hwnd);
        let root_class = get_window_class_name(root);
        let root_title = get_window_title(root);

        log_debug(&format!(
            "DETECT_START: raw=0x{:X} pid={} proc='{}' cls='{}' title='{}' | root=0x{:X} r_cls='{}' r_title='{}'",
            raw_hwnd.0 as usize,
            pid,
            process_name,
            class_name,
            title,
            root.0 as usize,
            root_class,
            root_title
        ));

        // 1. 若为 WPS 进程，严格走 WPS 专属自绘/Qt 对话框探测通道
        if is_wps_process_name(&process_name) {
            if let Some(wps_info) = detect_wps_dialog(raw_hwnd, pid, process_name.clone()) {
                log_debug(&format!(
                    "DETECT_HIT_WPS: hwnd=0x{:X} title='{}' rect={:?}",
                    wps_info.hwnd.0 as usize, wps_info.window_title, wps_info.rect
                ));
                return Some(wps_info);
            }
            return None;
        }

        // 2. 非 WPS 进程：只走具有列表和有效文件名输入的标准 #32770 对话框通道
        if let Some(std_info) = detect_standard_dialog(raw_hwnd, pid, process_name.clone()) {
            log_debug(&format!(
                "DETECT_HIT_STD: hwnd=0x{:X} title='{}' rect={:?}",
                std_info.hwnd.0 as usize, std_info.window_title, std_info.rect
            ));
            Some(std_info)
        } else {
            None
        }
    }
}

/// 标准 Win32 对话框子枚举：优先 edt1/cmb13，再选择非地址/搜索组合框的 Edit。
unsafe extern "system" fn enum_standard_children_proc(child: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lparam.0 as *mut StandardChildEnumContext);
        if !is_visible_dialog_child(ctx.dialog_hwnd, child) {
            return BOOL(1);
        }
        let class = get_window_class_name(child);

        if class == "DirectUIHWND" || class == "SHELLDLL_DefView" || class == "SysListView32" {
            ctx.has_list_view = true;
        } else if class == "ComboBoxEx32" || class.eq_ignore_ascii_case("ComboBox") {
            ctx.has_file_name_combo = true;
        } else if class.eq_ignore_ascii_case("Edit") {
            ctx.edit_count += 1;
            let mut parent = GetAncestor(child, GA_PARENT);
            let mut has_combo = false;
            let mut has_file_name_combo_id = false;
            let mut has_address_or_search = false;
            while !parent.0.is_null() && parent != ctx.dialog_hwnd {
                let parent_class = get_window_class_name(parent);
                let lower_class = parent_class.to_ascii_lowercase();
                if parent_class == "ComboBoxEx32" || parent_class.eq_ignore_ascii_case("ComboBox") {
                    has_combo = true;
                    has_file_name_combo_id |= GetDlgCtrlID(parent) == CMB13;
                }
                has_address_or_search |= lower_class.contains("toolbar")
                    || lower_class.contains("breadcrumb")
                    || lower_class.contains("address")
                    || lower_class.contains("search");
                parent = GetAncestor(parent, GA_PARENT);
            }
            let usable = IsWindowEnabled(child).as_bool()
                && (GetWindowLongW(child, GWL_STYLE) & ES_READONLY) == 0;
            if let Some(priority) = file_name_edit_priority(
                usable,
                GetDlgCtrlID(child),
                has_combo,
                has_file_name_combo_id,
                has_address_or_search,
            ) && ctx
                .file_name_edit
                .is_none_or(|(current, _)| priority < current)
            {
                ctx.file_name_edit = Some((priority, child));
            }
        }

        BOOL(1)
    }
}

/// WPS Office 专用子控件枚举过程
unsafe extern "system" fn enum_children_proc(child: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lparam.0 as *mut ChildEnumContext);
        let class = get_window_class_name(child);

        if class == "DirectUIHWND" || class == "SHELLDLL_DefView" || class == "SysListView32" {
            ctx.has_list_view = true;
        } else if class == "ComboBoxEx32" || class.eq_ignore_ascii_case("ComboBox") {
            ctx.has_file_name_combo = true;
        } else if class.starts_with("ToolbarWindow32")
            || class.contains("Breadcrumb")
            || class == "NamespaceTreeControl"
        {
            ctx.has_toolbar_or_breadcrumb = true;
        } else if class.eq_ignore_ascii_case("Edit") && IsWindowVisible(child).as_bool() {
            ctx.edit_hwnds.push(child);
        }

        BOOL(1)
    }
}

pub fn get_window_class_name(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf);
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

pub fn get_window_title(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

pub fn get_window_process_info(hwnd: HWND) -> (u32, String) {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return (0, String::new());
        }

        // 1. 优先使用 PROCESS_QUERY_LIMITED_INFORMATION 配合 QueryFullProcessImageNameW
        // 跨架构兼容性极佳（支持 64 位读取 32 位 WPS 进程，免去 PROCESS_VM_READ 权限被拒问题）
        if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut name_buf = [0u16; 1024];
            let mut len = name_buf.len() as u32;
            if QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(name_buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok()
                && len > 0
            {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
                let full_path = String::from_utf16_lossy(&name_buf[..len as usize]);
                let file_name = Path::new(&full_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or(full_path);
                return (pid, file_name);
            }
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }

        // 2. 降级回退使用传统 GetModuleFileNameExW
        if let Ok(handle) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) {
            let mut name_buf = [0u16; 1024];
            let len = GetModuleFileNameExW(Some(handle), None, &mut name_buf);
            let _ = windows::Win32::Foundation::CloseHandle(handle);

            if len > 0 {
                let full_path = String::from_utf16_lossy(&name_buf[..len as usize]);
                let file_name = Path::new(&full_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or(full_path);
                return (pid, file_name);
            }
        }

        (pid, String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, HMENU, RegisterClassW, WINDOW_EX_STYLE,
        WINDOW_STYLE, WNDCLASSW, WS_CHILD,
    };
    use windows::core::{PCWSTR, w};

    struct TestDialog(HWND);

    unsafe extern "system" fn test_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    impl TestDialog {
        fn new() -> Self {
            static REGISTER: std::sync::Once = std::sync::Once::new();
            REGISTER.call_once(|| unsafe {
                let instance = HINSTANCE(GetModuleHandleW(None).unwrap().0);
                for class in [
                    w!("DirectUIHWND"),
                    w!("SHELLDLL_DefView"),
                    w!("SysListView32"),
                    w!("ComboBoxEx32"),
                    w!("ToolbarWindow32"),
                    w!("TestBreadcrumb"),
                    w!("TestSearchBox"),
                ] {
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(test_window_proc),
                        hInstance: instance,
                        lpszClassName: class,
                        ..Default::default()
                    };
                    assert_ne!(RegisterClassW(&wc), 0);
                }
            });
            unsafe {
                // 真正 #32770 根窗口保持隐藏，测试只枚举自建子树，不操作实际软件。
                Self(
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("#32770"),
                        w!("Open"),
                        WS_POPUP,
                        0,
                        0,
                        640,
                        480,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap(),
                )
            }
        }

        fn child(&self, parent: HWND, class: PCWSTR, id: i32, style: WINDOW_STYLE) -> HWND {
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class,
                    w!(""),
                    WS_CHILD | style,
                    0,
                    0,
                    200,
                    100,
                    Some(parent),
                    Some(HMENU(id as usize as *mut _)),
                    None,
                    None,
                )
                .unwrap()
            }
        }

        fn combo_edit(&self, parent: HWND, id: i32, visible: bool) -> HWND {
            let style = WINDOW_STYLE(2) | if visible { WS_VISIBLE } else { WINDOW_STYLE(0) };
            let combo = self.child(parent, w!("ComboBox"), id, style);
            // 直接取得真实 ComboBox 自动创建的 Edit，包含隐藏组合框情况。
            let mut child = unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetWindow(
                    combo,
                    windows::Win32::UI::WindowsAndMessaging::GW_CHILD,
                )
                .unwrap()
            };
            while get_window_class_name(child) != "Edit" {
                assert!(!child.0.is_null());
                child = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetWindow(
                        child,
                        windows::Win32::UI::WindowsAndMessaging::GW_HWNDNEXT,
                    )
                    .unwrap()
                };
            }
            child
        }
    }

    impl Drop for TestDialog {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn test_standard_predicate_requires_ready_controls() {
        assert!(is_standard_file_dialog("#32770", true, true, true));
        assert!(!is_standard_file_dialog("Qt5QWindowIcon", true, true, true));
        assert!(!is_standard_file_dialog("#32770", false, true, true));
        assert!(!is_standard_file_dialog("#32770", true, false, true));
        assert!(!is_standard_file_dialog("#32770", true, true, false));
    }

    #[test]
    fn test_file_name_priority_excludes_search_and_address() {
        assert_eq!(
            file_name_edit_priority(true, EDT1, false, false, false),
            Some(0)
        );
        assert_eq!(file_name_edit_priority(true, 0, true, true, false), Some(1));
        assert_eq!(
            file_name_edit_priority(true, 0, true, false, false),
            Some(2)
        );
        assert_eq!(file_name_edit_priority(true, 0, false, false, false), None);
        assert_eq!(file_name_edit_priority(true, EDT1, true, true, true), None);
        assert_eq!(
            file_name_edit_priority(false, EDT1, true, true, false),
            None
        );
    }

    #[test]
    fn test_hidden_modern_dialog_selects_filename_not_search() {
        let dialog = TestDialog::new();
        let search = dialog.child(dialog.0, w!("Edit"), 10, WS_VISIBLE);
        dialog.child(dialog.0, w!("DirectUIHWND"), 0, WS_VISIBLE);
        let edit = dialog.combo_edit(dialog.0, CMB13, true);
        let ctx = collect_standard_children(dialog.0);
        assert!(ctx.has_list_view && ctx.has_file_name_combo);
        assert_eq!(ctx.file_name_edit, Some((1, edit)));
        assert_ne!(edit, search);
        assert_eq!(find_dialog_root(edit), dialog.0);
        assert!(is_standard_file_dialog(
            &get_window_class_name(dialog.0),
            true,
            ctx.has_list_view,
            ctx.file_name_edit.is_some()
        ));
        assert!(detect_standard_dialog(edit, 0, String::new()).is_none());
    }

    #[test]
    fn test_hidden_classic_dialog_accepts_listview_and_edt1() {
        let dialog = TestDialog::new();
        dialog.child(dialog.0, w!("SysListView32"), 0, WS_VISIBLE);
        let edit = dialog.child(dialog.0, w!("Edit"), EDT1, WS_VISIBLE);
        let ctx = collect_standard_children(dialog.0);
        assert!(is_standard_file_dialog(
            "#32770",
            true,
            ctx.has_list_view,
            ctx.file_name_edit.is_some()
        ));
        assert_eq!(ctx.file_name_edit, Some((0, edit)));
    }

    #[test]
    fn test_hidden_comboboxex_fallback_and_id_priority() {
        let dialog = TestDialog::new();
        dialog.child(dialog.0, w!("SHELLDLL_DefView"), 0, WS_VISIBLE);
        let combo = dialog.child(dialog.0, w!("ComboBoxEx32"), 0, WS_VISIBLE);
        let fallback = dialog.child(combo, w!("Edit"), 0, WS_VISIBLE);
        assert_eq!(
            collect_standard_children(dialog.0).file_name_edit,
            Some((2, fallback))
        );
        let named_combo = dialog.child(dialog.0, w!("ComboBoxEx32"), CMB13, WS_VISIBLE);
        let named = dialog.child(named_combo, w!("Edit"), 0, WS_VISIBLE);
        assert_eq!(
            collect_standard_children(dialog.0).file_name_edit,
            Some((1, named))
        );
        let edt1 = dialog.child(dialog.0, w!("Edit"), EDT1, WS_VISIBLE);
        assert_eq!(
            collect_standard_children(dialog.0).file_name_edit,
            Some((0, edt1))
        );
        let ordinary = TestDialog::new();
        ordinary.child(ordinary.0, w!("SysListView32"), 0, WS_VISIBLE);
        let edit = ordinary.combo_edit(ordinary.0, 99, true);
        assert_eq!(
            collect_standard_children(ordinary.0).file_name_edit,
            Some((2, edit))
        );
    }

    #[test]
    fn test_hidden_properties_open_title_is_not_sufficient() {
        let dialog = TestDialog::new();
        dialog.child(dialog.0, w!("Edit"), 10, WS_VISIBLE);
        let combo = dialog.child(dialog.0, w!("ComboBoxEx32"), 0, WS_VISIBLE);
        dialog.child(combo, w!("Edit"), 0, WS_VISIBLE);
        let ctx = collect_standard_children(dialog.0);
        assert!(is_file_dialog_title(&get_window_title(dialog.0)));
        assert!(!is_standard_file_dialog(
            "#32770",
            true,
            ctx.has_list_view,
            ctx.file_name_edit.is_some()
        ));
        // 列表加独立普通 Edit 也不足以识别文件名。
        let search_only = TestDialog::new();
        search_only.child(search_only.0, w!("SysListView32"), 0, WS_VISIBLE);
        search_only.child(search_only.0, w!("Edit"), 10, WS_VISIBLE);
        let ctx = collect_standard_children(search_only.0);
        assert!(!is_standard_file_dialog(
            "#32770",
            true,
            ctx.has_list_view,
            ctx.file_name_edit.is_some()
        ));
    }

    #[test]
    fn test_hidden_address_and_search_ancestors_are_excluded() {
        let dialog = TestDialog::new();
        dialog.child(dialog.0, w!("DirectUIHWND"), 0, WS_VISIBLE);
        for class in [
            w!("ToolbarWindow32"),
            w!("TestBreadcrumb"),
            w!("TestSearchBox"),
        ] {
            let container = dialog.child(dialog.0, class, 0, WS_VISIBLE);
            let combo = dialog.child(container, w!("ComboBoxEx32"), CMB13, WS_VISIBLE);
            dialog.child(combo, w!("Edit"), EDT1, WS_VISIBLE);
        }
        let ctx = collect_standard_children(dialog.0);
        assert!(ctx.has_list_view);
        assert!(ctx.file_name_edit.is_none());
    }

    #[test]
    fn test_hidden_unready_dialog_requires_usable_edit() {
        let dialog = TestDialog::new();
        dialog.child(dialog.0, w!("DirectUIHWND"), 0, WS_VISIBLE);
        dialog.child(dialog.0, w!("ComboBoxEx32"), CMB13, WS_VISIBLE);
        assert!(collect_standard_children(dialog.0).file_name_edit.is_none());
        let hidden = dialog.combo_edit(dialog.0, CMB13, false);
        assert!(collect_standard_children(dialog.0).file_name_edit.is_none());
        assert!(unsafe { IsWindow(Some(hidden)).as_bool() });
        let disabled = dialog.child(dialog.0, w!("Edit"), EDT1, WS_VISIBLE);
        unsafe {
            let _ = EnableWindow(disabled, false);
        }
        dialog.child(
            dialog.0,
            w!("Edit"),
            EDT1,
            WS_VISIBLE | WINDOW_STYLE(ES_READONLY as u32),
        );
        assert!(collect_standard_children(dialog.0).file_name_edit.is_none());
    }

    #[test]
    fn test_is_wps_process_name() {
        assert!(is_wps_process_name("wps.exe"));
        assert!(is_wps_process_name("WPS.EXE"));
        assert!(is_wps_process_name("wpsoffice.exe"));
        assert!(is_wps_process_name("WPSOffice.exe"));
        assert!(is_wps_process_name("et.exe"));
        assert!(is_wps_process_name("ET.EXE"));
        assert!(is_wps_process_name("wpp.exe"));
        assert!(is_wps_process_name("wpspdf.exe"));
        assert!(is_wps_process_name("kso.exe"));
        assert!(is_wps_process_name("ksolaunch.exe"));
        assert!(is_wps_process_name("kingsoft.exe"));

        assert!(!is_wps_process_name("explorer.exe"));
        assert!(!is_wps_process_name("notepad.exe"));
        assert!(!is_wps_process_name("chrome.exe"));
    }

    #[test]
    fn test_debug_mode_switch() {
        set_debug_mode_enabled(false);
        assert!(!is_debug_mode_enabled());

        set_debug_mode_enabled(true);
        assert!(is_debug_mode_enabled());

        set_debug_mode_enabled(false);
        assert!(!is_debug_mode_enabled());
    }

    #[test]
    fn test_standard_dialog_title_matching() {
        let titles_hit = [
            "另存为",
            "另存为...",
            "打开",
            "打开文件",
            "Save As",
            "Save as file",
            "Open",
            "Open File",
        ];
        let titles_miss = [
            "属性",
            "关于 Notepad3",
            "确认删除",
            "设置",
            "Settings",
            "Preferences",
        ];

        for t in titles_hit {
            assert!(is_file_dialog_title(t), "Should match dialog title: {}", t);
        }

        for t in titles_miss {
            assert!(
                !is_file_dialog_title(t),
                "Should NOT match dialog title: {}",
                t
            );
        }
    }

    #[test]
    fn test_is_wps_main_window() {
        let dummy_hwnd = HWND(std::ptr::null_mut());

        // 1. 应当被精准排除的 WPS 主窗口/文档窗口标题
        let main_titles = [
            "WPS Office",
            "WPS",
            "关于给予杨浩同学通报批评的决定2026.09.pdf - WPS Office",
            "关于给予杨浩同学通报批评的决定2026.09.pdf - WPS PDF",
            "新建文本文档.docx - WPS Office",
            "新建文本文档.docx - wps office",
            "工作簿1.xlsx - WPS 表格",
            "演示1.pptx - WPS 演示",
        ];

        for title in main_titles {
            assert!(
                is_wps_main_window(dummy_hwnd, "Qt5QWindowIcon", title),
                "Should identify as WPS main window: {}",
                title
            );
        }

        // 2. 应当被排除的经典主文档窗口类名
        let doc_classes = ["OpusApp", "XLMAIN", "PP9FrameClass", "KxPdfMainWindow"];
        for cls in doc_classes {
            assert!(
                is_wps_main_window(dummy_hwnd, cls, "未命名"),
                "Should identify doc class as main window: {}",
                cls
            );
        }

        // 3. 应当放行的真正另存为/打开对话框标题与空自绘标题
        let dialog_titles = ["另存为", "打开", "Save As", "Open", ""];

        for title in dialog_titles {
            assert!(
                !is_wps_main_window(dummy_hwnd, "Qt5QWindowIcon", title),
                "Should NOT identify dialog as main window: '{}'",
                title
            );
        }
    }
}

#[cfg(test)]
mod debug_log_tests {
    use super::*;

    #[test]
    fn test_replace_ascii_case_insensitive_keeps_bytes() {
        assert_eq!(
            replace_ascii_case_insensitive(r"C:\Users\BOB\x", r"c:\users\bob", "<p>"),
            r"<p>\x"
        );
        assert_eq!(replace_ascii_case_insensitive("aBaB", "ab", "-"), "--");
        assert_eq!(replace_ascii_case_insensitive("abc", "", "X"), "abc");
        assert_eq!(replace_ascii_case_insensitive("abc", "zz", "X"), "abc");
    }

    #[test]
    fn test_sanitize_masks_profile_and_username() {
        let msg = r"UIA_INJECT_START: target='C:\Users\Bob\Docs' user=Bob";
        let masked = sanitize_with(msg, Some(r"C:\Users\Bob"), Some("Bob"));
        assert!(
            !masked.contains(r"C:\Users\Bob"),
            "个人目录不得原样出现: {masked}"
        );
        assert!(!masked.contains("Bob"), "用户名不得原样出现: {masked}");
        assert!(
            masked.contains("%USERPROFILE%"),
            "必须带上目录占位符: {masked}"
        );
        assert!(masked.contains("<user>"), "必须带上用户名占位符: {masked}");
    }

    /// 轮换：当前文件改名为 `.1` 并覆盖旧备份 → 磁盘占用有上限
    #[test]
    fn test_rotate_log_keeps_single_backup_and_caps_disk_use() {
        let dir = std::env::temp_dir().join(format!("quickpath_log_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join(DEBUG_LOG_FILE_NAME);
        std::fs::write(&path, b"current-content").expect("写入当前日志");
        std::fs::write(log_backup_path(&path), b"stale-backup").expect("写入旧备份");

        rotate_log(&path);

        assert!(!path.exists(), "轮换后当前文件必须已移走");
        assert_eq!(
            std::fs::read(log_backup_path(&path)).expect("备份必须存在"),
            b"current-content",
            "备份必须是刚轮换下来的内容（旧备份被覆盖）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 日志脱敏扩展（UNC / 非系统盘符 / 标题内嵌路径）----

    /// UNC：主机名与共享名替换为占位符，保留后续相对目录便于诊断。
    #[test]
    fn test_sanitize_masks_unc_host_and_share() {
        let out = sanitize_full_with(
            r"打开 \\fileserver\客户资料\2026\计划.docx 失败",
            None,
            None,
            Some("C:"),
        );
        assert!(!out.contains("fileserver"), "UNC 主机名不得原样出现: {out}");
        assert!(!out.contains("客户资料"), "UNC 共享名不得原样出现: {out}");
        assert!(
            out.contains(r"\\<unc>\<share>\2026\计划.docx"),
            "应保留相对结构: {out}"
        );
    }

    /// 长路径 `\\?\UNC\...`：保留前缀，主机/共享名同样替换。
    #[test]
    fn test_sanitize_masks_long_path_unc() {
        let out = sanitize_full_with(
            r"\\?\UNC\fileserver\share\dir\a.txt",
            None,
            None,
            Some("C:"),
        );
        assert_eq!(out, r"\\?\UNC\<unc>\<share>\dir\a.txt");
    }

    /// 非系统盘符（含小写与正斜杠）：盘符替换为占位符。
    #[test]
    fn test_sanitize_masks_non_system_drive() {
        assert_eq!(
            sanitize_full_with(r"D:\work\客户资料\plan.docx", None, None, Some("C:")),
            r"%DRIVE_D%:\work\客户资料\plan.docx"
        );
        assert_eq!(
            sanitize_full_with(r"e:/data/x.txt", None, None, Some("C:")),
            r"%DRIVE_E%:/data/x.txt"
        );
    }

    /// 系统盘上的非用户目录保留（`C:\Windows\...` 是诊断关键信息）。
    #[test]
    fn test_sanitize_preserves_system_drive_non_user_dirs() {
        assert_eq!(
            sanitize_full_with(
                r"C:\Windows\System32\drivers\etc\hosts",
                None,
                None,
                Some("C:")
            ),
            r"C:\Windows\System32\drivers\etc\hosts"
        );
        // 系统盘不可知时按 C: 处理，同样保留。
        assert_eq!(
            sanitize_full_with(r"C:\Windows\win.ini", None, None, None),
            r"C:\Windows\win.ini"
        );
    }

    /// 窗口标题内嵌路径必须被覆盖（脱敏作用于整个消息字符串）。
    #[test]
    fn test_sanitize_masks_path_embedded_in_window_title() {
        let out = sanitize_full_with(r"D:\secret\plan.docx - WPS Office", None, None, Some("C:"));
        assert_eq!(out, r"%DRIVE_D%:\secret\plan.docx - WPS Office");
        assert!(!out.contains(r"D:\secret"), "标题内路径不得原样出现: {out}");
    }

    /// 既有行为不回归：`USERPROFILE` → `%USERPROFILE%`，用户名 → `<user>`，且优先于路径处理。
    #[test]
    fn test_sanitize_keeps_profile_and_username_precedence() {
        let out = sanitize_full_with(
            r"target='C:\Users\Bob\Docs' user=Bob unc=\\srv\share\x drive=D:\x",
            Some(r"C:\Users\Bob"),
            Some("Bob"),
            Some("C:"),
        );
        assert!(out.contains("%USERPROFILE%"), "必须带目录占位符: {out}");
        assert!(out.contains("<user>"), "必须带用户名占位符: {out}");
        assert!(!out.contains("Bob"), "用户名不得原样出现: {out}");
        assert!(
            !out.contains(r"C:\Users\Bob"),
            "个人目录不得原样出现: {out}"
        );
        assert!(!out.contains("srv"), "UNC 主机不得原样出现: {out}");
        assert!(out.contains(r"%DRIVE_D%:\x"), "非系统盘应带占位符: {out}");
    }

    /// 大小写混合：盘符、UNC、个人目录的匹配均不区分大小写。
    #[test]
    fn test_sanitize_is_case_insensitive() {
        assert_eq!(
            sanitize_full_with(
                r"c:\users\BOB\file.txt",
                Some(r"C:\Users\Bob"),
                Some("Bob"),
                Some("C:")
            ),
            r"%USERPROFILE%\file.txt"
        );
        assert_eq!(
            sanitize_full_with(r"\\FileServer\Share\dir", None, None, Some("C:")),
            r"\\<unc>\<share>\dir"
        );
        assert_eq!(
            sanitize_full_with(r"d:\tmp\x", None, None, Some("C:")),
            r"%DRIVE_D%:\tmp\x"
        );
    }

    /// 幂等：对已脱敏文本再跑一次不产生新变化。
    #[test]
    fn test_sanitize_is_idempotent() {
        let msg = r"a \\fileserver\share\p  b D:\dir\f.txt  c C:\Windows\x  d C:\Users\Bob\y";
        let once = sanitize_full_with(msg, Some(r"C:\Users\Bob"), Some("Bob"), Some("C:"));
        let twice = sanitize_full_with(&once, Some(r"C:\Users\Bob"), Some("Bob"), Some("C:"));
        assert_eq!(once, twice, "脱敏必须幂等: {once} vs {twice}");
    }

    /// 不含路径的普通文本零改动（含大小写混合的普通词）。
    #[test]
    fn test_sanitize_leaves_plain_text_untouched() {
        let msg = "DETECT_START: hwnd=0x1234 pid=42 title='Save As' user=Bob";
        assert_eq!(
            sanitize_full_with(msg, Some(r"C:\Users\Bob"), Some("Bob"), Some("C:")),
            "DETECT_START: hwnd=0x1234 pid=42 title='Save As' user=<user>"
        );
    }

    /// `remove_log_files`：当前文件与 `.1` 备份都被删除。
    #[test]
    fn test_remove_log_files_deletes_current_and_backup() {
        let dir = std::env::temp_dir().join(format!("quickpath_clear_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join(DEBUG_LOG_FILE_NAME);
        std::fs::write(&path, b"current").expect("写入当前日志");
        std::fs::write(log_backup_path(&path), b"backup").expect("写入备份");

        let failures = remove_log_files(&path);

        assert!(failures.is_empty(), "正常删除不应失败: {failures:?}");
        assert!(!path.exists(), "当前日志必须被删除");
        assert!(!log_backup_path(&path).exists(), "备份必须被删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `remove_log_files`：文件本就不存在视为成功（幂等）；无法删除时如实上报失败且不 panic。
    #[test]
    fn test_remove_log_files_is_idempotent_and_reports_failures() {
        let dir =
            std::env::temp_dir().join(format!("quickpath_clear_absent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join(DEBUG_LOG_FILE_NAME);

        assert!(remove_log_files(&path).is_empty(), "缺失文件应视为成功");

        // 用同名目录占位模拟“无法删除”，必须上报失败而不是 panic。
        std::fs::create_dir_all(&path).expect("创建同名目录");
        assert!(
            !remove_log_files(&path).is_empty(),
            "无法删除时必须上报失败"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
