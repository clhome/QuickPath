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
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject,
    InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor, UpdateWindow, DT_CENTER,
    DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC,
    HGDIOBJ, PAINTSTRUCT, PS_NULL, PS_SOLID, SRCCOPY, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DrawIconEx, GetClientRect, GetSystemMetrics, LoadCursorW,
    RegisterClassW, SendMessageW, SetCursor, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    DI_NORMAL, ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_HAND, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETICON, WM_SYSKEYDOWN, WNDCLASSW,
    WS_CAPTION, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_SYSMENU,
};

static SETTINGS_WINDOW_HWND: AtomicPtr<core::ffi::c_void> =
    AtomicPtr::new(std::ptr::null_mut());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    None,
    OpacitySlider,
    AutoSwitchToggle { start_x: i32, initial_state: bool, has_moved: bool },
    AutostartToggle { start_x: i32, initial_state: bool, has_moved: bool },
}

static DRAG_MODE: Mutex<DragMode> = Mutex::new(DragMode::None);
static IS_RECORDING_HOTKEY: AtomicBool = AtomicBool::new(false);
static HOVER_CARD_IDX: Mutex<Option<usize>> = Mutex::new(None);
static DRAFT_CONFIG: Mutex<Option<AppConfig>> = Mutex::new(None);
static IS_DROPDOWN_OPEN: AtomicBool = AtomicBool::new(false);
static HOVER_DROPDOWN_ITEM_IDX: Mutex<Option<usize>> = Mutex::new(None);

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
                660,
                720,
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

            // 获取当前显示器 DPI 并自适应缩放尺寸
            let dpi = GetDpiForWindow(hwnd);
            let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };
            let win_w = (660.0 * scale).round() as i32;
            let win_h = (720.0 * scale).round() as i32;

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
        let current = crate::get_global_config();
        set_draft_config(current);
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
        if let Ok(mut drag) = DRAG_MODE.lock() {
            *drag = DragMode::None;
        }

        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    pub fn hide(&self) {
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
        if let Ok(mut drag) = DRAG_MODE.lock() {
            *drag = DragMode::None;
        }
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
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_settings_mouse_move(hwnd, x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_settings_mouse_up(hwnd, x, y);
            LRESULT(0)
        }
        WM_CLOSE => {
            let current = crate::get_global_config();
            set_draft_config(current);
            IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
            IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
            if let Ok(mut drag) = DRAG_MODE.lock() {
                *drag = DragMode::None;
            }
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
            IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
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

        // Fluent 深邃暗夜背景 #1c1c1c
        let bg_brush = CreateSolidBrush(COLORREF(0x001c1c1c));
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
            (-13.5 * scale).round() as i32, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_text = CreateFontW(
            (-11.5 * scale).round() as i32, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );
        let font_small = CreateFontW(
            (-10.5 * scale).round() as i32, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );

        let config = get_draft_config();
        let bundle = I18n::get_bundle(&config.language);
        let pad_x = (28.0 * scale).round() as i32;

        // 1. 顶部 Header 品牌区（主标题 + 副标题 + 雅致分割线）
        let header_text_x = pad_x;
        SelectObject(hdc, HGDIOBJ(font_h1.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let title_str = &bundle.settings.title;
        let mut title_buf: Vec<u16> = title_str.encode_utf16().collect();
        let mut title_rect = RECT {
            left: header_text_x,
            top: (14.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: (36.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        SelectObject(hdc, HGDIOBJ(font_text.0 as _));
        SetTextColor(hdc, COLORREF(0x008e8e8e));
        let sub_str = if bundle.settings.subtitle.is_empty() {
            "系统集成与智能文件对话框跳转偏好"
        } else {
            &bundle.settings.subtitle
        };
        let mut sub_buf: Vec<u16> = sub_str.encode_utf16().collect();
        let mut sub_rect = RECT {
            left: header_text_x,
            top: (37.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: (54.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut sub_buf, &mut sub_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 细微分割线
        let line_y = (60.0 * scale).round() as i32;
        let line_rect = RECT {
            left: pad_x,
            top: line_y,
            right: rect.right - pad_x,
            bottom: line_y + 1,
        };
        let line_brush = CreateSolidBrush(COLORREF(0x002e2e2e));
        FillRect(hdc, &line_rect, line_brush);
        let _ = DeleteObject(HGDIOBJ(line_brush.0 as _));

        // 2. 卡片区域几何计算
        let card_w = rect.right - pad_x * 2;
        let card_h = (56.0 * scale).round() as i32;
        let card_gap = (8.0 * scale).round() as i32;
        let card_top_base = (70.0 * scale).round() as i32;

        let hovered = HOVER_CARD_IDX.lock().ok().and_then(|h| *h);

        // 卡片 0：自动秒切路径 (AutoSwitch) - Toggle 开关滑块
        let card0_top = card_top_base;
        let is_hov_0 = hovered == Some(0);
        render_toggle_card(
            hdc, pad_x, card0_top, pad_x + card_w, card0_top + card_h, scale,
            font_card_title, font_text,
            &bundle.settings.auto_switch, &bundle.settings.auto_switch_desc,
            config.auto_switch_enabled,
            &bundle.settings.status_enabled, &bundle.settings.status_disabled,
            is_hov_0,
        );

        // 卡片 1：唤出快捷键 (Hotkey) - 录制 Badge
        let card1_top = card0_top + card_h + card_gap;
        let is_hov_1 = hovered == Some(1);
        let is_recording = IS_RECORDING_HOTKEY.load(Ordering::SeqCst);
        let hotkey_disp = crate::win32::hotkey::format_hotkey_display(&config.hotkey);
        let card1_s = if is_recording {
            bundle.hotkey.recording_prompt.clone()
        } else {
            format!("[ {} · {} ]", hotkey_disp, bundle.hotkey.click_to_record)
        };
        render_hotkey_card(
            hdc, pad_x, card1_top, pad_x + card_w, card1_top + card_h, scale,
            font_card_title, font_text,
            &bundle.hotkey.card_title, &bundle.hotkey.card_desc,
            &card1_s, is_recording, is_hov_1,
        );

        // 卡片 2：开机静默自启 (Autostart) - Toggle 开关滑块
        let card2_top = card1_top + card_h + card_gap;
        let is_hov_2 = hovered == Some(2);
        render_toggle_card(
            hdc, pad_x, card2_top, pad_x + card_w, card2_top + card_h, scale,
            font_card_title, font_text,
            &bundle.settings.autostart, &bundle.settings.autostart_desc,
            config.autostart_enabled,
            &bundle.settings.status_enabled, &bundle.settings.status_disabled,
            is_hov_2,
        );

        // 卡片 3：弹出层半透明效果 (Opacity) - 调节 Slider 滑块
        let card3_top = card2_top + card_h + card_gap;
        let is_hov_3 = hovered == Some(3);
        render_slider_card(
            hdc, pad_x, card3_top, pad_x + card_w, card3_top + card_h, scale,
            font_card_title, font_text,
            &bundle.settings.opacity, &bundle.settings.opacity_desc,
            config.floating_bar_opacity, is_hov_3,
        );

        // 卡片 4：界面语言 (Language) - 现代 Dropdown 下拉选择框
        let card4_top = card3_top + card_h + card_gap;
        let is_hov_4 = hovered == Some(4);
        let is_dropdown_open = IS_DROPDOWN_OPEN.load(Ordering::SeqCst);
        let lang_name = match &config.language {
            Language::Auto => &bundle.settings.lang_auto,
            _ => &bundle.meta.name,
        };
        render_dropdown_card(
            hdc, pad_x, card4_top, pad_x + card_w, card4_top + card_h, scale,
            font_card_title, font_text,
            &bundle.settings.language, &bundle.settings.language_desc,
            lang_name, is_hov_4, is_dropdown_open,
        );

        // 卡片 5：“关于 QuickPath”现代专属信息卡片 (About Card)
        let card5_top = card4_top + card_h + card_gap + (2.0 * scale).round() as i32;
        let about_h = (98.0 * scale).round() as i32;
        render_about_card(
            hdc, pad_x, card5_top, pad_x + card_w, card5_top + about_h, scale,
            &bundle, font_card_title, font_text, font_small,
        );

        // 3. 底部「确定」与「取消」按钮
        let btn_top = card5_top + about_h + (16.0 * scale).round() as i32;
        let btn_h = (34.0 * scale).round() as i32;
        let btn_w = (98.0 * scale).round() as i32;
        let btn_gap = (12.0 * scale).round() as i32;

        let btn_cancel_right = rect.right - pad_x;
        let btn_cancel_left = btn_cancel_right - btn_w;
        let btn_ok_right = btn_cancel_left - btn_gap;
        let btn_ok_left = btn_ok_right - btn_w;

        let btn_ok_rect = RECT {
            left: btn_ok_left,
            top: btn_top,
            right: btn_ok_right,
            bottom: btn_top + btn_h,
        };
        let btn_cancel_rect = RECT {
            left: btn_cancel_left,
            top: btn_top,
            right: btn_cancel_right,
            bottom: btn_top + btn_h,
        };

        let is_hov_ok = hovered == Some(10);
        let is_hov_cancel = hovered == Some(11);

        // 确定按钮：Win11 Accent Blue（深色高亮微渐变）
        let ok_bg = if is_hov_ok { COLORREF(0x00e88410) } else { COLORREF(0x00d47800) };
        render_rounded_button(
            hdc, &btn_ok_rect, &bundle.settings.btn_ok, font_card_title,
            ok_bg, COLORREF(0x00ffffff), None, (4.0 * scale).round() as i32,
        );

        // 取消按钮：精致深灰面板，带微边框
        let cancel_bg = if is_hov_cancel { COLORREF(0x00383838) } else { COLORREF(0x002c2c2c) };
        let cancel_border = if is_hov_cancel { COLORREF(0x005c5c5c) } else { COLORREF(0x00444444) };
        render_rounded_button(
            hdc, &btn_cancel_rect, &bundle.settings.btn_cancel, font_card_title,
            cancel_bg, COLORREF(0x00e2e2e2), Some(cancel_border), (4.0 * scale).round() as i32,
        );

        // 4. 底部微型标语与出品方
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x006e6e6e));
        let ver_str = &bundle.settings.version_info;
        let mut ver_buf: Vec<u16> = ver_str.encode_utf16().collect();
        let mut ver_rect = RECT {
            left: pad_x,
            top: rect.bottom - (24.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: rect.bottom - (6.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut ver_buf, &mut ver_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 5. 顶层叠加渲染网页风格下拉选择选项层（展开时覆盖在关于卡片上方）
        if is_dropdown_open {
            let combo_w = (170.0 * scale).round() as i32;
            let combo_h = (30.0 * scale).round() as i32;
            let right_pad = (16.0 * scale).round() as i32;
            let combo_right = pad_x + card_w - right_pad;
            let combo_left = combo_right - combo_w;
            let combo_cy = card4_top + card_h / 2;
            let combo_bottom = combo_cy + combo_h / 2;

            let hov_item = HOVER_DROPDOWN_ITEM_IDX.lock().ok().and_then(|h| *h);
            render_web_dropdown_layer(
                hdc, combo_left, combo_bottom, combo_w, scale,
                config.language.as_code(), font_text, hov_item,
            );
        }

        let _ = DeleteObject(HGDIOBJ(font_h1.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_card_title.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_text.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_small.0 as _));
    }
}

/// 辅助绘制带圆角和描边的精致 Fluent 面板
unsafe fn draw_fluent_box(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    radius: i32,
    bg_color: COLORREF,
    border_color: COLORREF,
) {
    unsafe {
        let brush = CreateSolidBrush(bg_color);
        let pen = CreatePen(PS_SOLID, 1, border_color);
        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0 as _));
        let old_pen = SelectObject(hdc, HGDIOBJ(pen.0 as _));

        let _ = RoundRect(hdc, left, top, right, bottom, radius, radius);

        let _ = SelectObject(hdc, old_brush);
        let _ = SelectObject(hdc, old_pen);
        let _ = DeleteObject(HGDIOBJ(brush.0 as _));
        let _ = DeleteObject(HGDIOBJ(pen.0 as _));
    }
}

/// 辅助渲染圆角按钮
unsafe fn render_rounded_button(
    hdc: HDC,
    rect: &RECT,
    text: &str,
    font: windows::Win32::Graphics::Gdi::HFONT,
    bg_color: COLORREF,
    text_color: COLORREF,
    border_color: Option<COLORREF>,
    radius: i32,
) {
    unsafe {
        let bc = border_color.unwrap_or(bg_color);
        draw_fluent_box(hdc, rect.left, rect.top, rect.right, rect.bottom, radius, bg_color, bc);

        SelectObject(hdc, HGDIOBJ(font.0 as _));
        SetTextColor(hdc, text_color);
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let mut r = *rect;
        DrawTextW(hdc, &mut buf, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 绘制卡片基础文本（标题与描述）
unsafe fn render_card_texts(
    hdc: HDC,
    left: i32,
    top: i32,
    right_limit: i32,
    bottom: i32,
    scale: f32,
    font_title: windows::Win32::Graphics::Gdi::HFONT,
    font_desc: windows::Win32::Graphics::Gdi::HFONT,
    title: &str,
    desc: &str,
) {
    unsafe {
        let inner_x = left + (16.0 * scale).round() as i32;

        SelectObject(hdc, HGDIOBJ(font_title.0 as _));
        SetTextColor(hdc, COLORREF(0x00f3f3f3));
        let mut title_buf: Vec<u16> = title.encode_utf16().collect();
        let mut title_rect = RECT {
            left: inner_x,
            top: top + (8.0 * scale).round() as i32,
            right: right_limit,
            bottom: top + (27.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00909090));
        let mut desc_buf: Vec<u16> = desc.encode_utf16().collect();
        let mut desc_rect = RECT {
            left: inner_x,
            top: top + (29.0 * scale).round() as i32,
            right: right_limit,
            bottom: bottom - (6.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut desc_buf, &mut desc_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 渲染带现代 Toggle 开关滑块的卡片
unsafe fn render_toggle_card(
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
    is_on: bool,
    status_on: &str,
    status_off: &str,
    is_hovered: bool,
) {
    unsafe {
        let (bg, border) = if is_hovered {
            (COLORREF(0x002b2b2b), COLORREF(0x004c4c4c))
        } else {
            (COLORREF(0x00242424), COLORREF(0x00353535))
        };
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, bg, border);

        let sw_w = (46.0 * scale).round() as i32;
        let sw_h = (24.0 * scale).round() as i32;
        let right_pad = (18.0 * scale).round() as i32;
        let sw_right = right - right_pad;
        let sw_left = sw_right - sw_w;
        let sw_cy = top + (bottom - top) / 2;
        let sw_top = sw_cy - sw_h / 2;
        let sw_bottom = sw_top + sw_h;

        let right_limit = sw_left - (80.0 * scale).round() as i32;
        render_card_texts(hdc, left, top, right_limit, bottom, scale, font_title, font_desc, title, desc);

        // 状态文字提示（开关左侧）
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        let (txt, txt_color) = if is_on {
            (status_on, COLORREF(0x0050d268))
        } else {
            (status_off, COLORREF(0x00787878))
        };
        SetTextColor(hdc, txt_color);
        let mut status_buf: Vec<u16> = txt.encode_utf16().collect();
        let mut status_rect = RECT {
            left: sw_left - (70.0 * scale).round() as i32,
            top: top,
            right: sw_left - (10.0 * scale).round() as i32,
            bottom: bottom,
        };
        DrawTextW(hdc, &mut status_buf, &mut status_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);

        // 绘制 Toggle Switch 胶囊滑块槽体
        let (sw_bg, sw_border) = if is_on {
            (COLORREF(0x00d47800), COLORREF(0x00d47800)) // Accent Blue
        } else {
            (COLORREF(0x00323232), COLORREF(0x00525252)) // 灰底
        };
        draw_fluent_box(hdc, sw_left, sw_top, sw_right, sw_bottom, sw_h, sw_bg, sw_border);

        // 绘制 Toggle 滑块圆纽 Thumb（纯白圆）
        let thumb_margin = (3.0 * scale).round() as i32;
        let thumb_radius = (sw_h - thumb_margin * 2) / 2;
        let thumb_cx = if is_on {
            sw_right - thumb_margin - thumb_radius
        } else {
            sw_left + thumb_margin + thumb_radius
        };

        let thumb_brush = CreateSolidBrush(COLORREF(0x00ffffff));
        let thumb_pen = CreatePen(PS_NULL, 0, COLORREF(0));
        let old_b = SelectObject(hdc, HGDIOBJ(thumb_brush.0 as _));
        let old_p = SelectObject(hdc, HGDIOBJ(thumb_pen.0 as _));
        let _ = RoundRect(
            hdc,
            thumb_cx - thumb_radius,
            sw_cy - thumb_radius,
            thumb_cx + thumb_radius + 1,
            sw_cy + thumb_radius + 1,
            thumb_radius * 2,
            thumb_radius * 2,
        );
        let _ = SelectObject(hdc, old_b);
        let _ = SelectObject(hdc, old_p);
        let _ = DeleteObject(HGDIOBJ(thumb_brush.0 as _));
        let _ = DeleteObject(HGDIOBJ(thumb_pen.0 as _));
    }
}

/// 渲染快捷键录制卡片
unsafe fn render_hotkey_card(
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
    badge_text: &str,
    is_recording: bool,
    is_hovered: bool,
) {
    unsafe {
        let (bg, border) = if is_hovered {
            (COLORREF(0x002b2b2b), COLORREF(0x004c4c4c))
        } else {
            (COLORREF(0x00242424), COLORREF(0x00353535))
        };
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, bg, border);

        let badge_w = (180.0 * scale).round() as i32;
        let right_limit = right - badge_w - (16.0 * scale).round() as i32;
        render_card_texts(hdc, left, top, right_limit, bottom, scale, font_title, font_desc, title, desc);

        // 右侧按键 Badge
        let badge_h = (28.0 * scale).round() as i32;
        let badge_cy = top + (bottom - top) / 2;
        let badge_rect = RECT {
            left: right - badge_w - (16.0 * scale).round() as i32,
            top: badge_cy - badge_h / 2,
            right: right - (16.0 * scale).round() as i32,
            bottom: badge_cy + badge_h / 2,
        };

        let (badge_bg, badge_border, text_color) = if is_recording {
            (COLORREF(0x001f3045), COLORREF(0x0000a5ff), COLORREF(0x0000a5ff)) // 录制橙/亮蓝
        } else {
            (COLORREF(0x002c2c2c), COLORREF(0x00d47800), COLORREF(0x0050d268)) // 正常常态
        };

        draw_fluent_box(hdc, badge_rect.left, badge_rect.top, badge_rect.right, badge_rect.bottom, (4.0 * scale).round() as i32, badge_bg, badge_border);

        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, text_color);
        let mut buf: Vec<u16> = badge_text.encode_utf16().collect();
        let mut r = badge_rect;
        DrawTextW(hdc, &mut buf, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 渲染带百分比数值与滑块的透明度调节卡片
unsafe fn render_slider_card(
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
    is_hovered: bool,
) {
    unsafe {
        let (bg, border) = if is_hovered {
            (COLORREF(0x002b2b2b), COLORREF(0x004c4c4c))
        } else {
            (COLORREF(0x00242424), COLORREF(0x00353535))
        };
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, bg, border);

        let slider_w = (140.0 * scale).round() as i32;
        let right_pad = (18.0 * scale).round() as i32;
        let track_right = right - right_pad;
        let track_left = track_right - slider_w;
        let right_limit = track_left - (20.0 * scale).round() as i32;

        render_card_texts(hdc, left, top, right_limit, bottom, scale, font_title, font_desc, title, desc);

        // 1. 数值显示
        let val_text = format!("{}%", opacity_pct);
        let mut val_buf: Vec<u16> = val_text.encode_utf16().collect();
        let mut val_rect = RECT {
            left: track_left,
            top: top + (6.0 * scale).round() as i32,
            right: track_right,
            bottom: top + (24.0 * scale).round() as i32,
        };
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x0050d268));
        DrawTextW(hdc, &mut val_buf, &mut val_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);

        // 2. 滑槽
        let track_cy = top + (36.0 * scale).round() as i32;
        let track_h = (4.0 * scale).round() as i32;
        let track_rect = RECT {
            left: track_left,
            top: track_cy - track_h / 2,
            right: track_right,
            bottom: track_cy + track_h / 2,
        };
        let bg_track_brush = CreateSolidBrush(COLORREF(0x00404040));
        FillRect(hdc, &track_rect, bg_track_brush);
        let _ = DeleteObject(HGDIOBJ(bg_track_brush.0 as _));

        let ratio = ((opacity_pct.clamp(40, 100) as f32 - 40.0) / 60.0).clamp(0.0, 1.0);
        let thumb_x = track_left + (ratio * slider_w as f32).round() as i32;

        let active_track_rect = RECT {
            left: track_left,
            top: track_cy - track_h / 2,
            right: thumb_x,
            bottom: track_cy + track_h / 2,
        };
        let active_brush = CreateSolidBrush(COLORREF(0x00d47800));
        FillRect(hdc, &active_track_rect, active_brush);
        let _ = DeleteObject(HGDIOBJ(active_brush.0 as _));

        // 3. Thumb
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

/// 渲染带现代 Dropdown 下拉选择框的卡片
unsafe fn render_dropdown_card(
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
    current_lang_name: &str,
    is_hovered: bool,
    is_open: bool,
) {
    unsafe {
        let (bg, border) = if is_hovered {
            (COLORREF(0x002b2b2b), COLORREF(0x004c4c4c))
        } else {
            (COLORREF(0x00242424), COLORREF(0x00353535))
        };
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, bg, border);

        let combo_w = (170.0 * scale).round() as i32;
        let combo_h = (30.0 * scale).round() as i32;
        let right_pad = (16.0 * scale).round() as i32;
        let combo_right = right - right_pad;
        let combo_left = combo_right - combo_w;
        let combo_cy = top + (bottom - top) / 2;
        let combo_top = combo_cy - combo_h / 2;
        let combo_bottom = combo_top + combo_h;

        let right_limit = combo_left - (16.0 * scale).round() as i32;
        render_card_texts(hdc, left, top, right_limit, bottom, scale, font_title, font_desc, title, desc);

        // 下拉框背景与描边：展开时边框呈现 Accent Blue 激活高亮
        let combo_bg = COLORREF(0x002b2b2b);
        let combo_border = if is_open {
            COLORREF(0x00d47800) // 激活亮蓝
        } else if is_hovered {
            COLORREF(0x005c5c5c)
        } else {
            COLORREF(0x00444444)
        };
        draw_fluent_box(hdc, combo_left, combo_top, combo_right, combo_bottom, (4.0 * scale).round() as i32, combo_bg, combo_border);

        // 下拉框文字
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00e6e6e6));
        let mut lang_buf: Vec<u16> = current_lang_name.encode_utf16().collect();
        let mut lang_rect = RECT {
            left: combo_left + (10.0 * scale).round() as i32,
            top: combo_top,
            right: combo_right - (22.0 * scale).round() as i32,
            bottom: combo_bottom,
        };
        DrawTextW(hdc, &mut lang_buf, &mut lang_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 下拉小三角：展开时为 ▲，收起时为 ▼
        SetTextColor(hdc, if is_open { COLORREF(0x00d47800) } else { COLORREF(0x00999999) });
        let arrow_char = if is_open { "▲" } else { "▼" };
        let mut arrow_buf: Vec<u16> = arrow_char.encode_utf16().collect();
        let mut arrow_rect = RECT {
            left: combo_right - (22.0 * scale).round() as i32,
            top: combo_top,
            right: combo_right - (8.0 * scale).round() as i32,
            bottom: combo_bottom,
        };
        DrawTextW(hdc, &mut arrow_buf, &mut arrow_rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 渲染网页风格的下拉浮动面板（紧贴在下拉框下方）
unsafe fn render_web_dropdown_layer(
    hdc: HDC,
    combo_left: i32,
    combo_bottom: i32,
    combo_w: i32,
    scale: f32,
    current_lang_code: &str,
    font: windows::Win32::Graphics::Gdi::HFONT,
    hovered_item: Option<usize>,
) {
    unsafe {
        let avail = I18n::get_available_languages();
        let mut lang_options = vec![("auto".to_string(), "自动跟随系统 (Auto)".to_string())];
        for (code, name) in avail {
            if !lang_options.iter().any(|(c, _)| c == &code) {
                lang_options.push((code, name));
            }
        }

        let item_h = (28.0 * scale).round() as i32;
        let list_pad = (4.0 * scale).round() as i32;
        let list_w = combo_w;
        let list_h = item_h * lang_options.len() as i32 + list_pad * 2;
        let list_left = combo_left;
        let list_top = combo_bottom + (3.0 * scale).round() as i32;
        let list_right = list_left + list_w;
        let list_bottom = list_top + list_h;

        // 面板底色与精致外边框（圆角 6px）
        draw_fluent_box(
            hdc,
            list_left,
            list_top,
            list_right,
            list_bottom,
            (6.0 * scale).round() as i32,
            COLORREF(0x00222222),
            COLORREF(0x004c4c4c),
        );

        // 逐项渲染
        for (idx, (code, name)) in lang_options.iter().enumerate() {
            let it_top = list_top + list_pad + idx as i32 * item_h;
            let it_bottom = it_top + item_h;
            let it_left = list_left + list_pad;
            let it_right = list_right - list_pad;

            let is_selected = code == current_lang_code;
            let is_hover = hovered_item == Some(idx);

            if is_hover {
                let hover_bg = COLORREF(0x00333333);
                draw_fluent_box(hdc, it_left, it_top, it_right, it_bottom, (4.0 * scale).round() as i32, hover_bg, hover_bg);
            } else if is_selected {
                let sel_bg = COLORREF(0x002a2a2a);
                draw_fluent_box(hdc, it_left, it_top, it_right, it_bottom, (4.0 * scale).round() as i32, sel_bg, sel_bg);
            }

            SelectObject(hdc, HGDIOBJ(font.0 as _));
            let text_color = if is_selected {
                COLORREF(0x0050d268) // 选中绿色
            } else if is_hover {
                COLORREF(0x00ffffff) // 悬停纯白
            } else {
                COLORREF(0x00d2d2d2) // 正常浅灰
            };
            SetTextColor(hdc, text_color);

            let disp_text = if is_selected {
                format!("✔  {}", name)
            } else {
                format!("    {}", name)
            };
            let mut buf: Vec<u16> = disp_text.encode_utf16().collect();
            let mut r = RECT {
                left: it_left + (8.0 * scale).round() as i32,
                top: it_top,
                right: it_right - (8.0 * scale).round() as i32,
                bottom: it_bottom,
            };
            DrawTextW(hdc, &mut buf, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }
    }
}

/// 渲染“关于 QuickPath”现代商业软件大卡片
unsafe fn render_about_card(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f32,
    bundle: &crate::rules::LocaleBundle,
    font_bold: windows::Win32::Graphics::Gdi::HFONT,
    font_normal: windows::Win32::Graphics::Gdi::HFONT,
    font_small: windows::Win32::Graphics::Gdi::HFONT,
) {
    unsafe {
        // 卡片背景与精致微边框
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, COLORREF(0x00212121), COLORREF(0x00343434));

        // 左侧 Logo 图标：放大显示 logo.png，占满所在卡片高度的 80%
        let card_h = bottom - top;
        let logo_h = ((card_h as f32) * 0.8).round() as i32;
        let logo_w = logo_h; // 正方形原图保持等比例
        let logo_x = left + (16.0 * scale).round() as i32;
        let logo_y = top + (card_h - logo_h) / 2;

        if !crate::win32::icon::draw_logo_png(hdc, logo_x, logo_y, logo_w, logo_h) {
            let hicon = crate::win32::icon::get_app_icon(false);
            let _ = DrawIconEx(hdc, logo_x, logo_y, hicon, logo_w, logo_h, 0, None, DI_NORMAL);
        }

        let text_x = logo_x + logo_w + (18.0 * scale).round() as i32;
        let text_right = right - (16.0 * scale).round() as i32;

        // 行 1: QuickPath + 版本徽标
        SelectObject(hdc, HGDIOBJ(font_bold.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let ver_name = if bundle.about.version.is_empty() {
            "v1.0.0 正式版 (原生极速 · 极简轻量)"
        } else {
            &bundle.about.version
        };
        let line1 = format!("QuickPath  {}", ver_name);
        let mut buf1: Vec<u16> = line1.encode_utf16().collect();
        let mut r1 = RECT {
            left: text_x,
            top: top + (9.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (28.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut buf1, &mut r1, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 行 2: 出品方信息（衢州御风科技有限公司）
        SelectObject(hdc, HGDIOBJ(font_normal.0 as _));
        SetTextColor(hdc, COLORREF(0x00e0e0e0));
        let company_text = if bundle.about.company.is_empty() {
            "出品方：衢州御风科技有限公司"
        } else {
            &bundle.about.company
        };
        let mut buf2: Vec<u16> = company_text.encode_utf16().collect();
        let mut r2 = RECT {
            left: text_x,
            top: top + (31.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (49.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut buf2, &mut r2, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 行 3: 产品研发定位
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x00909090));
        let desc_text = if bundle.about.desc.is_empty() {
            "专注于新一代 Windows 现代生产力工具与原生系统增强研发"
        } else {
            &bundle.about.desc
        };
        let mut buf3: Vec<u16> = desc_text.encode_utf16().collect();
        let mut r3 = RECT {
            left: text_x,
            top: top + (52.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (70.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut buf3, &mut r3, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 行 4: 版权声明
        SetTextColor(hdc, COLORREF(0x00666666));
        let copyright_text = if bundle.about.copyright.is_empty() {
            "Copyright © 2026 衢州御风科技有限公司. All Rights Reserved."
        } else {
            &bundle.about.copyright
        };
        let mut buf4: Vec<u16> = copyright_text.encode_utf16().collect();
        let mut r4 = RECT {
            left: text_x,
            top: top + (72.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (90.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut buf4, &mut r4, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 鼠标悬停位置探测与手型光标切换
fn handle_settings_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };

    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let pad_x = (28.0 * scale).round() as i32;

    let card_w = rect.right - pad_x * 2;
    let card_h = (56.0 * scale).round() as i32;
    let card_gap = (8.0 * scale).round() as i32;
    let card_top_base = (70.0 * scale).round() as i32;

    let card0_top = card_top_base;
    let card1_top = card0_top + card_h + card_gap;
    let card2_top = card1_top + card_h + card_gap;
    let card3_top = card2_top + card_h + card_gap;
    let card4_top = card3_top + card_h + card_gap;
    let card5_top = card4_top + card_h + card_gap + (2.0 * scale).round() as i32;
    let about_h = (98.0 * scale).round() as i32;

    let btn_top = card5_top + about_h + (16.0 * scale).round() as i32;
    let btn_h = (34.0 * scale).round() as i32;
    let btn_w = (98.0 * scale).round() as i32;
    let btn_gap = (12.0 * scale).round() as i32;

    let btn_cancel_right = rect.right - pad_x;
    let btn_cancel_left = btn_cancel_right - btn_w;
    let btn_ok_right = btn_cancel_left - btn_gap;
    let btn_ok_left = btn_ok_right - btn_w;

    // 如果下拉层处于展开状态，优先处理下拉列表项的悬停检测
    if IS_DROPDOWN_OPEN.load(Ordering::SeqCst) {
        let combo_w = (170.0 * scale).round() as i32;
        let combo_h = (30.0 * scale).round() as i32;
        let right_pad = (16.0 * scale).round() as i32;
        let combo_right = pad_x + card_w - right_pad;
        let combo_left = combo_right - combo_w;
        let combo_cy = card4_top + card_h / 2;
        let combo_top = combo_cy - combo_h / 2;
        let combo_bottom = combo_top + combo_h;

        let avail = I18n::get_available_languages();
        let mut lang_options = vec![("auto".to_string(), "自动跟随系统 (Auto)".to_string())];
        for (code, name) in avail {
            if !lang_options.iter().any(|(c, _)| c == &code) {
                lang_options.push((code, name));
            }
        }

        let item_h = (28.0 * scale).round() as i32;
        let list_pad = (4.0 * scale).round() as i32;
        let list_top = combo_bottom + (3.0 * scale).round() as i32;
        let list_h = item_h * lang_options.len() as i32 + list_pad * 2;
        let list_bottom = list_top + list_h;
        let list_left = combo_left;
        let list_right = list_left + combo_w;

        let mut new_hov_item = None;
        if x >= list_left && x <= list_right && y >= list_top && y <= list_bottom {
            let offset_y = y - list_top - list_pad;
            if offset_y >= 0 {
                let idx = (offset_y / item_h) as usize;
                if idx < lang_options.len() {
                    new_hov_item = Some(idx);
                }
            }
        }

        let mut changed = false;
        if let Ok(mut h) = HOVER_DROPDOWN_ITEM_IDX.lock() {
            if *h != new_hov_item {
                *h = new_hov_item;
                changed = true;
            }
        }
        if changed {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }

        let is_over_combo = x >= combo_left && x <= combo_right && y >= combo_top && y <= combo_bottom;
        unsafe {
            let cursor_id = if new_hov_item.is_some() || is_over_combo { IDC_HAND } else { IDC_ARROW };
            let _ = SetCursor(Some(LoadCursorW(None, cursor_id).unwrap_or_default()));
        }
        return;
    }

    // 检查拖拽中模式
    if let Ok(mut drag) = DRAG_MODE.lock() {
        match *drag {
            DragMode::OpacitySlider => {
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
                return;
            }
            DragMode::AutoSwitchToggle { start_x, initial_state, ref mut has_moved } => {
                let dx = x - start_x;
                if dx.abs() > 4 {
                    *has_moved = true;
                }
                let mut config = get_draft_config();
                let new_state = if dx > 8 {
                    true
                } else if dx < -8 {
                    false
                } else {
                    initial_state
                };
                if config.auto_switch_enabled != new_state {
                    config.auto_switch_enabled = new_state;
                    set_draft_config(config);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                return;
            }
            DragMode::AutostartToggle { start_x, initial_state, ref mut has_moved } => {
                let dx = x - start_x;
                if dx.abs() > 4 {
                    *has_moved = true;
                }
                let mut config = get_draft_config();
                let new_state = if dx > 8 {
                    true
                } else if dx < -8 {
                    false
                } else {
                    initial_state
                };
                if config.autostart_enabled != new_state {
                    config.autostart_enabled = new_state;
                    set_draft_config(config);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                return;
            }
            DragMode::None => {}
        }
    }

    // 判断悬停卡片与控件
    let mut new_hover = None;
    let mut is_pointer = false;

    if x >= pad_x && x <= pad_x + card_w {
        if y >= card0_top && y <= card0_top + card_h {
            new_hover = Some(0);
            is_pointer = true;
        } else if y >= card1_top && y <= card1_top + card_h {
            new_hover = Some(1);
            is_pointer = true;
        } else if y >= card2_top && y <= card2_top + card_h {
            new_hover = Some(2);
            is_pointer = true;
        } else if y >= card3_top && y <= card3_top + card_h {
            new_hover = Some(3);
            is_pointer = true;
        } else if y >= card4_top && y <= card4_top + card_h {
            let combo_w = (170.0 * scale).round() as i32;
            let combo_h = (30.0 * scale).round() as i32;
            let right_pad = (16.0 * scale).round() as i32;
            let combo_right = pad_x + card_w - right_pad;
            let combo_left = combo_right - combo_w;
            let combo_cy = card4_top + card_h / 2;
            let combo_top = combo_cy - combo_h / 2;
            let combo_bottom = combo_top + combo_h;

            if x >= combo_left && x <= combo_right && y >= combo_top && y <= combo_bottom {
                new_hover = Some(4);
                is_pointer = true;
            }
        }
    }

    if y >= btn_top && y <= btn_top + btn_h {
        if x >= btn_ok_left && x <= btn_ok_right {
            new_hover = Some(10);
            is_pointer = true;
        } else if x >= btn_cancel_left && x <= btn_cancel_right {
            new_hover = Some(11);
            is_pointer = true;
        }
    }

    // 设置鼠标指针形态
    unsafe {
        let cursor_id = if is_pointer { IDC_HAND } else { IDC_ARROW };
        let _ = SetCursor(Some(LoadCursorW(None, cursor_id).unwrap_or_default()));
    }

    // 悬停发生变化时触发重绘
    if let Ok(mut h) = HOVER_CARD_IDX.lock() {
        if *h != new_hover {
            *h = new_hover;
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    }
}

/// 鼠标松开处理（释放拖拽模式）
fn handle_settings_mouse_up(hwnd: HWND, _x: i32, _y: i32) {
    if let Ok(mut drag) = DRAG_MODE.lock() {
        if *drag != DragMode::None {
            *drag = DragMode::None;
            unsafe {
                let _ = ReleaseCapture();
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    }
}

/// 鼠标点击交互核心引擎
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

    let card_w = rect.right - pad_x * 2;
    let card_h = (56.0 * scale).round() as i32;
    let card_gap = (8.0 * scale).round() as i32;
    let card_top_base = (70.0 * scale).round() as i32;

    let card0_top = card_top_base;
    let card1_top = card0_top + card_h + card_gap;
    let card2_top = card1_top + card_h + card_gap;
    let card3_top = card2_top + card_h + card_gap;
    let card4_top = card3_top + card_h + card_gap;
    let card5_top = card4_top + card_h + card_gap + (2.0 * scale).round() as i32;
    let about_h = (98.0 * scale).round() as i32;

    let is_dropdown_open = IS_DROPDOWN_OPEN.load(Ordering::SeqCst);
    let combo_w = (170.0 * scale).round() as i32;
    let combo_h = (30.0 * scale).round() as i32;
    let right_pad = (16.0 * scale).round() as i32;
    let combo_right = pad_x + card_w - right_pad;
    let combo_left = combo_right - combo_w;
    let combo_cy = card4_top + card_h / 2;
    let combo_top = combo_cy - combo_h / 2;
    let combo_bottom = combo_top + combo_h;

    // 如果下拉选项层处于展开状态，优先处理其点击或外部点击收起
    if is_dropdown_open {
        let avail = I18n::get_available_languages();
        let mut lang_options = vec![("auto".to_string(), "自动跟随系统 (Auto)".to_string())];
        for (code, name) in avail {
            if !lang_options.iter().any(|(c, _)| c == &code) {
                lang_options.push((code, name));
            }
        }

        let item_h = (28.0 * scale).round() as i32;
        let list_pad = (4.0 * scale).round() as i32;
        let list_top = combo_bottom + (3.0 * scale).round() as i32;
        let list_h = item_h * lang_options.len() as i32 + list_pad * 2;
        let list_bottom = list_top + list_h;
        let list_left = combo_left;
        let list_right = list_left + combo_w;

        // 1. 点击在选项列表中
        if x >= list_left && x <= list_right && y >= list_top && y <= list_bottom {
            let offset_y = y - list_top - list_pad;
            if offset_y >= 0 {
                let clicked_idx = (offset_y / item_h) as usize;
                if let Some((code, _)) = lang_options.get(clicked_idx) {
                    config.language = Language::from_code(code);
                    set_draft_config(config);
                }
            }
            IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
                let _ = UpdateWindow(hwnd);
            }
            return;
        }

        // 2. 点击在下拉框本身：切换收起
        if x >= combo_left && x <= combo_right && y >= combo_top && y <= combo_bottom {
            IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
                let _ = UpdateWindow(hwnd);
            }
            return;
        }

        // 3. 点击外部任何区域：自动收起
        IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
        return;
    }

    // 点击卡片 0: 自动秒切路径（支持点击与拖拽左右滑块）
    if x >= pad_x && x <= pad_x + card_w && y >= card0_top && y <= card0_top + card_h {
        let old_state = config.auto_switch_enabled;
        config.auto_switch_enabled = !old_state;
        set_draft_config(config);
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        need_redraw = true;

        if let Ok(mut drag) = DRAG_MODE.lock() {
            *drag = DragMode::AutoSwitchToggle {
                start_x: x,
                initial_state: old_state,
                has_moved: false,
            };
        }
        unsafe {
            let _ = SetCapture(hwnd);
        }
    }
    // 点击卡片 1: 唤出快捷键（开启/取消按键录制）
    else if x >= pad_x && x <= pad_x + card_w && y >= card1_top && y <= card1_top + card_h {
        let is_rec = IS_RECORDING_HOTKEY.load(Ordering::SeqCst);
        IS_RECORDING_HOTKEY.store(!is_rec, Ordering::SeqCst);
        need_redraw = true;
    }
    // 点击卡片 2: 开机静默自启（支持点击与拖拽左右滑块）
    else if x >= pad_x && x <= pad_x + card_w && y >= card2_top && y <= card2_top + card_h {
        let old_state = config.autostart_enabled;
        config.autostart_enabled = !old_state;
        set_draft_config(config);
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        need_redraw = true;

        if let Ok(mut drag) = DRAG_MODE.lock() {
            *drag = DragMode::AutostartToggle {
                start_x: x,
                initial_state: old_state,
                has_moved: false,
            };
        }
        unsafe {
            let _ = SetCapture(hwnd);
        }
    }
    // 点击卡片 3: 透明度调节滑块
    else if x >= pad_x && x <= pad_x + card_w && y >= card3_top && y <= card3_top + card_h {
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        let slider_w = (140.0 * scale).round() as i32;
        let track_right = rect.right - pad_x - (18.0 * scale).round() as i32;
        let track_left = track_right - slider_w;

        if x >= track_left - (20.0 * scale) as i32 && x <= track_right + (20.0 * scale) as i32 {
            let ratio = ((x - track_left) as f32 / slider_w as f32).clamp(0.0, 1.0);
            let new_val = (40.0 + ratio * 60.0).round() as u8;
            config.floating_bar_opacity = new_val;
            set_draft_config(config);
            need_redraw = true;

            if let Ok(mut drag) = DRAG_MODE.lock() {
                *drag = DragMode::OpacitySlider;
            }
            unsafe {
                let _ = SetCapture(hwnd);
            }
        }
    }
    // 点击卡片 4: 界面语言下拉菜单 (Dropdown)
    else if y >= card4_top && y <= card4_top + card_h {
        // 严格限定：只有点击在右侧下拉框区域内才展开！
        if x >= combo_left && x <= combo_right && y >= combo_top && y <= combo_bottom {
            IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
            IS_DROPDOWN_OPEN.store(true, Ordering::SeqCst);
            need_redraw = true;
        }
    }
    else {
        // 按钮区域点击判定
        let btn_top = card5_top + about_h + (16.0 * scale).round() as i32;
        let btn_h = (34.0 * scale).round() as i32;
        let btn_bottom = btn_top + btn_h;
        let btn_w = (98.0 * scale).round() as i32;
        let btn_gap = (12.0 * scale).round() as i32;

        let btn_cancel_right = rect.right - pad_x;
        let btn_cancel_left = btn_cancel_right - btn_w;
        let btn_ok_right = btn_cancel_left - btn_gap;
        let btn_ok_left = btn_ok_right - btn_w;

        if y >= btn_top && y <= btn_bottom {
            if x >= btn_ok_left && x <= btn_ok_right {
                // 点击「确定」：执行保存与全局热同步
                let old_config = crate::get_global_config();
                if config.autostart_enabled != old_config.autostart_enabled
                    || config.autostart_task_scheduler != old_config.autostart_task_scheduler
                {
                    let _ = set_autostart(config.autostart_enabled, config.autostart_task_scheduler);
                }
                let _ = config.save();
                crate::update_global_config(config);
                IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                if let Ok(mut drag) = DRAG_MODE.lock() {
                    *drag = DragMode::None;
                }
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                return;
            } else if x >= btn_cancel_left && x <= btn_cancel_right {
                // 点击「取消」：恢复生效配置
                set_draft_config(crate::get_global_config());
                IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                if let Ok(mut drag) = DRAG_MODE.lock() {
                    *drag = DragMode::None;
                }
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
