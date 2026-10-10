#![allow(dead_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetSystemMetrics, GetWindow, GetWindowLongW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    IsZoomed, GWL_STYLE, GW_OWNER, SM_CXSCREEN, SM_CYSCREEN, WS_POPUP,
};

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

pub fn log_debug(msg: &str) {
    if !is_debug_mode_enabled() {
        return;
    }
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() % 86400;
    let hours = (secs / 3600 + 8) % 24; // UTC+8
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    let ms = dur.subsec_millis();
    let line = format!("[{:02}:{:02}:{:02}.{:03}] {}\n", hours, mins, s, ms, msg);

    // 1. 优先写入 exe 同级目录
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let log_file = exe_dir.join("quickpath_debug.log");
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
                .and_then(|mut f| f.write_all(line.as_bytes()));
        }
    }

    // 2. 写入系统临时目录 %TEMP%（确保无写入权限限制）
    let temp_log = std::env::temp_dir().join("quickpath_debug.log");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(temp_log)
        .and_then(|mut f| f.write_all(line.as_bytes()));
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
                    && width >= 400
                    && width <= 1600
                    && height >= 250
                    && height <= 1100
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
                hwnd.0 as usize, class_name, window_title, width, height, is_kcfd, is_qt5_dialog, is_32770
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

/// 探测标准 Win32 (#32770) 文件对话框
fn detect_standard_dialog(raw_hwnd: HWND, pid: u32, process_name: String) -> Option<FileDialogInfo> {
    unsafe {
        let hwnd = find_dialog_root(raw_hwnd);

        if !IsWindowVisible(hwnd).as_bool() {
            log_debug(&format!("detect_std REJECT: hwnd=0x{:X} not visible", hwnd.0 as usize));
            return None;
        }

        let class_name = get_window_class_name(hwnd);
        let window_title = get_window_title(hwnd);

        // 大多数标准与通用文件对话框的类名均为 #32770
        let is_32770 = class_name == "#32770";
        let is_dialog_title = is_file_dialog_title(&window_title);

        if !is_32770 && !is_dialog_title {
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

        // 标准 Win32 对话框判定条件：
        // 1. 具有输入控件 (ComboBoxEx32 / ComboBox / 可见 Edit)
        let has_input = ctx.has_file_name_combo || !ctx.edit_hwnds.is_empty();

        // 2. 具有列表或工具栏导航
        let has_navigation_or_list = ctx.has_list_view || ctx.has_toolbar_or_breadcrumb;

        // 3. 判定准则：
        // 准则 A：若标题明确为“另存为/打开/Save As/Open”，且窗口类名为 #32770：
        // 只要具备输入控件 (has_input) 或具备浏览导航列表 (has_navigation_or_list)，即认定为文件对话框
        // 准则 B：若具备文件列表且具备输入控件 (has_list_view && has_input)，无条件认定为标准文件对话框
        let is_standard_dialog = (ctx.has_list_view && has_input)
            || (is_dialog_title && (has_navigation_or_list || has_input));

        if !is_standard_dialog {
            log_debug(&format!(
                "detect_std REJECT: hwnd=0x{:X} list={} combo={} bar={} edits={} title='{}'",
                hwnd.0 as usize, ctx.has_list_view, ctx.has_file_name_combo, ctx.has_toolbar_or_breadcrumb, ctx.edit_hwnds.len(), window_title
            ));
            return None;
        }

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let file_name_edit = ctx.edit_hwnds.first().copied();

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
            raw_hwnd.0 as usize, pid, process_name, class_name, title, root.0 as usize, root_class, root_title
        ));

        // 1. 若为 WPS 进程，优先尝试识别 WPS 自绘文件对话框
        if is_wps_process_name(&process_name) {
            if let Some(wps_info) = detect_wps_dialog(raw_hwnd, pid, process_name.clone()) {
                log_debug(&format!("DETECT_HIT_WPS: hwnd=0x{:X} title='{}' rect={:?}", wps_info.hwnd.0 as usize, wps_info.window_title, wps_info.rect));
                return Some(wps_info);
            }
        }

        // 2. 标准 Win32 对话框识别
        if let Some(std_info) = detect_standard_dialog(raw_hwnd, pid, process_name.clone()) {
            log_debug(&format!("DETECT_HIT_STD: hwnd=0x{:X} title='{}' rect={:?}", std_info.hwnd.0 as usize, std_info.window_title, std_info.rect));
            Some(std_info)
        } else {
            None
        }
    }
}

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
        } else if class.eq_ignore_ascii_case("Edit") {
            if IsWindowVisible(child).as_bool() {
                ctx.edit_hwnds.push(child);
            }
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
        let titles_hit = ["另存为", "另存为...", "打开", "打开文件", "Save As", "Save as file", "Open", "Open File"];
        let titles_miss = ["属性", "关于 Notepad3", "确认删除", "设置", "Settings", "Preferences"];

        for t in titles_hit {
            assert!(is_file_dialog_title(t), "Should match dialog title: {}", t);
        }

        for t in titles_miss {
            assert!(!is_file_dialog_title(t), "Should NOT match dialog title: {}", t);
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
        let dialog_titles = [
            "另存为",
            "打开",
            "Save As",
            "Open",
            "",
        ];

        for title in dialog_titles {
            assert!(
                !is_wps_main_window(dummy_hwnd, "Qt5QWindowIcon", title),
                "Should NOT identify dialog as main window: '{}'",
                title
            );
        }
    }
}
