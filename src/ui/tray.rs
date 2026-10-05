use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, RegisterClassW, SetForegroundWindow,
    TrackPopupMenu, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

pub const WM_USER: u32 = 0x0400;
pub const WM_TRAY_CALLBACK: u32 = WM_USER + 201;

// 菜单命令 ID
pub const IDM_TOGGLE_AUTOSWITCH: usize = 1001;
pub const IDM_TOGGLE_AUTOSTART: usize = 1002;
pub const IDM_SHOW_CANDIDATES: usize = 1003;
pub const IDM_OPEN_SETTINGS: usize = 1004;
pub const IDM_EXIT: usize = 1005;

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

            Ok(Self { hwnd, nid })
        }
    }

    /// 弹出右键菜单（支持多语言）
    pub fn show_context_menu(
        hwnd: HWND,
        auto_switch_enabled: bool,
        autostart_enabled: bool,
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

            let hotkey_disp = crate::win32::hotkey::format_hotkey_display(hotkey);

            let str_autoswitch: Vec<u16> = format!("⚡ {}", crate::rules::I18n::tray_toggle_autoswitch(&lang, false))
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let str_autostart: Vec<u16> = format!("🚀 {}", crate::rules::I18n::tray_toggle_autostart(&lang, false))
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let str_show: Vec<u16> = format!("📂 {}", crate::rules::I18n::tray_show_candidates(&lang, &hotkey_disp))
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
            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
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
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
            if !self.hwnd.0.is_null() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
