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
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics,
    LoadCursorW, RegisterClassW, SendMessageW, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    ICON_BIG, ICON_SMALL, IDC_ARROW, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SW_HIDE, SW_SHOW,
    WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_SETICON, WM_SYSKEYDOWN, WNDCLASSW, WS_CAPTION, WS_CLIPCHILDREN,
    WS_MINIMIZEBOX, WS_SYSMENU,
};


static SETTINGS_WINDOW_HWND: AtomicPtr<core::ffi::c_void> =
    AtomicPtr::new(std::ptr::null_mut());
static IS_DRAGGING_SLIDER: AtomicBool = AtomicBool::new(false);
static IS_RECORDING_HOTKEY: AtomicBool = AtomicBool::new(false);
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
            let hicon_big = crate::win32::icon::get_app_icon(false);
            let hicon_sm = crate::win32::icon::get_app_icon(true);
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(settings_wnd_proc),
                hInstance: HINSTANCE::default(),
                hIcon: hicon_big,
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
                670,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) if !h.0.is_null() => {
                    let _ = SendMessageW(h, WM_SETICON, Some(WPARAM(ICON_SMALL as _)), Some(LPARAM(hicon_sm.0 as _)));
                    let _ = SendMessageW(h, WM_SETICON, Some(WPARAM(ICON_BIG as _)), Some(LPARAM(hicon_big.0 as _)));
                    h
                }

                _ => return Err("创建设置中心窗口失败".to_string()),
            };


            // 获取当前显示器 DPI 并缩放尺寸
            let dpi = GetDpiForWindow(hwnd);
            let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };
            let win_w = (660.0 * scale).round() as i32;
            let win_h = (670.0 * scale).round() as i32;

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
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);

        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    pub fn hide(&self) {
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
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

                // 双缓冲绘制，避免滑块拖拽与交互时闪烁
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
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if IS_RECORDING_HOTKEY.load(Ordering::SeqCst) {
                let vk = wparam.0 as u32;
                if vk == 0x1B {
                    // ESC 键取消录制
                    IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    return LRESULT(0);
                }

                if let Some(key_name) = crate::win32::hotkey::vk_to_key_name(vk) {
                    let is_ctrl = unsafe { GetKeyState(0x11) < 0 };
                    let is_alt = unsafe { GetKeyState(0x12) < 0 };
                    let is_shift = unsafe { GetKeyState(0x10) < 0 };
                    let is_win = unsafe {
                        GetKeyState(0x5B) < 0 || GetKeyState(0x5C) < 0
                    };

                    let mut combo_parts = Vec::new();
                    if is_ctrl { combo_parts.push("Ctrl"); }
                    if is_alt { combo_parts.push("Alt"); }
                    if is_shift { combo_parts.push("Shift"); }
                    if is_win { combo_parts.push("Win"); }

                    // 如果未按修饰键且不是功能键，默认补充 Ctrl 防止全局按键冲突
                    if combo_parts.is_empty() && !key_name.starts_with('F') {
                        combo_parts.push("Ctrl");
                    }
                    combo_parts.push(key_name);

                    let new_hotkey_str = combo_parts.join("+");
                    let mut config = get_draft_config();
                    config.hotkey = new_hotkey_str;
                    set_draft_config(config);

                    IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        let _ = UpdateWindow(hwnd);
                    }
                    return LRESULT(0);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
            IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
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
            (-19.0 * scale).round() as i32, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_card_title = CreateFontW(
            (-14.0 * scale).round() as i32, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_text = CreateFontW(
            (-12.0 * scale).round() as i32, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );

        let config = get_draft_config();
        let bundle = I18n::get_bundle(&config.language);

        // 1. 顶部大标题
        let pad_x = (28.0 * scale).round() as i32;
        SelectObject(hdc, HGDIOBJ(font_h1.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let title_str = &bundle.settings.title;
        let mut title_buf: Vec<u16> = title_str.encode_utf16().collect();
        let mut title_rect = RECT {
            left: pad_x,
            top: (16.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: (48.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let mut card_top = (54.0 * scale).round() as i32;
        let card_h = (58.0 * scale).round() as i32;
        let card_gap = (8.0 * scale).round() as i32;

        // 卡片 1：自动秒切 (AutoSwitch)
        let card1_s = if config.auto_switch_enabled {
            &bundle.settings.status_enabled
        } else {
            &bundle.settings.status_disabled
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, &bundle.settings.auto_switch, &bundle.settings.auto_switch_desc, card1_s,
            if config.auto_switch_enabled { COLORREF(0x0050d268) } else { COLORREF(0x00777777) },
        );

        card_top += card_h + card_gap;

        // 卡片 2：唤出候选目录快捷键 (Hotkey) - 支持录制修改
        let is_recording = IS_RECORDING_HOTKEY.load(Ordering::SeqCst);
        let hotkey_disp = crate::win32::hotkey::format_hotkey_display(&config.hotkey);
        let card2_s = if is_recording {
            bundle.hotkey.recording_prompt.clone()
        } else {
            format!("[ {} · {} ]", hotkey_disp, bundle.hotkey.click_to_record)
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, &bundle.hotkey.card_title, &bundle.hotkey.card_desc, &card2_s,
            if is_recording { COLORREF(0x0000a5ff) } else { COLORREF(0x0050d268) },
        );

        card_top += card_h + card_gap;

        // 卡片 3：开机自启动
        let card3_s = if config.autostart_enabled {
            &bundle.settings.status_enabled
        } else {
            &bundle.settings.status_disabled
        };
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, &bundle.settings.autostart, &bundle.settings.autostart_desc, card3_s,
            if config.autostart_enabled { COLORREF(0x0050d268) } else { COLORREF(0x00777777) },
        );

        card_top += card_h + card_gap;

        // 卡片 4：弹出层半透明度调节（滑块控制 Slider）
        render_card_with_slider(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, &bundle.settings.opacity, &bundle.settings.opacity_desc, config.floating_bar_opacity,
        );

        card_top += card_h + card_gap;

        // 卡片 5：多国语言 (Language)
        let lang_name = match &config.language {
            Language::Auto => &bundle.settings.lang_auto,
            _ => &bundle.meta.name,
        };
        let card5_s = format!("[{}]", lang_name);
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, &bundle.settings.language, &bundle.settings.language_desc, &card5_s,
            COLORREF(0x0050d268),
        );

        card_top += card_h + card_gap;

        // 卡片 6：全生态管理器支持
        render_card(
            hdc, pad_x, card_top, rect.right - pad_x, card_top + card_h, scale,
            font_card_title, font_text, &bundle.settings.ecosystem, &bundle.settings.ecosystem_desc, &bundle.settings.status_all_ready,
            COLORREF(0x0050d268),
        );

        // 按钮操作区：「确定」与「取消」按钮
        card_top += card_h + (14.0 * scale).round() as i32;
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
            &bundle.settings.btn_ok,
            font_card_title,
            COLORREF(0x00d47800),
            COLORREF(0x00ffffff),
            None,
        );

        // 取消按钮：次要深灰底，细边框，灰白字
        render_button(
            hdc,
            &btn_cancel_rect,
            &bundle.settings.btn_cancel,
            font_card_title,
            COLORREF(0x002f2f2f),
            COLORREF(0x00e0e0e0),
            Some(COLORREF(0x00444444)),
        );

        // 底部左侧：版本标识
        SelectObject(hdc, HGDIOBJ(font_text.0 as _));
        SetTextColor(hdc, COLORREF(0x00777777));
        let ver_str = &bundle.settings.version_info;
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
        let producer_str = &bundle.settings.producer;
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
    status_color: COLORREF,
) {
    unsafe {
        let card_rect = RECT { left, top, right, bottom };
        let card_brush = CreateSolidBrush(COLORREF(0x00262626));
        FillRect(hdc, &card_rect, card_brush);
        let _ = DeleteObject(HGDIOBJ(card_brush.0 as _));

        // 标题
        let inner_x = left + (16.0 * scale).round() as i32;
        let right_pad = (210.0 * scale).round() as i32;

        SelectObject(hdc, HGDIOBJ(font_title.0 as _));
        SetTextColor(hdc, COLORREF(0x00f0f0f0));
        let mut title_buf: Vec<u16> = title.encode_utf16().collect();
        let mut title_rect = RECT {
            left: inner_x,
            top: top + (8.0 * scale).round() as i32,
            right: right - right_pad,
            bottom: top + (28.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 描述
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00999999));
        let mut desc_buf: Vec<u16> = desc.encode_utf16().collect();
        let mut desc_rect = RECT {
            left: inner_x,
            top: top + (30.0 * scale).round() as i32,
            right: right - right_pad,
            bottom: bottom - (6.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut desc_buf, &mut desc_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 状态按键
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, status_color);
        let mut status_buf: Vec<u16> = status_text.encode_utf16().collect();
        let mut status_rect = RECT {
            left: right - right_pad,
            top: top + (8.0 * scale).round() as i32,
            right: right - (16.0 * scale).round() as i32,
            bottom: bottom - (8.0 * scale).round() as i32,
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
            top: top + (8.0 * scale).round() as i32,
            right: right - right_pad,
            bottom: top + (28.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00999999));
        let mut desc_buf: Vec<u16> = desc.encode_utf16().collect();
        let mut desc_rect = RECT {
            left: inner_x,
            top: top + (30.0 * scale).round() as i32,
            right: right - right_pad,
            bottom: bottom - (6.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut desc_buf, &mut desc_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 右侧滑块组件（Slider Track & Thumb）
        let slider_w = (140.0 * scale).round() as i32;
        let track_right = right - (18.0 * scale).round() as i32;
        let track_left = track_right - slider_w;
        let track_cy = top + (40.0 * scale).round() as i32;
        let track_h = (4.0 * scale).round() as i32;

        // 1. 滑块数值文字（在滑块槽上方展示，例如 88%）
        let val_text = format!("{}%", opacity_pct);
        let mut val_buf: Vec<u16> = val_text.encode_utf16().collect();
        let mut val_rect = RECT {
            left: track_left,
            top: top + (8.0 * scale).round() as i32,
            right: track_right,
            bottom: top + (26.0 * scale).round() as i32,
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
        let thumb_r = (6.0 * scale).round() as i32;
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

    let card_top_1 = (54.0 * scale).round() as i32;
    let card_h = (58.0 * scale).round() as i32;
    let card_gap = (8.0 * scale).round() as i32;

    let card_top_2 = card_top_1 + card_h + card_gap; // 快捷键卡片
    let card_top_3 = card_top_2 + card_h + card_gap; // 开机自启
    let card_top_4 = card_top_3 + card_h + card_gap; // 透明度
    let card_top_5 = card_top_4 + card_h + card_gap; // 语言
    let card_top_6 = card_top_5 + card_h + card_gap; // 生态

    // 点击卡片 1: 自动秒切
    if y >= card_top_1 && y <= card_top_1 + card_h {
        config.auto_switch_enabled = !config.auto_switch_enabled;
        set_draft_config(config);
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        need_redraw = true;
    }
    // 点击卡片 2: 唤出快捷键（进入/退出按键录制模式）
    else if y >= card_top_2 && y <= card_top_2 + card_h {
        let is_rec = IS_RECORDING_HOTKEY.load(Ordering::SeqCst);
        IS_RECORDING_HOTKEY.store(!is_rec, Ordering::SeqCst);
        need_redraw = true;
    }
    // 点击卡片 3: 开机自启
    else if y >= card_top_3 && y <= card_top_3 + card_h {
        config.autostart_enabled = !config.autostart_enabled;
        set_draft_config(config);
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        need_redraw = true;
    }
    // 点击卡片 4: 透明度滑块（支持直接点击或拖动）
    else if y >= card_top_4 && y <= card_top_4 + card_h {
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
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
    // 点击卡片 5: 语言切换 (动态按可用语言列表轮转)
    else if y >= card_top_5 && y <= card_top_5 + card_h {
        config.language = config.language.next();
        set_draft_config(config);
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        need_redraw = true;
    }

    else {
        // 按钮区域判断
        let btn_top = card_top_6 + card_h + (14.0 * scale).round() as i32;
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
                // 3. 即时热更新运行时全局配置（包括快捷键重新注册）
                crate::update_global_config(config);
                IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);

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
                IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
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
