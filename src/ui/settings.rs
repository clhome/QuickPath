#![allow(dead_code)]

use crate::rules::{AppConfig, I18n, Language, MonitorPosition};
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
    DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, FW_SEMIBOLD,
    HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT, PS_NULL, PS_SOLID, SRCCOPY, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::ShellExecuteW;
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
pub enum SettingsTab {
    General,
    Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    None,
    OpacitySlider,
    AutoSwitchToggle { start_x: i32, initial_state: bool, has_moved: bool },
    AutostartToggle { start_x: i32, initial_state: bool, has_moved: bool },
    MonitorMasterToggle { start_x: i32, initial_state: bool, has_moved: bool },
    MonitorOpacitySlider,
}

static CURRENT_TAB: Mutex<SettingsTab> = Mutex::new(SettingsTab::General);
static HOVER_TAB: Mutex<Option<usize>> = Mutex::new(None);
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
                750,
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
            let win_h = (750.0 * scale).round() as i32;

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

        let current_tab = CURRENT_TAB.lock().map(|t| *t).unwrap_or(SettingsTab::General);
        let hovered = HOVER_CARD_IDX.lock().ok().and_then(|h| *h);

        // 1. 顶部 Header 品牌区（主标题 + 副标题）
        let header_text_x = pad_x;
        SelectObject(hdc, HGDIOBJ(font_h1.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let title_str = &bundle.settings.title;
        let mut title_buf: Vec<u16> = title_str.encode_utf16().collect();
        let mut title_rect = RECT {
            left: header_text_x,
            top: (14.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: (34.0 * scale).round() as i32,
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
            top: (35.0 * scale).round() as i32,
            right: rect.right - pad_x,
            bottom: (50.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut sub_buf, &mut sub_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 顶部 Tab 选项卡栏
        let tab_y = (56.0 * scale).round() as i32;
        let tab_h = (30.0 * scale).round() as i32;
        let tab_w = (116.0 * scale).round() as i32;
        let tab_gap = (8.0 * scale).round() as i32;
        render_segmented_tabs(
            hdc,
            pad_x,
            tab_y,
            tab_w,
            tab_h,
            tab_gap,
            scale,
            current_tab,
            hovered,
            font_card_title,
            &bundle.monitor.tab_general,
            &bundle.monitor.tab_monitor,
        );

        // 细微分割线
        let line_y = tab_y + tab_h + (8.0 * scale).round() as i32;
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
        let card_gap = (8.0 * scale).round() as i32;
        let card_top_base = line_y + (10.0 * scale).round() as i32;

        match current_tab {
            SettingsTab::General => {
                let card_h = (56.0 * scale).round() as i32;

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
                let about_h = (104.0 * scale).round() as i32;
                render_about_card(
                    hdc, pad_x, card5_top, pad_x + card_w, card5_top + about_h, scale,
                    &bundle, font_card_title, font_text, font_small, hovered,
                );
            }
            SettingsTab::Monitor => {
                let card_m0_top = card_top_base;
                let card_m0_h = (56.0 * scale).round() as i32;
                let is_hov_m0 = hovered == Some(30);
                render_toggle_card(
                    hdc, pad_x, card_m0_top, pad_x + card_w, card_m0_top + card_m0_h, scale,
                    font_card_title, font_text,
                    &bundle.monitor.master_switch, &bundle.monitor.master_switch_desc,
                    config.monitor.enabled,
                    &bundle.settings.status_enabled, &bundle.settings.status_disabled,
                    is_hov_m0,
                );

                let card_m1_top = card_m0_top + card_m0_h + card_gap;
                let card_m1_h = (130.0 * scale).round() as i32;
                render_metrics_checklist_card(
                    hdc, pad_x, card_m1_top, pad_x + card_w, card_m1_top + card_m1_h, scale,
                    font_card_title, font_text,
                    &bundle.monitor.metrics_title, &bundle.monitor.metrics_desc,
                    &bundle, &config.monitor, hovered,
                );

                let card_m2_top = card_m1_top + card_m1_h + card_gap;
                let card_m2_h = (172.0 * scale).round() as i32;
                render_monitor_visual_card(
                    hdc, pad_x, card_m2_top, pad_x + card_w, card_m2_top + card_m2_h, scale,
                    font_card_title, font_text,
                    &bundle.monitor.visual_title, &bundle.monitor.visual_desc,
                    &bundle, &config.monitor, hovered,
                );

                let card_m3_top = card_m2_top + card_m2_h + card_gap;
                let card_m3_h = (68.0 * scale).round() as i32;
                render_monitor_tips_card(
                    hdc, pad_x, card_m3_top, pad_x + card_w, card_m3_top + card_m3_h, scale,
                    font_card_title, font_small,
                );
            }
        }

        // 3. 底部「确定」与「取消」按钮（沉底排布，与窗体底部保留舒缓间距）
        let bottom_pad = (22.0 * scale).round() as i32;
        let btn_h = (34.0 * scale).round() as i32;
        let btn_w = (98.0 * scale).round() as i32;
        let btn_gap = (12.0 * scale).round() as i32;

        let btn_bottom = rect.bottom - bottom_pad;
        let btn_top = btn_bottom - btn_h;

        let btn_cancel_right = rect.right - pad_x;
        let btn_cancel_left = btn_cancel_right - btn_w;
        let btn_ok_right = btn_cancel_left - btn_gap;
        let btn_ok_left = btn_ok_right - btn_w;

        let btn_ok_rect = RECT {
            left: btn_ok_left,
            top: btn_top,
            right: btn_ok_right,
            bottom: btn_bottom,
        };
        let btn_cancel_rect = RECT {
            left: btn_cancel_left,
            top: btn_top,
            right: btn_cancel_right,
            bottom: btn_bottom,
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

        // 4. 底部微型标语与版本信息（与确定/取消按钮垂直平齐，排布在左侧）
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x006e6e6e));
        let ver_str = &bundle.settings.version_info;
        let mut ver_buf: Vec<u16> = ver_str.encode_utf16().collect();
        let mut ver_rect = RECT {
            left: pad_x,
            top: btn_top,
            right: btn_ok_left - (16.0 * scale).round() as i32,
            bottom: btn_bottom,
        };
        DrawTextW(hdc, &mut ver_buf, &mut ver_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);

        // 5. 顶层叠加渲染网页风格下拉选择选项层（展开时覆盖在关于卡片上方）
        if current_tab == SettingsTab::General && IS_DROPDOWN_OPEN.load(Ordering::SeqCst) {
            let card_h = (56.0 * scale).round() as i32;
            let card4_top = card_top_base + (card_h + card_gap) * 4;
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
    hovered: Option<usize>,
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

        // 行 1: QuickPath + 动态版本徽标
        SelectObject(hdc, HGDIOBJ(font_bold.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let ver_name = if bundle.about.version.is_empty() {
            format!("v{} 正式版 (原生极速 · 极简轻量)", crate::rules::version::APP_VERSION)
        } else {
            bundle.about.version.clone()
        };
        let line1 = format!("QuickPath  {}", ver_name);
        let mut buf1: Vec<u16> = line1.encode_utf16().collect();
        let mut r1 = RECT {
            left: text_x,
            top: top + (10.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (29.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut buf1, &mut r1, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 行 2: 出品方及官网链接（衢州御风科技有限公司 · qp.yftec.top ↗）
        SelectObject(hdc, HGDIOBJ(font_normal.0 as _));
        let is_hov_web = hovered == Some(20);
        let web_color = if is_hov_web {
            COLORREF(0x00ffcd60) // 悬停 Fluent Accent 亮天蓝
        } else {
            COLORREF(0x00d8d8d8) // 普通态柔和亮白
        };
        SetTextColor(hdc, web_color);
        let company_text = if bundle.about.company.is_empty() {
            "出品方：衢州御风科技有限公司"
        } else {
            &bundle.about.company
        };
        let line2 = format!("{}  ·  qp.yftec.top ↗", company_text);
        let mut buf2: Vec<u16> = line2.encode_utf16().collect();
        let mut r2 = RECT {
            left: text_x,
            top: top + (32.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (50.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut buf2, &mut r2, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 行 3: GitHub 官方开源地址（GitHub 纯白高清图标 + github.com/clhome/QuickPath ↗）
        let gh_size = (15.0 * scale).round() as i32;
        let gh_y = top + (54.0 * scale).round() as i32 + ((18.0 * scale) as i32 - gh_size) / 2;
        let _ = crate::win32::icon::draw_github_png(hdc, text_x, gh_y, gh_size, gh_size);

        let is_hov_github = hovered == Some(21);
        let gh_color = if is_hov_github {
            COLORREF(0x00ffcd60) // 悬停 Fluent Accent 亮天蓝
        } else {
            COLORREF(0x00b0b0b0) // 普通态优雅浅灰
        };
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, gh_color);
        let line3 = "GitHub: github.com/clhome/QuickPath ↗";
        let mut buf3: Vec<u16> = line3.encode_utf16().collect();
        let mut r3 = RECT {
            left: text_x + gh_size + (7.0 * scale).round() as i32,
            top: top + (53.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (72.0 * scale).round() as i32,
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
            top: top + (75.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (93.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut buf4, &mut r4, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 辅助渲染 Fluent 胶囊 Tab 切换栏
unsafe fn render_segmented_tabs(
    hdc: HDC,
    left: i32,
    top: i32,
    tab_w: i32,
    tab_h: i32,
    gap: i32,
    scale: f32,
    current_tab: SettingsTab,
    hovered: Option<usize>,
    font: windows::Win32::Graphics::Gdi::HFONT,
    label_general: &str,
    label_monitor: &str,
) {
    unsafe {
        let tabs = [
            (0usize, SettingsTab::General, label_general, 100usize),
            (1usize, SettingsTab::Monitor, label_monitor, 101usize),
        ];

        for (idx, tab_type, label, hov_id) in tabs {
            let tab_left = left + idx as i32 * (tab_w + gap);
            let tab_right = tab_left + tab_w;
            let tab_bottom = top + tab_h;

            let is_active = current_tab == tab_type;
            let is_hov = hovered == Some(hov_id);

            let (bg, border, text_color) = if is_active {
                (COLORREF(0x002e2e2e), COLORREF(0x004d4d4d), COLORREF(0x00ffffff))
            } else if is_hov {
                (COLORREF(0x00262626), COLORREF(0x00383838), COLORREF(0x00d0d0d0))
            } else {
                (COLORREF(0x001f1f1f), COLORREF(0x002a2a2a), COLORREF(0x00888888))
            };

            draw_fluent_box(hdc, tab_left, top, tab_right, tab_bottom, (6.0 * scale).round() as i32, bg, border);

            // 若激活，底部绘制 Win11 Accent Blue 发光指示条 (高 3px)
            if is_active {
                let bar_h = (3.0 * scale).round() as i32;
                let bar_pad = (16.0 * scale).round() as i32;
                let bar_rect = RECT {
                    left: tab_left + bar_pad,
                    top: tab_bottom - bar_h - 1,
                    right: tab_right - bar_pad,
                    bottom: tab_bottom - 1,
                };
                let bar_brush = CreateSolidBrush(COLORREF(0x00d47800)); // Accent Blue
                FillRect(hdc, &bar_rect, bar_brush);
                let _ = DeleteObject(HGDIOBJ(bar_brush.0 as _));
            }

            SelectObject(hdc, HGDIOBJ(font.0 as _));
            SetTextColor(hdc, text_color);
            let mut buf: Vec<u16> = label.encode_utf16().collect();
            let mut r = RECT {
                left: tab_left,
                top,
                right: tab_right,
                bottom: tab_bottom,
            };
            DrawTextW(hdc, &mut buf, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }
    }
}

/// 辅助绘制复选框项 (Checkbox)
unsafe fn draw_checkbox_item(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f32,
    font: windows::Win32::Graphics::Gdi::HFONT,
    label: &str,
    is_checked: bool,
    is_hovered: bool,
) {
    unsafe {
        if is_hovered {
            let hov_brush = CreateSolidBrush(COLORREF(0x002a2a2a));
            let r = RECT { left, top, right, bottom };
            FillRect(hdc, &r, hov_brush);
            let _ = DeleteObject(HGDIOBJ(hov_brush.0 as _));
        }

        let box_size = (16.0 * scale).round() as i32;
        let cy = top + (bottom - top) / 2;
        let box_left = left + (8.0 * scale).round() as i32;
        let box_top = cy - box_size / 2;
        let box_right = box_left + box_size;
        let box_bottom = box_top + box_size;

        let (box_bg, box_border) = if is_checked {
            (COLORREF(0x00d47800), COLORREF(0x00d47800)) // Accent Blue
        } else if is_hovered {
            (COLORREF(0x00282828), COLORREF(0x00666666))
        } else {
            (COLORREF(0x00222222), COLORREF(0x00444444))
        };

        draw_fluent_box(hdc, box_left, box_top, box_right, box_bottom, (4.0 * scale).round() as i32, box_bg, box_border);

        if is_checked {
            SelectObject(hdc, HGDIOBJ(font.0 as _));
            SetTextColor(hdc, COLORREF(0x00ffffff));
            let mut check_buf: Vec<u16> = "✓".encode_utf16().collect();
            let mut check_rect = RECT {
                left: box_left,
                top: box_top,
                right: box_right,
                bottom: box_bottom,
            };
            DrawTextW(hdc, &mut check_buf, &mut check_rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }

        SelectObject(hdc, HGDIOBJ(font.0 as _));
        SetTextColor(hdc, if is_checked { COLORREF(0x00f0f0f0) } else { COLORREF(0x00999999) });
        let mut text_buf: Vec<u16> = label.encode_utf16().collect();
        let mut text_rect = RECT {
            left: box_right + (8.0 * scale).round() as i32,
            top,
            right,
            bottom,
        };
        DrawTextW(hdc, &mut text_buf, &mut text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }
}

/// 渲染常驻指标勾选卡片
unsafe fn render_metrics_checklist_card(
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
    bundle: &crate::rules::LocaleBundle,
    config: &crate::rules::MonitorConfig,
    hovered: Option<usize>,
) {
    unsafe {
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, COLORREF(0x00242424), COLORREF(0x00353535));

        render_card_texts(hdc, left, top, right, top + (36.0 * scale).round() as i32, scale, font_title, font_desc, title, desc);

        let items_top = top + (42.0 * scale).round() as i32;
        let item_h = (26.0 * scale).round() as i32;
        let col_w = (right - left - (32.0 * scale).round() as i32) / 2;
        let col1_left = left + (16.0 * scale).round() as i32;
        let col2_left = col1_left + col_w;

        // 左列 (Net, CPU, Mem)
        draw_checkbox_item(
            hdc, col1_left, items_top, col1_left + col_w, items_top + item_h, scale,
            font_desc, &bundle.monitor.metric_net_label, config.show_network, hovered == Some(31),
        );
        draw_checkbox_item(
            hdc, col1_left, items_top + item_h, col1_left + col_w, items_top + item_h * 2, scale,
            font_desc, &bundle.monitor.metric_cpu_label, config.show_cpu, hovered == Some(32),
        );
        draw_checkbox_item(
            hdc, col1_left, items_top + item_h * 2, col1_left + col_w, items_top + item_h * 3, scale,
            font_desc, &bundle.monitor.metric_mem_label, config.show_memory, hovered == Some(33),
        );

        // 右列 (GPU, Disk)
        draw_checkbox_item(
            hdc, col2_left, items_top, col2_left + col_w, items_top + item_h, scale,
            font_desc, &bundle.monitor.metric_gpu_label, config.show_gpu, hovered == Some(34),
        );
        draw_checkbox_item(
            hdc, col2_left, items_top + item_h, col2_left + col_w, items_top + item_h * 2, scale,
            font_desc, &bundle.monitor.metric_disk_label, config.show_disk, hovered == Some(35),
        );
    }
}

/// 渲染外观与采样参数卡片
unsafe fn render_monitor_visual_card(
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
    bundle: &crate::rules::LocaleBundle,
    config: &crate::rules::MonitorConfig,
    hovered: Option<usize>,
) {
    unsafe {
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, COLORREF(0x00242424), COLORREF(0x00353535));

        render_card_texts(hdc, left, top, right, top + (34.0 * scale).round() as i32, scale, font_title, font_desc, title, desc);

        let row0_top = top + (36.0 * scale).round() as i32;
        let row1_top = top + (68.0 * scale).round() as i32;
        let row2_top = top + (100.0 * scale).round() as i32;
        let row3_top = top + (132.0 * scale).round() as i32;
        let inner_x = left + (16.0 * scale).round() as i32;
        let right_pad = (18.0 * scale).round() as i32;

        // 行 0: 任务栏停靠位置 (2段式单选胶囊，防遮挡)
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00d0d0d0));
        let mut r0_lbl: Vec<u16> = bundle.monitor.position_label.encode_utf16().collect();
        let mut r0_rect = RECT {
            left: inner_x,
            top: row0_top,
            right: inner_x + (160.0 * scale).round() as i32,
            bottom: row0_top + (26.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut r0_lbl, &mut r0_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let pos_w = (120.0 * scale).round() as i32;
        let seg_h = (24.0 * scale).round() as i32;
        let seg_gap = (6.0 * scale).round() as i32;
        let pos_base_right = right - right_pad;
        let pos_opts = [
            (1, MonitorPosition::TaskbarLeft, &bundle.monitor.menu_pos_taskbar_left, 46usize),
            (0, MonitorPosition::TrayLeft, &bundle.monitor.menu_pos_tray_left, 45usize),
        ];

        for (rev_i, pos_val, label, hov_id) in pos_opts {
            let seg_r = pos_base_right - rev_i * (pos_w + seg_gap);
            let seg_l = seg_r - pos_w;
            let is_sel = config.position == pos_val;
            let is_hov = hovered == Some(hov_id);

            let (s_bg, s_bd, s_txt) = if is_sel {
                (COLORREF(0x00d47800), COLORREF(0x00d47800), COLORREF(0x00ffffff))
            } else if is_hov {
                (COLORREF(0x00333333), COLORREF(0x00555555), COLORREF(0x00e0e0e0))
            } else {
                (COLORREF(0x00282828), COLORREF(0x003d3d3d), COLORREF(0x00909090))
            };

            let seg_rect = RECT { left: seg_l, top: row0_top, right: seg_r, bottom: row0_top + seg_h };
            render_rounded_button(hdc, &seg_rect, label, font_desc, s_bg, s_txt, Some(s_bd), (4.0 * scale).round() as i32);
        }

        // 行 1: 采样刷新频率 (3段式单选胶囊)
        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00d0d0d0));
        let mut r1_lbl: Vec<u16> = bundle.monitor.interval_label.encode_utf16().collect();
        let mut r1_rect = RECT {
            left: inner_x,
            top: row1_top,
            right: inner_x + (160.0 * scale).round() as i32,
            bottom: row1_top + (26.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut r1_lbl, &mut r1_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let seg_w = (84.0 * scale).round() as i32;
        let seg_h = (24.0 * scale).round() as i32;
        let seg_gap = (6.0 * scale).round() as i32;
        let seg_base_right = right - right_pad;
        let seg_opts = [
            (2, 1000u64, &bundle.monitor.interval_1s, 40usize),
            (1, 3000u64, &bundle.monitor.interval_3s, 41usize),
            (0, 5000u64, &bundle.monitor.interval_5s, 42usize),
        ];

        for (rev_i, val, label, hov_id) in seg_opts {
            let seg_r = seg_base_right - rev_i * (seg_w + seg_gap);
            let seg_l = seg_r - seg_w;
            let is_sel = config.refresh_interval_ms == val;
            let is_hov = hovered == Some(hov_id);

            let (s_bg, s_bd, s_txt) = if is_sel {
                (COLORREF(0x00d47800), COLORREF(0x00d47800), COLORREF(0x00ffffff))
            } else if is_hov {
                (COLORREF(0x00333333), COLORREF(0x00555555), COLORREF(0x00e0e0e0))
            } else {
                (COLORREF(0x00282828), COLORREF(0x003d3d3d), COLORREF(0x00909090))
            };

            let seg_rect = RECT { left: seg_l, top: row1_top, right: seg_r, bottom: row1_top + seg_h };
            render_rounded_button(hdc, &seg_rect, label, font_desc, s_bg, s_txt, Some(s_bd), (4.0 * scale).round() as i32);
        }

        // 行 2: 实时历史波形折线图 Toggle (ID 43)
        SetTextColor(hdc, COLORREF(0x00d0d0d0));
        let mut r2_lbl: Vec<u16> = bundle.monitor.graph_bg_label.encode_utf16().collect();
        let mut r2_rect = RECT {
            left: inner_x,
            top: row2_top,
            right: inner_x + (280.0 * scale).round() as i32,
            bottom: row2_top + (24.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut r2_lbl, &mut r2_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let sw_w = (46.0 * scale).round() as i32;
        let sw_h = (22.0 * scale).round() as i32;
        let sw_right = right - right_pad;
        let sw_left = sw_right - sw_w;
        let sw_cy = row2_top + sw_h / 2;
        let sw_top = sw_cy - sw_h / 2;
        let sw_bottom = sw_top + sw_h;

        let is_on = config.show_graph_bg;
        let (sw_bg, sw_border) = if is_on {
            (COLORREF(0x00d47800), COLORREF(0x00d47800))
        } else {
            (COLORREF(0x00323232), COLORREF(0x00525252))
        };
        draw_fluent_box(hdc, sw_left, sw_top, sw_right, sw_bottom, sw_h, sw_bg, sw_border);

        let thumb_margin = (2.0 * scale).round() as i32;
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
            hdc, thumb_cx - thumb_radius, sw_cy - thumb_radius,
            thumb_cx + thumb_radius + 1, sw_cy + thumb_radius + 1,
            thumb_radius * 2, thumb_radius * 2,
        );
        let _ = SelectObject(hdc, old_b);
        let _ = SelectObject(hdc, old_p);
        let _ = DeleteObject(HGDIOBJ(thumb_brush.0 as _));
        let _ = DeleteObject(HGDIOBJ(thumb_pen.0 as _));

        // 行 3: 监控面板透明度 Slider (ID 44)
        SetTextColor(hdc, COLORREF(0x00d0d0d0));
        let mut r3_lbl: Vec<u16> = bundle.monitor.opacity_label.encode_utf16().collect();
        let mut r3_rect = RECT {
            left: inner_x,
            top: row3_top,
            right: inner_x + (160.0 * scale).round() as i32,
            bottom: row3_top + (24.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut r3_lbl, &mut r3_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let slider_w = (140.0 * scale).round() as i32;
        let track_right = right - right_pad;
        let track_left = track_right - slider_w;

        let pct = config.opacity.clamp(50, 100);
        let pct_str = format!("{}%", pct);
        let mut pct_buf: Vec<u16> = pct_str.encode_utf16().collect();
        let mut pct_rect = RECT {
            left: track_left - (50.0 * scale).round() as i32,
            top: row3_top,
            right: track_left - (8.0 * scale).round() as i32,
            bottom: row3_top + (24.0 * scale).round() as i32,
        };
        SetTextColor(hdc, COLORREF(0x0050d268));
        DrawTextW(hdc, &mut pct_buf, &mut pct_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);

        let track_cy = row3_top + (12.0 * scale).round() as i32;
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

        let ratio = ((pct as f32 - 50.0) / 50.0).clamp(0.0, 1.0);
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

/// 渲染交互快捷操作与特性说明卡片
unsafe fn render_monitor_tips_card(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f32,
    font_bold: windows::Win32::Graphics::Gdi::HFONT,
    font_small: windows::Win32::Graphics::Gdi::HFONT,
) {
    unsafe {
        draw_fluent_box(hdc, left, top, right, bottom, (8.0 * scale).round() as i32, COLORREF(0x00212121), COLORREF(0x00343434));

        let inner_x = left + (16.0 * scale).round() as i32;

        SelectObject(hdc, HGDIOBJ(font_bold.0 as _));
        SetTextColor(hdc, COLORREF(0x0050d268));
        let mut title_buf: Vec<u16> = "💡 高效交互与智能特性指南".encode_utf16().collect();
        let mut title_rect = RECT {
            left: inner_x,
            top: top + (8.0 * scale).round() as i32,
            right: right - (16.0 * scale).round() as i32,
            bottom: top + (26.0 * scale).round() as i32,
        };
        DrawTextW(hdc, &mut title_buf, &mut title_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x009e9e9e));

        let tips = [
            "• 双击直接调出系统任务管理器；右键菜单可快速切换常驻指标与停靠位置",
            "• 鼠标悬停弹出 Fluent 硬件详情看板；全屏应用与 3D 游戏智能避让隐匿",
        ];

        let line_h = (18.0 * scale).round() as i32;
        let mut tip_top = top + (30.0 * scale).round() as i32;
        for tip in tips {
            let mut buf: Vec<u16> = tip.encode_utf16().collect();
            let mut r = RECT {
                left: inner_x,
                top: tip_top,
                right: right - (16.0 * scale).round() as i32,
                bottom: tip_top + line_h,
            };
            DrawTextW(hdc, &mut buf, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            tip_top += line_h + (2.0 * scale).round() as i32;
        }
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
    let card_gap = (8.0 * scale).round() as i32;

    // 顶部 Tab 栏坐标
    let tab_y = (56.0 * scale).round() as i32;
    let tab_h = (30.0 * scale).round() as i32;
    let tab_w = (116.0 * scale).round() as i32;
    let tab_gap = (8.0 * scale).round() as i32;
    let line_y = tab_y + tab_h + (8.0 * scale).round() as i32;
    let card_top_base = line_y + (10.0 * scale).round() as i32;

    let bottom_pad = (22.0 * scale).round() as i32;
    let btn_h = (34.0 * scale).round() as i32;
    let btn_bottom = rect.bottom - bottom_pad;
    let btn_top = btn_bottom - btn_h;
    let btn_w = (98.0 * scale).round() as i32;
    let btn_gap = (12.0 * scale).round() as i32;

    let btn_cancel_right = rect.right - pad_x;
    let btn_cancel_left = btn_cancel_right - btn_w;
    let btn_ok_right = btn_cancel_left - btn_gap;
    let btn_ok_left = btn_ok_right - btn_w;

    let current_tab = CURRENT_TAB.lock().map(|t| *t).unwrap_or(SettingsTab::General);

    // 如果下拉层处于展开状态（仅在 General Tab），优先处理下拉列表项的悬停检测
    if current_tab == SettingsTab::General && IS_DROPDOWN_OPEN.load(Ordering::SeqCst) {
        let card_h = (56.0 * scale).round() as i32;
        let card4_top = card_top_base + (card_h + card_gap) * 4;
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
            DragMode::MonitorMasterToggle { start_x, initial_state, ref mut has_moved } => {
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
                if config.monitor.enabled != new_state {
                    config.monitor.enabled = new_state;
                    set_draft_config(config);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                return;
            }
            DragMode::MonitorOpacitySlider => {
                let slider_w = (140.0 * scale).round() as i32;
                let right_pad = (18.0 * scale).round() as i32;
                let track_right = rect.right - pad_x - right_pad;
                let track_left = track_right - slider_w;

                let ratio = ((x - track_left) as f32 / slider_w as f32).clamp(0.0, 1.0);
                let new_val = (50.0 + ratio * 50.0).round() as u8;

                let mut config = get_draft_config();
                if config.monitor.opacity != new_val {
                    config.monitor.opacity = new_val;
                    set_draft_config(config);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        let _ = UpdateWindow(hwnd);
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

    // 1. 顶部 Tab 选项卡检测
    if y >= tab_y && y <= tab_y + tab_h {
        let tab0_r = pad_x + tab_w;
        let tab1_l = tab0_r + tab_gap;
        let tab1_r = tab1_l + tab_w;
        if x >= pad_x && x <= tab0_r {
            new_hover = Some(100);
            is_pointer = true;
        } else if x >= tab1_l && x <= tab1_r {
            new_hover = Some(101);
            is_pointer = true;
        }
    }

    // 2. 根据 Tab 内容检测卡片与交互控件
    match current_tab {
        SettingsTab::General => {
            let card_h = (56.0 * scale).round() as i32;
            let card0_top = card_top_base;
            let card1_top = card0_top + card_h + card_gap;
            let card2_top = card1_top + card_h + card_gap;
            let card3_top = card2_top + card_h + card_gap;
            let card4_top = card3_top + card_h + card_gap;
            let card5_top = card4_top + card_h + card_gap + (2.0 * scale).round() as i32;
            let about_h = (104.0 * scale).round() as i32;

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
                } else if y >= card5_top && y <= card5_top + about_h {
                    let logo_w = ((about_h as f32) * 0.8).round() as i32;
                    let text_x = pad_x + (16.0 * scale).round() as i32 + logo_w + (18.0 * scale).round() as i32;
                    let text_right = pad_x + card_w - (16.0 * scale).round() as i32;

                    let web_top = card5_top + (32.0 * scale).round() as i32;
                    let web_bottom = card5_top + (50.0 * scale).round() as i32;
                    let gh_top = card5_top + (53.0 * scale).round() as i32;
                    let gh_bottom = card5_top + (72.0 * scale).round() as i32;

                    if x >= text_x && x <= text_right && y >= web_top && y <= web_bottom {
                        new_hover = Some(20);
                        is_pointer = true;
                    } else if x >= text_x && x <= text_right && y >= gh_top && y <= gh_bottom {
                        new_hover = Some(21);
                        is_pointer = true;
                    }
                }
            }
        }
        SettingsTab::Monitor => {
            let card_m0_top = card_top_base;
            let card_m0_h = (56.0 * scale).round() as i32;
            let card_m1_top = card_m0_top + card_m0_h + card_gap;
            let card_m1_h = (130.0 * scale).round() as i32;
            let card_m2_top = card_m1_top + card_m1_h + card_gap;
            let card_m2_h = (140.0 * scale).round() as i32;

            if x >= pad_x && x <= pad_x + card_w {
                if y >= card_m0_top && y <= card_m0_top + card_m0_h {
                    new_hover = Some(30);
                    is_pointer = true;
                } else if y >= card_m1_top && y <= card_m1_top + card_m1_h {
                    let items_top = card_m1_top + (42.0 * scale).round() as i32;
                    let item_h = (26.0 * scale).round() as i32;
                    let col_w = (card_w - (32.0 * scale).round() as i32) / 2;
                    let col1_left = pad_x + (16.0 * scale).round() as i32;
                    let col2_left = col1_left + col_w;

                    if y >= items_top && y <= items_top + item_h * 3 {
                        let row = ((y - items_top) / item_h) as usize;
                        if x >= col1_left && x <= col1_left + col_w {
                            match row {
                                0 => { new_hover = Some(31); is_pointer = true; }
                                1 => { new_hover = Some(32); is_pointer = true; }
                                2 => { new_hover = Some(33); is_pointer = true; }
                                _ => {}
                            }
                        } else if x >= col2_left && x <= col2_left + col_w {
                            match row {
                                0 => { new_hover = Some(34); is_pointer = true; }
                                1 => { new_hover = Some(35); is_pointer = true; }
                                _ => {}
                            }
                        }
                    }
                } else if y >= card_m2_top && y <= card_m2_top + card_m2_h {
                    let row0_top = card_m2_top + (36.0 * scale).round() as i32;
                    let row1_top = card_m2_top + (68.0 * scale).round() as i32;
                    let row2_top = card_m2_top + (100.0 * scale).round() as i32;
                    let row3_top = card_m2_top + (132.0 * scale).round() as i32;
                    let right_pad = (18.0 * scale).round() as i32;

                    // 行 0: 2个停靠位置胶囊 (ID 45 / 46)
                    if y >= row0_top && y <= row0_top + (24.0 * scale).round() as i32 {
                        let pos_w = (120.0 * scale).round() as i32;
                        let seg_gap = (6.0 * scale).round() as i32;
                        let pos_base_right = pad_x + card_w - right_pad;
                        let pos_opts = [(1, 46usize), (0, 45usize)];
                        for (rev_i, hov_id) in pos_opts {
                            let seg_r = pos_base_right - rev_i * (pos_w + seg_gap);
                            let seg_l = seg_r - pos_w;
                            if x >= seg_l && x <= seg_r {
                                new_hover = Some(hov_id);
                                is_pointer = true;
                                break;
                            }
                        }
                    }
                    // 行 1: 3个单选胶囊 (ID 40 / 41 / 42)
                    else if y >= row1_top && y <= row1_top + (24.0 * scale).round() as i32 {
                        let seg_w = (84.0 * scale).round() as i32;
                        let seg_gap = (6.0 * scale).round() as i32;
                        let seg_base_right = pad_x + card_w - right_pad;
                        let seg_opts = [(2, 40usize), (1, 41usize), (0, 42usize)];
                        for (rev_i, hov_id) in seg_opts {
                            let seg_r = seg_base_right - rev_i * (seg_w + seg_gap);
                            let seg_l = seg_r - seg_w;
                            if x >= seg_l && x <= seg_r {
                                new_hover = Some(hov_id);
                                is_pointer = true;
                                break;
                            }
                        }
                    }
                    // 行 2: 波形 Toggle
                    else if y >= row2_top && y <= row2_top + (24.0 * scale).round() as i32 {
                        new_hover = Some(43);
                        is_pointer = true;
                    }
                    // 行 3: 透明度 Slider
                    else if y >= row3_top && y <= row3_top + (24.0 * scale).round() as i32 {
                        let slider_w = (140.0 * scale).round() as i32;
                        let track_right = pad_x + card_w - right_pad;
                        let track_left = track_right - slider_w;
                        if x >= track_left - (10.0 * scale) as i32 && x <= track_right + (10.0 * scale) as i32 {
                            new_hover = Some(44);
                            is_pointer = true;
                        }
                    }
                }
            }
        }
    }

    // 3. 底部确定与取消按钮检测
    if y >= btn_top && y <= btn_bottom {
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
    let card_gap = (8.0 * scale).round() as i32;

    let tab_y = (56.0 * scale).round() as i32;
    let tab_h = (30.0 * scale).round() as i32;
    let tab_w = (116.0 * scale).round() as i32;
    let tab_gap = (8.0 * scale).round() as i32;
    let line_y = tab_y + tab_h + (8.0 * scale).round() as i32;
    let card_top_base = line_y + (10.0 * scale).round() as i32;

    let current_tab = CURRENT_TAB.lock().map(|t| *t).unwrap_or(SettingsTab::General);

    // 1. 顶部 Tab 选项卡点击
    if y >= tab_y && y <= tab_y + tab_h {
        let tab0_r = pad_x + tab_w;
        let tab1_l = tab0_r + tab_gap;
        let tab1_r = tab1_l + tab_w;
        if x >= pad_x && x <= tab0_r {
            if let Ok(mut t) = CURRENT_TAB.lock() {
                *t = SettingsTab::General;
            }
            IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
            IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
                let _ = UpdateWindow(hwnd);
            }
            return;
        } else if x >= tab1_l && x <= tab1_r {
            if let Ok(mut t) = CURRENT_TAB.lock() {
                *t = SettingsTab::Monitor;
            }
            IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
            IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
                let _ = UpdateWindow(hwnd);
            }
            return;
        }
    }

    // 2. 根据 Tab 内容分流处理
    match current_tab {
        SettingsTab::General => {
            let card_h = (56.0 * scale).round() as i32;
            let card0_top = card_top_base;
            let card1_top = card0_top + card_h + card_gap;
            let card2_top = card1_top + card_h + card_gap;
            let card3_top = card2_top + card_h + card_gap;
            let card4_top = card3_top + card_h + card_gap;
            let card5_top = card4_top + card_h + card_gap + (2.0 * scale).round() as i32;
            let about_h = (104.0 * scale).round() as i32;

            let is_dropdown_open = IS_DROPDOWN_OPEN.load(Ordering::SeqCst);
            let combo_w = (170.0 * scale).round() as i32;
            let combo_h = (30.0 * scale).round() as i32;
            let right_pad = (16.0 * scale).round() as i32;
            let combo_right = pad_x + card_w - right_pad;
            let combo_left = combo_right - combo_w;
            let combo_cy = card4_top + card_h / 2;
            let combo_top = combo_cy - combo_h / 2;
            let combo_bottom = combo_top + combo_h;

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

                if x >= combo_left && x <= combo_right && y >= combo_top && y <= combo_bottom {
                    IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        let _ = UpdateWindow(hwnd);
                    }
                    return;
                }

                IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    let _ = UpdateWindow(hwnd);
                }
                return;
            }

            if x >= pad_x && x <= pad_x + card_w && y >= card0_top && y <= card0_top + card_h {
                let old_state = config.auto_switch_enabled;
                config.auto_switch_enabled = !old_state;
                set_draft_config(config.clone());
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
            } else if x >= pad_x && x <= pad_x + card_w && y >= card1_top && y <= card1_top + card_h {
                let is_rec = IS_RECORDING_HOTKEY.load(Ordering::SeqCst);
                IS_RECORDING_HOTKEY.store(!is_rec, Ordering::SeqCst);
                need_redraw = true;
            } else if x >= pad_x && x <= pad_x + card_w && y >= card2_top && y <= card2_top + card_h {
                let old_state = config.autostart_enabled;
                config.autostart_enabled = !old_state;
                set_draft_config(config.clone());
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
            } else if x >= pad_x && x <= pad_x + card_w && y >= card3_top && y <= card3_top + card_h {
                IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                let slider_w = (140.0 * scale).round() as i32;
                let track_right = rect.right - pad_x - (18.0 * scale).round() as i32;
                let track_left = track_right - slider_w;

                if x >= track_left - (20.0 * scale) as i32 && x <= track_right + (20.0 * scale) as i32 {
                    let ratio = ((x - track_left) as f32 / slider_w as f32).clamp(0.0, 1.0);
                    let new_val = (40.0 + ratio * 60.0).round() as u8;
                    config.floating_bar_opacity = new_val;
                    set_draft_config(config.clone());
                    need_redraw = true;

                    if let Ok(mut drag) = DRAG_MODE.lock() {
                        *drag = DragMode::OpacitySlider;
                    }
                    unsafe {
                        let _ = SetCapture(hwnd);
                    }
                }
            } else if y >= card4_top && y <= card4_top + card_h {
                if x >= combo_left && x <= combo_right && y >= combo_top && y <= combo_bottom {
                    IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                    IS_DROPDOWN_OPEN.store(true, Ordering::SeqCst);
                    need_redraw = true;
                }
            } else if y >= card5_top && y <= card5_top + about_h {
                let logo_w = ((about_h as f32) * 0.8).round() as i32;
                let text_x = pad_x + (16.0 * scale).round() as i32 + logo_w + (18.0 * scale).round() as i32;
                let text_right = pad_x + card_w - (16.0 * scale).round() as i32;

                let web_top = card5_top + (32.0 * scale).round() as i32;
                let web_bottom = card5_top + (50.0 * scale).round() as i32;
                let gh_top = card5_top + (53.0 * scale).round() as i32;
                let gh_bottom = card5_top + (72.0 * scale).round() as i32;

                if x >= text_x && x <= text_right && y >= web_top && y <= web_bottom {
                    let url: Vec<u16> = "https://qp.yftec.top\0".encode_utf16().collect();
                    let op: Vec<u16> = "open\0".encode_utf16().collect();
                    unsafe {
                        ShellExecuteW(
                            None,
                            windows::core::PCWSTR(op.as_ptr()),
                            windows::core::PCWSTR(url.as_ptr()),
                            windows::core::PCWSTR::null(),
                            windows::core::PCWSTR::null(),
                            SW_SHOW,
                        );
                    }
                    return;
                } else if x >= text_x && x <= text_right && y >= gh_top && y <= gh_bottom {
                    let url: Vec<u16> = "https://github.com/clhome/QuickPath\0".encode_utf16().collect();
                    let op: Vec<u16> = "open\0".encode_utf16().collect();
                    unsafe {
                        ShellExecuteW(
                            None,
                            windows::core::PCWSTR(op.as_ptr()),
                            windows::core::PCWSTR(url.as_ptr()),
                            windows::core::PCWSTR::null(),
                            windows::core::PCWSTR::null(),
                            SW_SHOW,
                        );
                    }
                    return;
                }
            }
        }
        SettingsTab::Monitor => {
            let card_m0_top = card_top_base;
            let card_m0_h = (56.0 * scale).round() as i32;
            let card_m1_top = card_m0_top + card_m0_h + card_gap;
            let card_m1_h = (130.0 * scale).round() as i32;
            let card_m2_top = card_m1_top + card_m1_h + card_gap;
            let card_m2_h = (140.0 * scale).round() as i32;

            // 点击 M0: 任务栏监控总开关
            if x >= pad_x && x <= pad_x + card_w && y >= card_m0_top && y <= card_m0_top + card_m0_h {
                let old_state = config.monitor.enabled;
                config.monitor.enabled = !old_state;
                set_draft_config(config.clone());
                need_redraw = true;

                if let Ok(mut drag) = DRAG_MODE.lock() {
                    *drag = DragMode::MonitorMasterToggle {
                        start_x: x,
                        initial_state: old_state,
                        has_moved: false,
                    };
                }
                unsafe {
                    let _ = SetCapture(hwnd);
                }
            }
            // 点击 M1: 常驻指标勾选面板
            else if x >= pad_x && x <= pad_x + card_w && y >= card_m1_top && y <= card_m1_top + card_m1_h {
                let items_top = card_m1_top + (42.0 * scale).round() as i32;
                let item_h = (26.0 * scale).round() as i32;
                let col_w = (card_w - (32.0 * scale).round() as i32) / 2;
                let col1_left = pad_x + (16.0 * scale).round() as i32;
                let col2_left = col1_left + col_w;

                if y >= items_top && y <= items_top + item_h * 3 {
                    let row = ((y - items_top) / item_h) as usize;
                    if x >= col1_left && x <= col1_left + col_w {
                        match row {
                            0 => config.monitor.show_network = !config.monitor.show_network,
                            1 => config.monitor.show_cpu = !config.monitor.show_cpu,
                            2 => config.monitor.show_memory = !config.monitor.show_memory,
                            _ => {}
                        }
                        // 防呆保护：至少保留 1 项开启
                        if !config.monitor.show_network && !config.monitor.show_cpu && !config.monitor.show_memory && !config.monitor.show_gpu && !config.monitor.show_disk {
                            match row {
                                0 => config.monitor.show_network = true,
                                1 => config.monitor.show_cpu = true,
                                2 => config.monitor.show_memory = true,
                                _ => {}
                            }
                        }
                        set_draft_config(config.clone());
                        need_redraw = true;
                    } else if x >= col2_left && x <= col2_left + col_w {
                        match row {
                            0 => config.monitor.show_gpu = !config.monitor.show_gpu,
                            1 => config.monitor.show_disk = !config.monitor.show_disk,
                            _ => {}
                        }
                        if !config.monitor.show_network && !config.monitor.show_cpu && !config.monitor.show_memory && !config.monitor.show_gpu && !config.monitor.show_disk {
                            match row {
                                0 => config.monitor.show_gpu = true,
                                1 => config.monitor.show_disk = true,
                                _ => {}
                            }
                        }
                        set_draft_config(config.clone());
                        need_redraw = true;
                    }
                }
            }
            // 点击 M2: 外观与采样参数面板
            else if x >= pad_x && x <= pad_x + card_w && y >= card_m2_top && y <= card_m2_top + card_m2_h {
                let row0_top = card_m2_top + (36.0 * scale).round() as i32;
                let row1_top = card_m2_top + (68.0 * scale).round() as i32;
                let row2_top = card_m2_top + (100.0 * scale).round() as i32;
                let row3_top = card_m2_top + (132.0 * scale).round() as i32;
                let seg_h = (24.0 * scale).round() as i32;
                let right_pad = (18.0 * scale).round() as i32;

                // 行 0: 任务栏停靠位置单选胶囊 (防遮挡)
                if y >= row0_top && y <= row0_top + seg_h {
                    let pos_w = (120.0 * scale).round() as i32;
                    let seg_gap = (6.0 * scale).round() as i32;
                    let pos_base_right = pad_x + card_w - right_pad;
                    let pos_opts = [
                        (1, MonitorPosition::TaskbarLeft),
                        (0, MonitorPosition::TrayLeft),
                    ];
                    for (rev_i, pos_val) in pos_opts {
                        let seg_r = pos_base_right - rev_i * (pos_w + seg_gap);
                        let seg_l = seg_r - pos_w;
                        if x >= seg_l && x <= seg_r {
                            if config.monitor.position != pos_val {
                                config.monitor.position = pos_val;
                                set_draft_config(config.clone());
                                need_redraw = true;
                            }
                            break;
                        }
                    }
                }
                // 行 1: 刷新采样频率胶囊单选
                else if y >= row1_top && y <= row1_top + seg_h {
                    let seg_w = (84.0 * scale).round() as i32;
                    let seg_gap = (6.0 * scale).round() as i32;
                    let seg_base_right = pad_x + card_w - right_pad;
                    let seg_opts = [
                        (2, 1000u64),
                        (1, 3000u64),
                        (0, 5000u64),
                    ];
                    for (rev_i, val) in seg_opts {
                        let seg_r = seg_base_right - rev_i * (seg_w + seg_gap);
                        let seg_l = seg_r - seg_w;
                        if x >= seg_l && x <= seg_r {
                            config.monitor.refresh_interval_ms = val;
                            set_draft_config(config.clone());
                            need_redraw = true;
                            break;
                        }
                    }
                }
                // 行 2: 实时历史波形折线图 Toggle
                else if y >= row2_top && y <= row2_top + (24.0 * scale).round() as i32 {
                    config.monitor.show_graph_bg = !config.monitor.show_graph_bg;
                    set_draft_config(config.clone());
                    need_redraw = true;
                }
                // 行 3: 监控面板透明度 Slider
                else if y >= row3_top && y <= row3_top + (24.0 * scale).round() as i32 {
                    let slider_w = (140.0 * scale).round() as i32;
                    let track_right = pad_x + card_w - right_pad;
                    let track_left = track_right - slider_w;
                    if x >= track_left - (20.0 * scale) as i32 && x <= track_right + (20.0 * scale) as i32 {
                        let ratio = ((x - track_left) as f32 / slider_w as f32).clamp(0.0, 1.0);
                        let new_val = (50.0 + ratio * 50.0).round() as u8;
                        config.monitor.opacity = new_val;
                        set_draft_config(config.clone());
                        need_redraw = true;

                        if let Ok(mut drag) = DRAG_MODE.lock() {
                            *drag = DragMode::MonitorOpacitySlider;
                        }
                        unsafe {
                            let _ = SetCapture(hwnd);
                        }
                    }
                }
            }
        }
    }

    // 3. 底部「确定」与「取消」按钮点击处理（通用）
    let bottom_pad = (22.0 * scale).round() as i32;
    let btn_h = (34.0 * scale).round() as i32;
    let btn_bottom = rect.bottom - bottom_pad;
    let btn_top = btn_bottom - btn_h;
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

    if need_redraw {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
    }
}
