use std::sync::atomic::{AtomicPtr, Ordering};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, RegisterClassW,
    SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
    WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{PCWSTR, w};

pub const WM_USER: u32 = 0x0400;
pub const WM_TRAY_CALLBACK: u32 = WM_USER + 201;

// 菜单命令 ID
pub const IDM_TOGGLE_AUTOSWITCH: usize = 1001;
pub const IDM_TOGGLE_AUTOSTART: usize = 1002;
pub const IDM_SHOW_CANDIDATES: usize = 1003;
pub const IDM_OPEN_SETTINGS: usize = 1004;
pub const IDM_EXIT: usize = 1005;
pub const IDM_TOGGLE_MONITOR: usize = 1006;
/// 审计报告 U-01：显式获取第三方管理器当前路径（单次、前台、用户触发）
pub const IDM_PROBE_TOTALCMD: usize = 1007;
pub const IDM_PROBE_XYPLORER: usize = 1008;

/// 托盘消息宿主窗口（供宿主在显式探测结束后弹出结果提示）。
///
/// 只在 `TrayIcon` 存活期间有效；`Drop` 时立即置空，避免向已销毁的窗口发通知。
static TRAY_NOTIFY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// 弹出一条托盘气泡提示（显式探测结局的**可见反馈**，审计报告 D-01A）。
///
/// 标题与正文由调用方传入（已 i18n），本函数不记录也不回显任何剪贴板正文。
/// 托盘图标不存在（已退出）时静默返回。
pub fn notify(title: &str, text: &str) {
    let hwnd = TRAY_NOTIFY_HWND.load(Ordering::SeqCst);
    if hwnd.is_null() {
        return;
    }
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(hwnd),
        uID: 1,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    fill_wide(&mut nid.szInfoTitle, title);
    fill_wide(&mut nid.szInfo, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// 把字符串按 UTF-16 写入定长缓冲区（超出部分截断，始终以 NUL 结尾）。
fn fill_wide(buffer: &mut [u16], text: &str) {
    if buffer.is_empty() {
        return;
    }
    let last = buffer.len() - 1;
    for (slot, unit) in buffer[..last].iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
    }
    buffer[last] = 0;
}

pub struct TrayIcon {
    pub hwnd: HWND,
    nid: NOTIFYICONDATAW,
}

impl TrayIcon {
    pub fn new(notify_target: HWND) -> Result<Self, String> {
        let class_name = w!("QuickPath_Tray_Class");
        unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: HINSTANCE::default(),
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let hwnd = match CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
                class_name,
                w!("QuickPath Tray"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => return Err("创建托盘消息宿主窗口失败".to_string()),
            };
            let hicon = crate::win32::icon::get_app_icon(true);

            let mut tip = [0u16; 128];
            let tip_str = "QuickPath - 现代文件对话框智能跟随\0";
            for (i, c) in tip_str.encode_utf16().enumerate() {
                if i < 127 {
                    tip[i] = c;
                }
            }

            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: notify_target,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: WM_TRAY_CALLBACK,
                hIcon: hicon,
                szTip: tip,
                ..Default::default()
            };

            let added = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
            if !added {
                return Err("添加系统托盘图标失败".to_string());
            }

            TRAY_NOTIFY_HWND.store(notify_target.0, Ordering::SeqCst);
            Ok(Self { hwnd, nid })
        }
    }

    /// 弹出右键菜单（支持多语言）
    ///
    /// `probe_ready` 为 `[Total Commander, XYplorer]` 的可用性：未授权或无法归因到目标
    /// 窗口时对应菜单项置灰，避免用户点了却什么都不发生（审计报告 U-01）。
    pub fn show_context_menu(
        hwnd: HWND,
        auto_switch_enabled: bool,
        autostart_enabled: bool,
        monitor_enabled: bool,
        probe_ready: [bool; 2],
        lang: crate::rules::Language,
        hotkey: &str,
    ) {
        unsafe {
            let hmenu = CreatePopupMenu().unwrap_or_default();
            if hmenu.0.is_null() {
                return;
            }

            let mut cursor_pos = POINT::default();
            let _ = GetCursorPos(&mut cursor_pos);

            let auto_switch_flag = if auto_switch_enabled {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            };
            let autostart_flag = if autostart_enabled {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            };
            let monitor_flag = if monitor_enabled {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            };

            let hotkey_disp = crate::win32::hotkey::format_hotkey_display(hotkey);

            let str_autoswitch: Vec<u16> = format!(
                "⚡ {}",
                crate::rules::I18n::tray_toggle_autoswitch(&lang, false)
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let str_autostart: Vec<u16> = format!(
                "🚀 {}",
                crate::rules::I18n::tray_toggle_autostart(&lang, false)
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let str_monitor: Vec<u16> = format!(
                "📊 {}",
                crate::rules::I18n::tray_toggle_monitor(&lang, false)
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let str_show: Vec<u16> = format!(
                "📂 {}",
                crate::rules::I18n::tray_show_candidates(&lang, &hotkey_disp)
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            // 审计报告 U-01：显式获取第三方管理器当前路径（只授权不探测，点击才探测）
            let str_probe_tc: Vec<u16> = format!("📂 {}", crate::rules::I18n::tray_probe(&lang, 0))
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let str_probe_xy: Vec<u16> = format!("📂 {}", crate::rules::I18n::tray_probe(&lang, 1))
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let str_settings: Vec<u16> = format!("⚙ {}", crate::rules::I18n::tray_settings(&lang))
                .encode_utf16()
                .chain(Some(0))
                .collect();

            let str_exit: Vec<u16> = format!("❌ {}", crate::rules::I18n::tray_exit(&lang))
                .encode_utf16()
                .chain(Some(0))
                .collect();

            let _ = AppendMenuW(
                hmenu,
                MF_STRING | auto_switch_flag,
                IDM_TOGGLE_AUTOSWITCH,
                PCWSTR(str_autoswitch.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING | autostart_flag,
                IDM_TOGGLE_AUTOSTART,
                PCWSTR(str_autostart.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING | monitor_flag,
                IDM_TOGGLE_MONITOR,
                PCWSTR(str_monitor.as_ptr()),
            );
            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(
                hmenu,
                MF_STRING
                    | if probe_ready[0] {
                        MF_UNCHECKED
                    } else {
                        MF_GRAYED
                    },
                IDM_PROBE_TOTALCMD,
                PCWSTR(str_probe_tc.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING
                    | if probe_ready[1] {
                        MF_UNCHECKED
                    } else {
                        MF_GRAYED
                    },
                IDM_PROBE_XYPLORER,
                PCWSTR(str_probe_xy.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING,
                IDM_SHOW_CANDIDATES,
                PCWSTR(str_show.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING,
                IDM_OPEN_SETTINGS,
                PCWSTR(str_settings.as_ptr()),
            );
            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(hmenu, MF_STRING, IDM_EXIT, PCWSTR(str_exit.as_ptr()));

            // 必须在 TrackPopupMenu 之前调用 SetForegroundWindow
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(
                hmenu,
                TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RIGHTBUTTON,
                cursor_pos.x,
                cursor_pos.y,
                Some(0),
                hwnd,
                None,
            );

            let _ = DestroyMenu(hmenu);
        }
    }
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        TRAY_NOTIFY_HWND.store(std::ptr::null_mut(), Ordering::SeqCst);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
            if !self.hwnd.0.is_null() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
