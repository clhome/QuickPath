#![allow(dead_code)]

use crate::rules::{AppConfig, I18n, Language};
use crate::win32::autostart::set_autostart;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetStockObject, InvalidateRect, SelectObject, SetBkMode, SetTextColor, UpdateWindow, DT_LEFT,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC,
    HGDIOBJ, PAINTSTRUCT, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics,
    LoadCursorW, RegisterClassW, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    IDC_ARROW, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SW_HIDE, SW_SHOW,
    WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_PAINT, WNDCLASSW, WS_CAPTION,
    WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_SYSMENU,
};

static SETTINGS_WINDOW_HWND: AtomicPtr<core::ffi::c_void> =
    AtomicPtr::new(std::ptr::null_mut());

pub struct SettingsWindow {
    hwnd: HWND,
}

impl SettingsWindow {
    pub fn new() -> Result<Self, String> {
        let class_name = w!("QuickPath_Settings_Class");
        unsafe {
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(settings_wnd_proc),
                hInstance: HINSTANCE::default(),
                hCursor: cursor,
                lpszClassName: class_name,
                hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let hwnd = match CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
                class_name,
                w!("QuickPath 设置中心 - Settings"),
                WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
                0,
                0,
                640,
                600,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => return Err("创建设置中心窗口失败".to_string()),
            };

            // 获取当前显示器 DPI 并缩放尺寸
            let dpi = GetDpiForWindow(hwnd);
            let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };
            let win_w = (660.0 * scale).round() as i32;
            let win_h = (600.0 * scale).round() as i32;

            // 启用 Win11 圆角与深色模式
            let round_pref = DWMWCP_ROUND.0 as u32;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );

            let dark_mode = 1u32;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_mode as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );

            let backdrop_type = 2u32; // Mica
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop_type as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );

            // 屏幕居中显示
            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);
            let x = (screen_w - win_w) / 2;
            let y = (screen_h - win_h) / 2;
            let _ = SetWindowPos(hwnd, None, x, y, win_w, win_h, SWP_NOACTIVATE);

            SETTINGS_WINDOW_HWND.store(hwnd.0 as *mut _, Ordering::SeqCst);

            Ok(Self { hwnd })
        }
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

unsafe extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                render_settings_ui(hwnd, hdc);

                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_settings_click(hwnd, y);
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn render_settings_ui(hwnd: HWND, hdc: HDC) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };

        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);

        // Fluent 深色背景 #1a1a1a
        let bg_color = COLORREF(0x001a1a1a);
        let bg_brush = CreateSolidBrush(bg_color);
        FillRect(hdc, &rect, bg_brush);
        let _ = DeleteObject(HGDIOBJ(bg_brush.0 as _));

        SetBkMode(hdc, TRANSPARENT);

        let font_h1 = CreateFontW(
            (-20.0 * scale).round() as i32, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_card_title = CreateFontW(
            (-15.0 * scale).round() as i32, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_text = CreateFontW(
            (-13.0 * scale).round() as i32, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );

        let config = AppConfig::load();
        let locale = I18n::resolve_locale(config.language);

        // 1. 顶部标题
        let pad_x = (28.0 * scale).round() as i32;
        SelectObject(hdc, HGDIOBJ(font_h1.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let title_str = I18n::settings_title(config.language);
        let mut title_buf: Vec<u16> = title_str.encode_utf16().collect();
        let mut title_rect = RECT {
            left: pad_x,
            top: (20.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: (54.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let mut card_top = (66.0 * scale).round() as i32;
        let card_h = (72.0 * scale).round() as i32;
        let card_gap = (10.0 * scale).round() as i32;

        // 卡片 1：自动秒切 (AutoSwitch)
        let (card1_t, card1_d, card1_s) = match locale {
            Language::ZhCN => (
                "自动秒切路径 (AutoSwitch)",
                "文件对话框打开时，瞬时同步至当前或最后活跃的文件夹路径",
                if config.auto_switch_enabled { "[已开启 - 点击切换]" } else { "[已关闭 - 点击切换]" },
            ),
            _ => (
                "Auto-Switch Path (AutoSwitch)",
                "Instantly navigate to the active/last opened folder when dialog opens",
                if config.auto_switch_enabled { "[Enabled - Click]" } else { "[Disabled - Click]" },
            ),
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, card1_t, card1_d, card1_s, config.auto_switch_enabled,
        );

        card_top += card_h + card_gap;

        // 卡片 2：开机自启动
        let (card2_t, card2_d, card2_s) = match locale {
            Language::ZhCN => (
                "开机静默自启 (免 UAC 提权)",
                "通过 Windows 任务计划程序免除管理员 UAC 提示，登录即用",
                if config.autostart_enabled { "[已开启 - 点击切换]" } else { "[已关闭 - 点击切换]" },
            ),
            _ => (
                "Start on Boot (Silent UAC-free)",
                "Launch automatically on system login via Windows Task Scheduler",
                if config.autostart_enabled { "[Enabled - Click]" } else { "[Disabled - Click]" },
            ),
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, card2_t, card2_d, card2_s, config.autostart_enabled,
        );

        card_top += card_h + card_gap;

        // 卡片 3：弹出层半透明度调节
        let opacity_str = format!("{}%", config.floating_bar_opacity);
        let (card3_t, card3_d, card3_s) = match locale {
            Language::ZhCN => (
                "弹出层半透明效果 (Opacity)",
                "调整吸附条的半透明通透程度，点击可在 88% -> 100% -> 60% -> 75% 循环切换",
                format!("[当前: {} - 点击切换]", opacity_str),
            ),
            _ => (
                "Floating Bar Opacity",
                "Adjust transparency level: 88% -> 100% -> 60% -> 75%",
                format!("[Current: {} - Click]", opacity_str),
            ),
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, card3_t, card3_d, &card3_s, true,
        );

        card_top += card_h + card_gap;

        // 卡片 4：多国语言 (Language)
        let lang_name = match config.language {
            Language::Auto => match locale {
                Language::ZhCN => "自动跟随系统 (中文)",
                _ => "Auto (System)",
            },
            Language::ZhCN => "简体中文 (zh-CN)",
            Language::EnUS => "English (en-US)",
        };
        let (card4_t, card4_d, card4_s) = match locale {
            Language::ZhCN => (
                "界面语言 (Language)",
                "支持简体中文、英文，或跟随系统语言自动匹配，点击切换",
                format!("[{}]", lang_name),
            ),
            _ => (
                "Language (i18n)",
                "Supports English, Simplified Chinese, or Auto. Click to toggle",
                format!("[{}]", lang_name),
            ),
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, card4_t, card4_d, &card4_s, true,
        );

        card_top += card_h + card_gap;

        // 卡片 5：全生态管理器支持
        let (card5_t, card5_d, card5_s) = match locale {
            Language::ZhCN => (
                "文件管理器生态适配",
                "支持 Windows 11 多标签资源管理器、Directory Opus、Total Commander、XYplorer",
                "[全部适配就绪]",
            ),
            _ => (
                "Supported File Managers",
                "Native Win11 Tabs Explorer, Directory Opus, Total Commander, XYplorer",
                "[All Ready]",
            ),
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, card5_t, card5_d, card5_s, true,
        );

        // 底部版本标识
        SelectObject(hdc, HGDIOBJ(font_text.0 as _));
        SetTextColor(hdc, COLORREF(0x00777777));
        let ver_str = match locale {
            Language::ZhCN => "QuickPath v1.0.0 · 专为 Windows 10/11 深度打造 · 原生极速",
            _ => "QuickPath v1.0.0 · Native Ultra-Fast for Windows 10/11",
        };
        let mut ver_buf: Vec<u16> = ver_str.encode_utf16().collect();
        let mut ver_rect = RECT {
            left: pad_x,
            top: rect.bottom - (32.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: rect.bottom - (8.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut ver_buf, &mut ver_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let _ = DeleteObject(HGDIOBJ(font_h1.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_card_title.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_text.0 as _));
    }
}

unsafe fn render_card(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f32,
    font_title: windows::Win32::Graphics::Gdi::HFONT,
    font_desc: windows::Win32::Graphics::Gdi::HFONT,
    title: &str,
    desc: &str,
    status_text: &str,
    is_active: bool,
) {
    unsafe {
        let card_rect = RECT { left, top, right, bottom };
        let card_brush = CreateSolidBrush(COLORREF(0x00262626));
        FillRect(hdc, &card_rect, card_brush);
        let _ = DeleteObject(HGDIOBJ(card_brush.0 as _));

        // 标题
        let inner_x = left + (16.0 * scale).round() as i32;
        let right_pad = (200.0 * scale).round() as i32;

        SelectObject(hdc, HGDIOBJ(font_title.0 as _));
        SetTextColor(hdc, COLORREF(0x00f0f0f0));
        let mut title_buf: Vec<u16> = title.encode_utf16().collect();
        let mut title_rect = RECT {
            left: inner_x,
            top: top + (10.0 * scale).round() as i32,
            right: right - right_pad,
            bottom: top + (32.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 描述
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00999999));
        let mut desc_buf: Vec<u16> = desc.encode_utf16().collect();
        let mut desc_rect = RECT {
            left: inner_x,
            top: top + (34.0 * scale).round() as i32,
            right: right - right_pad,
            bottom: bottom - (8.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut desc_buf, &mut desc_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 状态按键
        let status_color = if is_active {
            COLORREF(0x0050d268) // 现代绿色
        } else {
            COLORREF(0x006666cc) // 浅灰红
        };
        SetTextColor(hdc, status_color);
        let mut status_buf: Vec<u16> = status_text.encode_utf16().collect();
        let mut status_rect = RECT {
            left: right - right_pad,
            top: top + (10.0 * scale).round() as i32,
            right: right - (16.0 * scale).round() as i32,
            bottom: bottom - (10.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut status_buf, &mut status_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
    }
}

fn handle_settings_click(hwnd: HWND, y: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };

    let mut config = AppConfig::load();
    let mut need_save = false;

    let card_top_1 = (66.0 * scale).round() as i32;
    let card_h = (72.0 * scale).round() as i32;
    let card_gap = (10.0 * scale).round() as i32;

    let card_top_2 = card_top_1 + card_h + card_gap;
    let card_top_3 = card_top_2 + card_h + card_gap;
    let card_top_4 = card_top_3 + card_h + card_gap;

    // 点击卡片 1: 自动秒切
    if y >= card_top_1 && y <= card_top_1 + card_h {
        config.auto_switch_enabled = !config.auto_switch_enabled;
        need_save = true;
    }
    // 点击卡片 2: 开机自启
    else if y >= card_top_2 && y <= card_top_2 + card_h {
        config.autostart_enabled = !config.autostart_enabled;
        let _ = set_autostart(config.autostart_enabled, config.autostart_task_scheduler);
        need_save = true;
    }
    // 点击卡片 3: 透明度循环调节 (88% -> 100% -> 60% -> 75% -> 88%)
    else if y >= card_top_3 && y <= card_top_3 + card_h {
        config.floating_bar_opacity = match config.floating_bar_opacity {
            88 => 100,
            100 => 60,
            60 => 75,
            _ => 88,
        };
        need_save = true;
    }
    // 点击卡片 4: 语言切换 (Auto -> ZhCN -> EnUS -> Auto)
    else if y >= card_top_4 && y <= card_top_4 + card_h {
        config.language = match config.language {
            Language::Auto => Language::ZhCN,
            Language::ZhCN => Language::EnUS,
            Language::EnUS => Language::Auto,
        };
        need_save = true;
    }

    if need_save {
        let _ = config.save();
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
    }
}
