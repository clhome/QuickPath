#![allow(dead_code)]

use crate::rules::{AppConfig, I18n, Language};
use crate::win32::autostart::set_autostart;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Mutex;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect,
    SelectObject, SetBkMode, SetTextColor, UpdateWindow, DT_CENTER, DT_LEFT, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC, HGDIOBJ,
    PAINTSTRUCT, SRCCOPY, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics,
    LoadCursorW, RegisterClassW, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    IDC_ARROW, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SW_HIDE, SW_SHOW,
    WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WNDCLASSW, WS_CAPTION, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_SYSMENU,
};

static SETTINGS_WINDOW_HWND: AtomicPtr<core::ffi::c_void> =
    AtomicPtr::new(std::ptr::null_mut());
static IS_DRAGGING_SLIDER: AtomicBool = AtomicBool::new(false);
static DRAFT_CONFIG: Mutex<Option<AppConfig>> = Mutex::new(None);

fn get_draft_config() -> AppConfig {
    if let Ok(draft) = DRAFT_CONFIG.lock() {
        if let Some(cfg) = &*draft {
            return cfg.clone();
        }
    }
    crate::get_global_config()
}

fn set_draft_config(cfg: AppConfig) {
    if let Ok(mut draft) = DRAFT_CONFIG.lock() {
        *draft = Some(cfg);
    }
}

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
                640,
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
            let win_h = (640.0 * scale).round() as i32;

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
        // 每次打开设置中心，同步最新生效配置至草稿
        let current = crate::get_global_config();
        set_draft_config(current);

        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
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

                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);

                // 双缓冲绘制，避免滑块拖拽时闪烁
                let mem_dc = CreateCompatibleDC(Some(hdc));
                let mem_bmp = CreateCompatibleBitmap(hdc, rect.right, rect.bottom);
                let old_bmp = SelectObject(mem_dc, HGDIOBJ(mem_bmp.0 as _));

                render_settings_ui(hwnd, mem_dc);

                let _ = BitBlt(hdc, 0, 0, rect.right, rect.bottom, Some(mem_dc), 0, 0, SRCCOPY);

                let _ = SelectObject(mem_dc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(mem_bmp.0 as _));
                let _ = DeleteDC(mem_dc);

                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_settings_mouse_down(hwnd, x, y);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if IS_DRAGGING_SLIDER.load(Ordering::SeqCst) {
                let x = (lparam.0 & 0xffff) as i16 as i32;
                handle_slider_move(hwnd, x);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if IS_DRAGGING_SLIDER.swap(false, Ordering::SeqCst) {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 点击右上角 X，等同于取消：丢弃未保存修改并隐藏窗口
            let current = crate::get_global_config();
            set_draft_config(current);
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
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

        let config = get_draft_config();
        let locale = I18n::resolve_locale(config.language);

        // 1. 顶部大标题
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

        // 卡片 3：弹出层半透明度调节（滑块控制 Slider）
        let (card3_t, card3_d) = match locale {
            Language::ZhCN => (
                "弹出层半透明效果 (Opacity)",
                "按住滑块左右滑动或直接点击槽位，实时调整通透程度",
            ),
            _ => (
                "Floating Bar Opacity",
                "Drag the slider or click the track to adjust transparency level",
            ),
        };
        render_card_with_slider(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, card3_t, card3_d, config.floating_bar_opacity,
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

        // 按钮操作区：「确定」与「取消」按钮
        card_top += card_h + (16.0 * scale).round() as i32;
        let btn_h = (32.0 * scale).round() as i32;
        let btn_w = (92.0 * scale).round() as i32;
        let btn_gap = (12.0 * scale).round() as i32;

        let btn_cancel_right = rect.right - pad_x;
        let btn_cancel_left = btn_cancel_right - btn_w;
        let btn_ok_right = btn_cancel_left - btn_gap;
        let btn_ok_left = btn_ok_right - btn_w;

        let btn_ok_rect = RECT {
            left: btn_ok_left,
            top: card_top,
            right: btn_ok_right,
            bottom: card_top + btn_h,
        };
        let btn_cancel_rect = RECT {
            left: btn_cancel_left,
            top: card_top,
            right: btn_cancel_right,
            bottom: card_top + btn_h,
        };

        // 确定按钮：Win11 Accent Blue #0078d4
        render_button(
            hdc,
            &btn_ok_rect,
            I18n::btn_ok(config.language),
            font_card_title,
            COLORREF(0x00d47800),
            COLORREF(0x00ffffff),
            None,
        );

        // 取消按钮：次要深灰底，细边框，灰白字
        render_button(
            hdc,
            &btn_cancel_rect,
            I18n::btn_cancel(config.language),
            font_card_title,
            COLORREF(0x002f2f2f),
            COLORREF(0x00e0e0e0),
            Some(COLORREF(0x00444444)),
        );

        // 底部左侧：版本标识
        SelectObject(hdc, HGDIOBJ(font_text.0 as _));
        SetTextColor(hdc, COLORREF(0x00777777));
        let ver_str = match locale {
            Language::ZhCN => "QuickPath v1.0.0 · 专为 Windows 10/11 深度打造 · 原生极速",
            _ => "QuickPath v1.0.0 · Native Ultra-Fast for Windows 10/11",
        };
        let mut ver_buf: Vec<u16> = ver_str.encode_utf16().collect();
        let mut ver_rect = RECT {
            left: pad_x,
            top: rect.bottom - (26.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: rect.bottom - (6.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut ver_buf, &mut ver_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 底部右侧：出品方信息（跟随当前草稿语言实时切换）
        SetTextColor(hdc, COLORREF(0x00888888));
        let producer_str = I18n::producer(config.language);
        let mut producer_buf: Vec<u16> = producer_str.encode_utf16().collect();
        let mut producer_rect = RECT {
            left: pad_x,
            top: rect.bottom - (26.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: rect.bottom - (6.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut producer_buf, &mut producer_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);

        let _ = DeleteObject(HGDIOBJ(font_h1.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_card_title.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_text.0 as _));
    }
}

/// 辅助渲染 Fluent 风格圆润按钮
unsafe fn render_button(
    hdc: HDC,
    rect: &RECT,
    text: &str,
    font: windows::Win32::Graphics::Gdi::HFONT,
    bg_color: COLORREF,
    text_color: COLORREF,
    border_color: Option<COLORREF>,
) {
    unsafe {
        if let Some(bc) = border_color {
            let border_brush = CreateSolidBrush(bc);
            FillRect(hdc, rect, border_brush);
            let _ = DeleteObject(HGDIOBJ(border_brush.0 as _));

            let inner_rect = RECT {
                left: rect.left + 1,
                top: rect.top + 1,
                right: rect.right - 1,
                bottom: rect.bottom - 1,
            };
            let bg_brush = CreateSolidBrush(bg_color);
            FillRect(hdc, &inner_rect, bg_brush);
            let _ = DeleteObject(HGDIOBJ(bg_brush.0 as _));
        } else {
            let bg_brush = CreateSolidBrush(bg_color);
            FillRect(hdc, rect, bg_brush);
            let _ = DeleteObject(HGDIOBJ(bg_brush.0 as _));
        }

        SelectObject(hdc, HGDIOBJ(font.0 as _));
        SetTextColor(hdc, text_color);
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let mut r = *rect;
        DrawTextW(hdc, &mut buf, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
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
            COLORREF(0x00777777)
        };
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
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

/// 专门渲染带滑块（Slider）的卡片
unsafe fn render_card_with_slider(
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
    opacity_pct: u8,
) {
    unsafe {
        let card_rect = RECT { left, top, right, bottom };
        let card_brush = CreateSolidBrush(COLORREF(0x00262626));
        FillRect(hdc, &card_rect, card_brush);
        let _ = DeleteObject(HGDIOBJ(card_brush.0 as _));

        // 标题与描述
        let inner_x = left + (16.0 * scale).round() as i32;
        let right_pad = (220.0 * scale).round() as i32;

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

        // 右侧滑块组件（Slider Track & Thumb）
        let slider_w = (140.0 * scale).round() as i32;
        let track_right = right - (18.0 * scale).round() as i32;
        let track_left = track_right - slider_w;
        let track_cy = top + (48.0 * scale).round() as i32;
        let track_h = (4.0 * scale).round() as i32;

        // 1. 滑块数值文字（在滑块槽上方展示，例如 88%）
        let val_text = format!("{}%", opacity_pct);
        let mut val_buf: Vec<u16> = val_text.encode_utf16().collect();
        let mut val_rect = RECT {
            left: track_left,
            top: top + (12.0 * scale).round() as i32,
            right: track_right,
            bottom: top + (32.0 * scale).round() as i32,
        };
        SetTextColor(hdc, COLORREF(0x0050d268)); // 鲜明绿色数值
        DrawTextW(hdc, &mut val_buf, &mut val_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);

        // 2. 灰色背景底槽
        let track_rect = RECT {
            left: track_left,
            top: track_cy - track_h / 2,
            right: track_right,
            bottom: track_cy + track_h / 2,
        };
        let bg_track_brush = CreateSolidBrush(COLORREF(0x00444444));
        FillRect(hdc, &track_rect, bg_track_brush);
        let _ = DeleteObject(HGDIOBJ(bg_track_brush.0 as _));

        // 3. 已激活高亮槽（Win11 强调蓝）
        let ratio = ((opacity_pct.clamp(40, 100) as f32 - 40.0) / 60.0).clamp(0.0, 1.0);
        let thumb_x = track_left + (ratio * slider_w as f32).round() as i32;

        let active_track_rect = RECT {
            left: track_left,
            top: track_cy - track_h / 2,
            right: thumb_x,
            bottom: track_cy + track_h / 2,
        };
        let active_brush = CreateSolidBrush(COLORREF(0x00d47800)); // Win11 Accent Blue
        FillRect(hdc, &active_track_rect, active_brush);
        let _ = DeleteObject(HGDIOBJ(active_brush.0 as _));

        // 4. 滑块圆纽 Thumb
        let thumb_r = (7.0 * scale).round() as i32;
        let thumb_rect = RECT {
            left: thumb_x - thumb_r,
            top: track_cy - thumb_r,
            right: thumb_x + thumb_r,
            bottom: track_cy + thumb_r,
        };
        let thumb_brush = CreateSolidBrush(COLORREF(0x00ffffff));
        FillRect(hdc, &thumb_rect, thumb_brush);
        let _ = DeleteObject(HGDIOBJ(thumb_brush.0 as _));
    }
}

fn handle_settings_mouse_down(hwnd: HWND, x: i32, y: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };

    let mut config = get_draft_config();
    let mut need_redraw = false;

    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let pad_x = (28.0 * scale).round() as i32;

    let card_top_1 = (66.0 * scale).round() as i32;
    let card_h = (72.0 * scale).round() as i32;
    let card_gap = (10.0 * scale).round() as i32;

    let card_top_2 = card_top_1 + card_h + card_gap;
    let card_top_3 = card_top_2 + card_h + card_gap;
    let card_top_4 = card_top_3 + card_h + card_gap;
    let card_top_5 = card_top_4 + card_h + card_gap;

    // 点击卡片 1: 自动秒切
    if y >= card_top_1 && y <= card_top_1 + card_h {
        config.auto_switch_enabled = !config.auto_switch_enabled;
        set_draft_config(config);
        need_redraw = true;
    }
    // 点击卡片 2: 开机自启
    else if y >= card_top_2 && y <= card_top_2 + card_h {
        config.autostart_enabled = !config.autostart_enabled;
        set_draft_config(config);
        need_redraw = true;
    }
    // 点击卡片 3: 透明度滑块（支持直接点击或拖动）
    else if y >= card_top_3 && y <= card_top_3 + card_h {
        let slider_w = (140.0 * scale).round() as i32;
        let track_right = rect.right - pad_x - (18.0 * scale).round() as i32;
        let track_left = track_right - slider_w;

        // 如果点击在滑块区域
        if x >= track_left - (20.0 * scale) as i32 && x <= track_right + (20.0 * scale) as i32 {
            let ratio = ((x - track_left) as f32 / slider_w as f32).clamp(0.0, 1.0);
            let new_val = (40.0 + ratio * 60.0).round() as u8;
            config.floating_bar_opacity = new_val;
            set_draft_config(config);
            need_redraw = true;

            IS_DRAGGING_SLIDER.store(true, Ordering::SeqCst);
            unsafe {
                let _ = SetCapture(hwnd);
            }
        }
    }
    // 点击卡片 4: 语言切换 (Auto -> ZhCN -> EnUS -> Auto)
    else if y >= card_top_4 && y <= card_top_4 + card_h {
        config.language = match config.language {
            Language::Auto => Language::ZhCN,
            Language::ZhCN => Language::EnUS,
            Language::EnUS => Language::Auto,
        };
        set_draft_config(config);
        need_redraw = true;
    }
    else {
        // 按钮区域判断
        let btn_top = card_top_5 + card_h + (16.0 * scale).round() as i32;
        let btn_h = (32.0 * scale).round() as i32;
        let btn_bottom = btn_top + btn_h;
        let btn_w = (92.0 * scale).round() as i32;
        let btn_gap = (12.0 * scale).round() as i32;

        let btn_cancel_right = rect.right - pad_x;
        let btn_cancel_left = btn_cancel_right - btn_w;
        let btn_ok_right = btn_cancel_left - btn_gap;
        let btn_ok_left = btn_ok_right - btn_w;

        if y >= btn_top && y <= btn_bottom {
            // 点击「确定」按钮
            if x >= btn_ok_left && x <= btn_ok_right {
                let old_config = crate::get_global_config();
                // 1. 同步自启动状态
                if config.autostart_enabled != old_config.autostart_enabled
                    || config.autostart_task_scheduler != old_config.autostart_task_scheduler
                {
                    let _ = set_autostart(config.autostart_enabled, config.autostart_task_scheduler);
                }
                // 2. 保存配置到磁盘
                let _ = config.save();
                // 3. 即时热更新运行时全局配置，所有后台监听与弹出层立即生效！
                crate::update_global_config(config);

                // 4. 隐藏窗口
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                return;
            }
            // 点击「取消」按钮
            else if x >= btn_cancel_left && x <= btn_cancel_right {
                // 丢弃未保存修改，重置为生效配置
                set_draft_config(crate::get_global_config());
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                return;
            }
        }
    }

    if need_redraw {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
    }
}

fn handle_slider_move(hwnd: HWND, x: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };

    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let pad_x = (28.0 * scale).round() as i32;

    let slider_w = (140.0 * scale).round() as i32;
    let track_right = rect.right - pad_x - (18.0 * scale).round() as i32;
    let track_left = track_right - slider_w;

    let ratio = ((x - track_left) as f32 / slider_w as f32).clamp(0.0, 1.0);
    let new_val = (40.0 + ratio * 60.0).round() as u8;

    let mut config = get_draft_config();
    if config.floating_bar_opacity != new_val {
        config.floating_bar_opacity = new_val;
        set_draft_config(config);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
    }
}
