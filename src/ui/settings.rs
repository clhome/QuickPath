#![allow(dead_code)]
// 自绘几何/布局辅助函数天然需要较多坐标与配色参数;拆成参数结构体只会增加
// 调用端样板而不带来任何安全性或可读性收益(clippy::too_many_arguments lint 说明)。
#![allow(clippy::too_many_arguments)]

use crate::rules::{AppConfig, I18n, Language, MonitorPosition};
use crate::win32::autostart::set_autostart;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW, EndPaint, FW_BOLD,
    FW_NORMAL, FW_SEMIBOLD, FillRect, GetStockObject, HBRUSH, HDC, HGDIOBJ, InvalidateRect,
    PAINTSTRUCT, PS_NULL, PS_SOLID, RoundRect, SRCCOPY, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT, UpdateWindow, WHITE_BRUSH,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DI_NORMAL, DefWindowProcW, DrawIconEx, GetClientRect,
    GetSystemMetrics, ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_HAND, LoadCursorW, RegisterClassW,
    SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SendMessageW, SetCursor,
    SetWindowPos, ShowWindow, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETICON, WM_SYSKEYDOWN, WNDCLASSW, WS_CAPTION,
    WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_SYSMENU,
};
use windows::core::w;

static SETTINGS_WINDOW_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    TabModule,
    Monitor,
    /// 审计报告 U-01：第三方管理器联动（仅授权开关 + 手动触发说明）
    Integration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    None,
    OpacitySlider,
    AutoSwitchToggle {
        start_x: i32,
        initial_state: bool,
        has_moved: bool,
    },
    AutostartToggle {
        start_x: i32,
        initial_state: bool,
        has_moved: bool,
    },
    TabMasterToggle {
        start_x: i32,
        initial_state: bool,
        has_moved: bool,
    },
    MonitorMasterToggle {
        start_x: i32,
        initial_state: bool,
        has_moved: bool,
    },
    /// 管理器联动页的授权开关（`which`: 0 = Total Commander，1 = XYplorer）
    IntegrationToggle {
        which: usize,
        start_x: i32,
        initial_state: bool,
        has_moved: bool,
    },
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
    if let Ok(draft) = DRAFT_CONFIG.lock()
        && let Some(cfg) = &*draft
    {
        return cfg.clone();
    }
    crate::get_global_config()
}

fn set_draft_config(cfg: AppConfig) {
    if let Ok(mut draft) = DRAFT_CONFIG.lock() {
        *draft = Some(cfg);
    }
}

/// General Tab 底部区域的悬停/命中 id：渲染、命中测试与点击处理共用同一常量，
/// 避免新增卡片后出现「点 A 触发 B」的错位。
const HOVER_ID_DEBUG_CHECKBOX: usize = 50;
const HOVER_ID_LOG_CARD: usize = 51;
const HOVER_ID_LOG_CLEAR: usize = 52;

/// 顶部 Tab 栏悬停/命中 id（渲染、悬停、点击三处共用）
const HOVER_ID_TAB_GENERAL: usize = 100;
const HOVER_ID_TAB_TABMODULE: usize = 102;
const HOVER_ID_TAB_MONITOR: usize = 101;
const HOVER_ID_TAB_INTEGRATION: usize = 103;
/// 管理器联动页的开关卡片 id（`which` 与 `DragMode::IntegrationToggle` 一致）
const HOVER_ID_INTEGRATION_TC: usize = 70;
const HOVER_ID_INTEGRATION_XY: usize = 71;

/// 顶部 Tab 栏几何参数（渲染 / 悬停 / 点击共用，避免宽度不一致导致高亮错位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabGeometry {
    top: i32,
    height: i32,
    width: i32,
    gap: i32,
}

/// 全部 Tab（顺序即绘制顺序）
const SETTINGS_TABS: [(SettingsTab, usize); 4] = [
    (SettingsTab::General, HOVER_ID_TAB_GENERAL),
    (SettingsTab::TabModule, HOVER_ID_TAB_TABMODULE),
    (SettingsTab::Monitor, HOVER_ID_TAB_MONITOR),
    (SettingsTab::Integration, HOVER_ID_TAB_INTEGRATION),
];

fn settings_tab_geometry(scale: f32) -> TabGeometry {
    TabGeometry {
        top: (56.0 * scale).round() as i32,
        height: (30.0 * scale).round() as i32,
        width: (118.0 * scale).round() as i32,
        gap: (8.0 * scale).round() as i32,
    }
}

fn settings_tab_left(pad_x: i32, geo: &TabGeometry, index: usize) -> i32 {
    pad_x + index as i32 * (geo.width + geo.gap)
}

/// 命中测试：返回被点击/悬停的 Tab 悬停 id（不在 Tab 栏范围内时返回 `None`）。
fn settings_tab_hit(pad_x: i32, geo: &TabGeometry, x: i32, y: i32) -> Option<(usize, SettingsTab)> {
    if y < geo.top || y > geo.top + geo.height {
        return None;
    }
    for (index, (tab, hover_id)) in SETTINGS_TABS.iter().enumerate() {
        let left = settings_tab_left(pad_x, geo, index);
        if x >= left && x <= left + geo.width {
            return Some((*hover_id, *tab));
        }
    }
    None
}

/// 最近一次「清理日志」的结果，用于日志卡片的可见反馈（本项目无 Toast 机制，故内联呈现）。
static LOG_CLEAR_RESULT: Mutex<Option<bool>> = Mutex::new(None);

/// General Tab「关于卡片」下方区域的几何：日志卡片、清理按钮与调试复选框。
/// 渲染、命中测试与点击处理共用本函数，确保三者坐标完全一致。
#[derive(Debug, Clone, Copy)]
struct GeneralFooterLayout {
    log_card: RECT,
    clear_button: RECT,
    debug_checkbox: RECT,
}

/// 计算 General Tab 底部布局。`client_bottom` 为窗口客户区底边（与底部按钮同源）。
fn general_footer_layout(
    pad_x: i32,
    card_w: i32,
    card5_bottom: i32,
    client_bottom: i32,
    scale: f32,
) -> GeneralFooterLayout {
    let px = |v: f32| (v * scale).round() as i32;

    let btn_top = client_bottom - px(22.0) - px(34.0);

    // 日志卡片：紧贴关于卡片下方
    let log_top = card5_bottom + px(8.0);
    let log_card = RECT {
        left: pad_x,
        top: log_top,
        right: pad_x + card_w,
        bottom: log_top + px(48.0),
    };

    // 「清理日志」按钮：日志卡片右侧
    let clear_right = log_card.right - px(16.0);
    let clear_cy = log_top + px(48.0) / 2;
    let clear_button = RECT {
        left: clear_right - px(96.0),
        top: clear_cy - px(28.0) / 2,
        right: clear_right,
        bottom: clear_cy + px(28.0) / 2,
    };

    // 调试复选框：日志卡片与底部按钮之间的剩余空间居中
    let debug_cy = log_card.bottom + (btn_top - log_card.bottom) / 2;
    let debug_checkbox = RECT {
        left: pad_x,
        top: debug_cy - px(26.0) / 2,
        right: pad_x + px(320.0),
        bottom: debug_cy + px(26.0) / 2,
    };

    GeneralFooterLayout {
        log_card,
        clear_button,
        debug_checkbox,
    }
}

/// 点是否落在矩形内（闭区间，与既有 `>= <=` 命中语义一致）。
fn rect_contains(rect: &RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom
}

/// General Tab 底部区域的命中测试（纯函数）：返回悬停 id。
/// 清理按钮在日志卡片之内，故必须先判按钮再判卡片。
fn general_footer_hit(x: i32, y: i32, layout: &GeneralFooterLayout) -> Option<usize> {
    if rect_contains(&layout.clear_button, x, y) {
        Some(HOVER_ID_LOG_CLEAR)
    } else if rect_contains(&layout.log_card, x, y) {
        Some(HOVER_ID_LOG_CARD)
    } else if rect_contains(&layout.debug_checkbox, x, y) {
        Some(HOVER_ID_DEBUG_CHECKBOX)
    } else {
        None
    }
}

/// i18n 文本缺失（空串）时回退到内置默认，避免卡片文案塌陷。
fn i18n_or_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
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
                    let _ = SendMessageW(
                        h,
                        WM_SETICON,
                        Some(WPARAM(ICON_SMALL as _)),
                        Some(LPARAM(hicon_sm.0 as _)),
                    );
                    let _ = SendMessageW(
                        h,
                        WM_SETICON,
                        Some(WPARAM(ICON_BIG as _)),
                        Some(LPARAM(hicon_big.0 as _)),
                    );
                    h
                }
                _ => return Err("创建设置中心窗口失败".to_string()),
            };

            // 获取当前显示器 DPI 并自适应缩放尺寸
            let dpi = GetDpiForWindow(hwnd);
            let scale = if dpi == 0 {
                1.0
            } else {
                (dpi as f32 / 96.0).max(1.0)
            };
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

    pub fn show(&self, current: AppConfig) {
        crate::dialog::detector::set_debug_mode_enabled(false);
        set_draft_config(current);
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
        // 重置上次「清理日志」结果，避免下次打开仍显示旧反馈
        if let Ok(mut result) = LOG_CLEAR_RESULT.lock() {
            *result = None;
        }
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

                let _ = BitBlt(
                    hdc,
                    0,
                    0,
                    rect.right,
                    rect.bottom,
                    Some(mem_dc),
                    0,
                    0,
                    SRCCOPY,
                );

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
                    let is_win = unsafe { GetKeyState(0x5B) < 0 || GetKeyState(0x5C) < 0 };

                    let mut combo_parts = Vec::new();
                    if is_ctrl {
                        combo_parts.push("Ctrl");
                    }
                    if is_alt {
                        combo_parts.push("Alt");
                    }
                    if is_shift {
                        combo_parts.push("Shift");
                    }
                    if is_win {
                        combo_parts.push("Win");
                    }

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
        let scale = if dpi == 0 {
            1.0
        } else {
            (dpi as f32 / 96.0).max(1.0)
        };

        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);

        // Fluent 深邃暗夜背景 #1c1c1c
        let bg_brush = CreateSolidBrush(COLORREF(0x001c1c1c));
        FillRect(hdc, &rect, bg_brush);
        let _ = DeleteObject(HGDIOBJ(bg_brush.0 as _));

        SetBkMode(hdc, TRANSPARENT);

        let font_h1 = CreateFontW(
            (-19.0 * scale).round() as i32,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_card_title = CreateFontW(
            (-13.5 * scale).round() as i32,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_text = CreateFontW(
            (-11.5 * scale).round() as i32,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );
        let font_small = CreateFontW(
            (-10.5 * scale).round() as i32,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
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

        let current_tab = CURRENT_TAB
            .lock()
            .map(|t| *t)
            .unwrap_or(SettingsTab::General);
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
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut title_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

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
        DrawTextW(
            hdc,
            &mut sub_buf,
            &mut sub_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        // 顶部 Tab 选项卡栏（几何与命中测试共享 `settings_tab_geometry`）
        let tab_geo = settings_tab_geometry(scale);
        let tab_y = tab_geo.top;
        let tab_h = tab_geo.height;
        let label_tabs = if bundle.meta.code.starts_with("zh") {
            "多标签增强"
        } else {
            "Tab Utility"
        };
        let integration_texts = I18n::integration_texts(&config.language);
        render_segmented_tabs(
            hdc,
            pad_x,
            &tab_geo,
            scale,
            current_tab,
            hovered,
            font_card_title,
            [
                &bundle.monitor.tab_general,
                label_tabs,
                &bundle.monitor.tab_monitor,
                &integration_texts.tab_title,
            ],
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
                    hdc,
                    pad_x,
                    card0_top,
                    pad_x + card_w,
                    card0_top + card_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.settings.auto_switch,
                    &bundle.settings.auto_switch_desc,
                    config.auto_switch_enabled,
                    &bundle.settings.status_enabled,
                    &bundle.settings.status_disabled,
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
                    hdc,
                    pad_x,
                    card1_top,
                    pad_x + card_w,
                    card1_top + card_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.hotkey.card_title,
                    &bundle.hotkey.card_desc,
                    &card1_s,
                    is_recording,
                    is_hov_1,
                );

                // 卡片 2：开机静默自启 (Autostart) - Toggle 开关滑块
                let card2_top = card1_top + card_h + card_gap;
                let is_hov_2 = hovered == Some(2);
                render_toggle_card(
                    hdc,
                    pad_x,
                    card2_top,
                    pad_x + card_w,
                    card2_top + card_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.settings.autostart,
                    &bundle.settings.autostart_desc,
                    config.autostart_enabled,
                    &bundle.settings.status_enabled,
                    &bundle.settings.status_disabled,
                    is_hov_2,
                );

                // 卡片 3：弹出层半透明效果 (Opacity) - 调节 Slider 滑块
                let card3_top = card2_top + card_h + card_gap;
                let is_hov_3 = hovered == Some(3);
                render_slider_card(
                    hdc,
                    pad_x,
                    card3_top,
                    pad_x + card_w,
                    card3_top + card_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.settings.opacity,
                    &bundle.settings.opacity_desc,
                    config.floating_bar_opacity,
                    is_hov_3,
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
                    hdc,
                    pad_x,
                    card4_top,
                    pad_x + card_w,
                    card4_top + card_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.settings.language,
                    &bundle.settings.language_desc,
                    lang_name,
                    is_hov_4,
                    is_dropdown_open,
                );

                // 卡片 5：“关于 QuickPath”现代专属信息卡片 (About Card)
                let card5_top = card4_top + card_h + card_gap + (2.0 * scale).round() as i32;
                let about_h = (120.0 * scale).round() as i32;
                render_about_card(
                    hdc,
                    pad_x,
                    card5_top,
                    pad_x + card_w,
                    card5_top + about_h,
                    scale,
                    bundle,
                    font_card_title,
                    font_text,
                    font_small,
                    hovered,
                );

                // 卡片 5 下方：日志卡片（展示日志路径与清理入口）与排错调试复选框
                let card5_bottom = card5_top + about_h;
                let footer = general_footer_layout(pad_x, card_w, card5_bottom, rect.bottom, scale);
                let is_chinese = bundle.meta.code.starts_with("zh");
                let log_path_text = crate::dialog::detector::debug_log_path_display()
                    .unwrap_or_else(|| {
                        i18n_or_fallback(
                            &bundle.settings.log_none,
                            if is_chinese {
                                "日志不可用"
                            } else {
                                "Log unavailable"
                            },
                        )
                        .to_string()
                    });
                let (log_status, log_status_ok) =
                    match LOG_CLEAR_RESULT.lock().ok().and_then(|r| *r) {
                        Some(true) => (
                            i18n_or_fallback(
                                &bundle.settings.log_cleared,
                                if is_chinese { "已清理" } else { "Cleared" },
                            ),
                            true,
                        ),
                        Some(false) => (
                            i18n_or_fallback(
                                &bundle.settings.log_clear_failed,
                                if is_chinese {
                                    "清理失败"
                                } else {
                                    "Clear failed"
                                },
                            ),
                            false,
                        ),
                        None => ("", true),
                    };
                render_log_card(
                    hdc,
                    &footer.log_card,
                    &footer.clear_button,
                    scale,
                    font_card_title,
                    font_text,
                    font_small,
                    i18n_or_fallback(
                        &bundle.settings.log_title,
                        if is_chinese { "日志" } else { "Log" },
                    ),
                    &log_path_text,
                    i18n_or_fallback(
                        &bundle.settings.log_clear,
                        if is_chinese {
                            "清理日志"
                        } else {
                            "Clear log"
                        },
                    ),
                    log_status,
                    log_status_ok,
                    hovered == Some(HOVER_ID_LOG_CARD) || hovered == Some(HOVER_ID_LOG_CLEAR),
                    hovered == Some(HOVER_ID_LOG_CLEAR),
                );

                let is_debug = crate::dialog::detector::is_debug_mode_enabled();
                let is_hov_debug = hovered == Some(HOVER_ID_DEBUG_CHECKBOX);
                draw_checkbox_item(
                    hdc,
                    footer.debug_checkbox.left,
                    footer.debug_checkbox.top,
                    footer.debug_checkbox.right,
                    footer.debug_checkbox.bottom,
                    scale,
                    font_text,
                    &bundle.settings.debug_mode,
                    is_debug,
                    is_hov_debug,
                );
            }
            SettingsTab::TabModule => {
                if !crate::win32::version::is_tab_module_supported() {
                    let bottom_pad = (22.0 * scale).round() as i32;
                    let btn_h = (34.0 * scale).round() as i32;
                    let btn_top = rect.bottom - bottom_pad - btn_h;
                    render_tab_unsupported_view(
                        hdc,
                        pad_x,
                        card_top_base,
                        pad_x + card_w,
                        btn_top,
                        scale,
                        bundle,
                        font_card_title,
                        font_text,
                        font_small,
                    );
                } else {
                    // 卡片 0：启用多标签增强 (Tab Utility) - 独立置顶总开关卡片（参考任务栏监控）
                    let card0_top = card_top_base;
                    let card0_h = (56.0 * scale).round() as i32;
                    let is_hov_0 = hovered == Some(60);
                    render_toggle_card(
                        hdc,
                        pad_x,
                        card0_top,
                        pad_x + card_w,
                        card0_top + card0_h,
                        scale,
                        font_card_title,
                        font_text,
                        &bundle.tab_module.enable_tab_module,
                        &bundle.tab_module.enable_tab_module_desc,
                        config.tab_module.enabled,
                        &bundle.settings.status_enabled,
                        &bundle.settings.status_disabled,
                        is_hov_0,
                    );

                    // 卡片 1：快捷键与窗口接管 (Card T1: Shortcuts & Window Integration)
                    let card1_top = card0_top + card0_h + card_gap;
                    let card1_h = (90.0 * scale).round() as i32;
                    let shortcuts_items = [(
                        bundle.tab_module.wine_to_tab.as_str(),
                        bundle.tab_module.wine_to_tab_desc.as_str(),
                        config.tab_module.wine_to_tab,
                        61usize,
                    )];
                    render_tab_group_card(
                        hdc,
                        pad_x,
                        card1_top,
                        pad_x + card_w,
                        card1_top + card1_h,
                        scale,
                        font_card_title,
                        font_text,
                        &bundle.tab_module.shortcuts_group_title,
                        &bundle.tab_module.shortcuts_group_desc,
                        &shortcuts_items,
                        config.tab_module.enabled,
                        hovered,
                        &bundle.settings.status_enabled,
                        &bundle.settings.status_disabled,
                    );

                    // 卡片 2：智能行为与鼠标手势 (Card T2: Behaviors & Mouse Gestures)
                    let card2_top = card1_top + card1_h + card_gap;
                    let card2_h = (134.0 * scale).round() as i32;
                    let behaviors_items = [
                        (
                            bundle.tab_module.tab_deduplication.as_str(),
                            bundle.tab_module.tab_deduplication_desc.as_str(),
                            config.tab_module.deduplication,
                            63usize,
                        ),
                        (
                            bundle.tab_module.double_click_duplicate.as_str(),
                            bundle.tab_module.double_click_duplicate_desc.as_str(),
                            config.tab_module.double_click_to_duplicate,
                            64usize,
                        ),
                    ];
                    render_tab_group_card(
                        hdc,
                        pad_x,
                        card2_top,
                        pad_x + card_w,
                        card2_top + card2_h,
                        scale,
                        font_card_title,
                        font_text,
                        &bundle.tab_module.behaviors_group_title,
                        &bundle.tab_module.behaviors_group_desc,
                        &behaviors_items,
                        config.tab_module.enabled,
                        hovered,
                        &bundle.settings.status_enabled,
                        &bundle.settings.status_disabled,
                    );

                    // 卡片 3：操作指南与实用技巧 (Card T3: High-Efficiency Tips Card)
                    let card3_top = card2_top + card2_h + card_gap;
                    let card3_h = (68.0 * scale).round() as i32;
                    render_tips_card(
                        hdc,
                        pad_x,
                        card3_top,
                        pad_x + card_w,
                        card3_top + card3_h,
                        scale,
                        font_card_title,
                        font_small,
                        &bundle.tab_module.tips_title,
                        &[
                            bundle.tab_module.tips_line1.as_str(),
                            bundle.tab_module.tips_line2.as_str(),
                        ],
                        config.tab_module.enabled,
                    );
                }
            }
            SettingsTab::Monitor => {
                let card_m0_top = card_top_base;
                let card_m0_h = (56.0 * scale).round() as i32;
                let is_hov_m0 = hovered == Some(30);
                render_toggle_card(
                    hdc,
                    pad_x,
                    card_m0_top,
                    pad_x + card_w,
                    card_m0_top + card_m0_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.monitor.master_switch,
                    &bundle.monitor.master_switch_desc,
                    config.monitor.enabled,
                    &bundle.settings.status_enabled,
                    &bundle.settings.status_disabled,
                    is_hov_m0,
                );

                let card_m1_top = card_m0_top + card_m0_h + card_gap;
                let card_m1_h = (140.0 * scale).round() as i32;
                render_metrics_checklist_card(
                    hdc,
                    pad_x,
                    card_m1_top,
                    pad_x + card_w,
                    card_m1_top + card_m1_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.monitor.metrics_title,
                    &bundle.monitor.metrics_desc,
                    bundle,
                    &config.monitor,
                    hovered,
                );

                let card_m2_top = card_m1_top + card_m1_h + card_gap;
                let card_m2_h = (182.0 * scale).round() as i32;
                render_monitor_visual_card(
                    hdc,
                    pad_x,
                    card_m2_top,
                    pad_x + card_w,
                    card_m2_top + card_m2_h,
                    scale,
                    font_card_title,
                    font_text,
                    &bundle.monitor.visual_title,
                    &bundle.monitor.visual_desc,
                    bundle,
                    &config.monitor,
                    hovered,
                );

                let card_m3_top = card_m2_top + card_m2_h + card_gap;
                let card_m3_h = (68.0 * scale).round() as i32;
                render_monitor_tips_card(
                    hdc,
                    pad_x,
                    card_m3_top,
                    pad_x + card_w,
                    card_m3_top + card_m3_h,
                    scale,
                    font_card_title,
                    font_small,
                    bundle,
                );
            }
            SettingsTab::Integration => {
                // 审计报告 U-01：开关只表示「已授权」，必须配合手动触发才能探测。
                let texts = I18n::integration_texts(&config.language);
                let card_h = (56.0 * scale).round() as i32;

                let card_i0_top = card_top_base;
                render_toggle_card(
                    hdc,
                    pad_x,
                    card_i0_top,
                    pad_x + card_w,
                    card_i0_top + card_h,
                    scale,
                    font_card_title,
                    font_text,
                    &texts.totalcmd_switch,
                    &texts.totalcmd_desc,
                    config.enable_totalcmd,
                    &bundle.settings.status_enabled,
                    &bundle.settings.status_disabled,
                    hovered == Some(HOVER_ID_INTEGRATION_TC),
                );

                let card_i1_top = card_i0_top + card_h + card_gap;
                render_toggle_card(
                    hdc,
                    pad_x,
                    card_i1_top,
                    pad_x + card_w,
                    card_i1_top + card_h,
                    scale,
                    font_card_title,
                    font_text,
                    &texts.xyplorer_switch,
                    &texts.xyplorer_desc,
                    config.enable_xyplorer,
                    &bundle.settings.status_enabled,
                    &bundle.settings.status_disabled,
                    hovered == Some(HOVER_ID_INTEGRATION_XY),
                );

                // 风险与使用方式提示（报告 §2.3 第 2 条：开关旁必须明确提示）
                let card_i2_top = card_i1_top + card_h + card_gap;
                let card_i2_h = (92.0 * scale).round() as i32;
                render_tips_card(
                    hdc,
                    pad_x,
                    card_i2_top,
                    pad_x + card_w,
                    card_i2_top + card_i2_h,
                    scale,
                    font_card_title,
                    font_small,
                    &texts.tips_title,
                    &[
                        texts.tips_line1.as_str(),
                        texts.tips_line2.as_str(),
                        texts.tips_line3.as_str(),
                    ],
                    config.enable_totalcmd || config.enable_xyplorer,
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
        let ok_bg = if is_hov_ok {
            COLORREF(0x00e88410)
        } else {
            COLORREF(0x00d47800)
        };
        render_rounded_button(
            hdc,
            &btn_ok_rect,
            &bundle.settings.btn_ok,
            font_card_title,
            ok_bg,
            COLORREF(0x00ffffff),
            None,
            (4.0 * scale).round() as i32,
        );

        // 取消按钮：精致深灰面板，带微边框
        let cancel_bg = if is_hov_cancel {
            COLORREF(0x00383838)
        } else {
            COLORREF(0x002c2c2c)
        };
        let cancel_border = if is_hov_cancel {
            COLORREF(0x005c5c5c)
        } else {
            COLORREF(0x00444444)
        };
        render_rounded_button(
            hdc,
            &btn_cancel_rect,
            &bundle.settings.btn_cancel,
            font_card_title,
            cancel_bg,
            COLORREF(0x00e2e2e2),
            Some(cancel_border),
            (4.0 * scale).round() as i32,
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
        DrawTextW(
            hdc,
            &mut ver_buf,
            &mut ver_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );

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
                hdc,
                combo_left,
                combo_bottom,
                combo_w,
                scale,
                config.language.as_code(),
                font_text,
                hov_item,
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
        draw_fluent_box(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius,
            bg_color,
            bc,
        );

        SelectObject(hdc, HGDIOBJ(font.0 as _));
        SetTextColor(hdc, text_color);
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let mut r = *rect;
        DrawTextW(
            hdc,
            &mut buf,
            &mut r,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
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
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut title_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, COLORREF(0x00909090));
        let mut desc_buf: Vec<u16> = desc.encode_utf16().collect();
        let mut desc_rect = RECT {
            left: inner_x,
            top: top + (29.0 * scale).round() as i32,
            right: right_limit,
            bottom: bottom - (6.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut desc_buf,
            &mut desc_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
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
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            bg,
            border,
        );

        let sw_w = (46.0 * scale).round() as i32;
        let sw_h = (24.0 * scale).round() as i32;
        let right_pad = (18.0 * scale).round() as i32;
        let sw_right = right - right_pad;
        let sw_left = sw_right - sw_w;
        let sw_cy = top + (bottom - top) / 2;
        let sw_top = sw_cy - sw_h / 2;
        let sw_bottom = sw_top + sw_h;

        let right_limit = sw_left - (80.0 * scale).round() as i32;
        render_card_texts(
            hdc,
            left,
            top,
            right_limit,
            bottom,
            scale,
            font_title,
            font_desc,
            title,
            desc,
        );

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
            top,
            right: sw_left - (10.0 * scale).round() as i32,
            bottom,
        };
        DrawTextW(
            hdc,
            &mut status_buf,
            &mut status_rect,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
        );

        // 绘制 Toggle Switch 胶囊滑块槽体
        let (sw_bg, sw_border) = if is_on {
            (COLORREF(0x00d47800), COLORREF(0x00d47800)) // Accent Blue
        } else {
            (COLORREF(0x00323232), COLORREF(0x00525252)) // 灰底
        };
        draw_fluent_box(
            hdc, sw_left, sw_top, sw_right, sw_bottom, sw_h, sw_bg, sw_border,
        );

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
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            bg,
            border,
        );

        let badge_w = (180.0 * scale).round() as i32;
        let right_limit = right - badge_w - (16.0 * scale).round() as i32;
        render_card_texts(
            hdc,
            left,
            top,
            right_limit,
            bottom,
            scale,
            font_title,
            font_desc,
            title,
            desc,
        );

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
            (
                COLORREF(0x001f3045),
                COLORREF(0x0000a5ff),
                COLORREF(0x0000a5ff),
            ) // 录制橙/亮蓝
        } else {
            (
                COLORREF(0x002c2c2c),
                COLORREF(0x00d47800),
                COLORREF(0x0050d268),
            ) // 正常常态
        };

        draw_fluent_box(
            hdc,
            badge_rect.left,
            badge_rect.top,
            badge_rect.right,
            badge_rect.bottom,
            (4.0 * scale).round() as i32,
            badge_bg,
            badge_border,
        );

        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(hdc, text_color);
        let mut buf: Vec<u16> = badge_text.encode_utf16().collect();
        let mut r = badge_rect;
        DrawTextW(
            hdc,
            &mut buf,
            &mut r,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
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
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            bg,
            border,
        );

        let slider_w = (140.0 * scale).round() as i32;
        let right_pad = (18.0 * scale).round() as i32;
        let track_right = right - right_pad;
        let track_left = track_right - slider_w;
        let right_limit = track_left - (20.0 * scale).round() as i32;

        render_card_texts(
            hdc,
            left,
            top,
            right_limit,
            bottom,
            scale,
            font_title,
            font_desc,
            title,
            desc,
        );

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
        DrawTextW(
            hdc,
            &mut val_buf,
            &mut val_rect,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
        );

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
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            bg,
            border,
        );

        let combo_w = (170.0 * scale).round() as i32;
        let combo_h = (30.0 * scale).round() as i32;
        let right_pad = (16.0 * scale).round() as i32;
        let combo_right = right - right_pad;
        let combo_left = combo_right - combo_w;
        let combo_cy = top + (bottom - top) / 2;
        let combo_top = combo_cy - combo_h / 2;
        let combo_bottom = combo_top + combo_h;

        let right_limit = combo_left - (16.0 * scale).round() as i32;
        render_card_texts(
            hdc,
            left,
            top,
            right_limit,
            bottom,
            scale,
            font_title,
            font_desc,
            title,
            desc,
        );

        // 下拉框背景与描边：展开时边框呈现 Accent Blue 激活高亮
        let combo_bg = COLORREF(0x002b2b2b);
        let combo_border = if is_open {
            COLORREF(0x00d47800) // 激活亮蓝
        } else if is_hovered {
            COLORREF(0x005c5c5c)
        } else {
            COLORREF(0x00444444)
        };
        draw_fluent_box(
            hdc,
            combo_left,
            combo_top,
            combo_right,
            combo_bottom,
            (4.0 * scale).round() as i32,
            combo_bg,
            combo_border,
        );

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
        DrawTextW(
            hdc,
            &mut lang_buf,
            &mut lang_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );

        // 下拉小三角：展开时为 ▲，收起时为 ▼
        SetTextColor(
            hdc,
            if is_open {
                COLORREF(0x00d47800)
            } else {
                COLORREF(0x00999999)
            },
        );
        let arrow_char = if is_open { "▲" } else { "▼" };
        let mut arrow_buf: Vec<u16> = arrow_char.encode_utf16().collect();
        let mut arrow_rect = RECT {
            left: combo_right - (22.0 * scale).round() as i32,
            top: combo_top,
            right: combo_right - (8.0 * scale).round() as i32,
            bottom: combo_bottom,
        };
        DrawTextW(
            hdc,
            &mut arrow_buf,
            &mut arrow_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
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
                draw_fluent_box(
                    hdc,
                    it_left,
                    it_top,
                    it_right,
                    it_bottom,
                    (4.0 * scale).round() as i32,
                    hover_bg,
                    hover_bg,
                );
            } else if is_selected {
                let sel_bg = COLORREF(0x002a2a2a);
                draw_fluent_box(
                    hdc,
                    it_left,
                    it_top,
                    it_right,
                    it_bottom,
                    (4.0 * scale).round() as i32,
                    sel_bg,
                    sel_bg,
                );
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
/// 渲染日志卡片：标题 + 日志路径 + 「清理日志」按钮 + 清理结果状态。
/// 几何由 `general_footer_layout` 统一给出，与命中测试/点击处理共用。
unsafe fn render_log_card(
    hdc: HDC,
    card: &RECT,
    clear_button: &RECT,
    scale: f32,
    font_title: windows::Win32::Graphics::Gdi::HFONT,
    font_text: windows::Win32::Graphics::Gdi::HFONT,
    font_small: windows::Win32::Graphics::Gdi::HFONT,
    title: &str,
    path_text: &str,
    clear_label: &str,
    status_text: &str,
    status_ok: bool,
    is_card_hovered: bool,
    is_button_hovered: bool,
) {
    unsafe {
        let (bg, border) = if is_card_hovered {
            (COLORREF(0x002b2b2b), COLORREF(0x004c4c4c))
        } else {
            (COLORREF(0x00242424), COLORREF(0x00353535))
        };
        draw_fluent_box(
            hdc,
            card.left,
            card.top,
            card.right,
            card.bottom,
            (8.0 * scale).round() as i32,
            bg,
            border,
        );

        let inner_x = card.left + (16.0 * scale).round() as i32;
        let text_right = clear_button.left - (12.0 * scale).round() as i32;

        // 标题行（右侧留给清理结果状态）
        SelectObject(hdc, HGDIOBJ(font_title.0 as _));
        SetTextColor(hdc, COLORREF(0x00f3f3f3));
        let mut title_buf: Vec<u16> = title.encode_utf16().collect();
        let mut title_rect = RECT {
            left: inner_x,
            top: card.top + (6.0 * scale).round() as i32,
            right: text_right,
            bottom: card.top + (25.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut title_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        // 日志路径（小字，超长省略，避免撑破卡片）
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x00909090));
        let mut path_buf: Vec<u16> = path_text.encode_utf16().collect();
        let mut path_rect = RECT {
            left: inner_x,
            top: card.top + (26.0 * scale).round() as i32,
            right: text_right,
            bottom: card.bottom - (5.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut path_buf,
            &mut path_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );

        // 清理结果状态（标题行右侧、按钮左侧）
        if !status_text.is_empty() {
            SetTextColor(
                hdc,
                if status_ok {
                    COLORREF(0x0050d268)
                } else {
                    COLORREF(0x006868ff)
                },
            );
            let mut status_buf: Vec<u16> = status_text.encode_utf16().collect();
            let mut status_rect = RECT {
                left: clear_button.left - (120.0 * scale).round() as i32,
                top: card.top + (6.0 * scale).round() as i32,
                right: clear_button.left - (12.0 * scale).round() as i32,
                bottom: card.top + (25.0 * scale).round() as i32,
            };
            DrawTextW(
                hdc,
                &mut status_buf,
                &mut status_rect,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }

        // 「清理日志」按钮
        let btn_bg = if is_button_hovered {
            COLORREF(0x00444444)
        } else {
            COLORREF(0x00323232)
        };
        let btn_border = if is_button_hovered {
            COLORREF(0x00666666)
        } else {
            COLORREF(0x00484848)
        };
        render_rounded_button(
            hdc,
            clear_button,
            clear_label,
            font_text,
            btn_bg,
            COLORREF(0x00e8e8e8),
            Some(btn_border),
            (4.0 * scale).round() as i32,
        );
    }
}

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
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            COLORREF(0x00212121),
            COLORREF(0x00343434),
        );

        // 左侧 Logo 图标：放大显示 logo.png，占满所在卡片高度的 80%
        let card_h = bottom - top;
        let logo_h = ((card_h as f32) * 0.8).round() as i32;
        let logo_w = logo_h; // 正方形原图保持等比例
        let logo_x = left + (16.0 * scale).round() as i32;
        let logo_y = top + (card_h - logo_h) / 2;

        if !crate::win32::icon::draw_logo_png(hdc, logo_x, logo_y, logo_w, logo_h) {
            let hicon = crate::win32::icon::get_app_icon(false);
            let _ = DrawIconEx(
                hdc, logo_x, logo_y, hicon, logo_w, logo_h, 0, None, DI_NORMAL,
            );
        }

        let text_x = logo_x + logo_w + (18.0 * scale).round() as i32;
        let text_right = right - (16.0 * scale).round() as i32;

        // 行 1: QuickPath + 动态版本徽标
        SelectObject(hdc, HGDIOBJ(font_bold.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let ver_name = if bundle.about.version.is_empty() {
            format!(
                "v{} 正式版 (原生极速 · 极简轻量)",
                crate::rules::version::APP_VERSION
            )
        } else {
            bundle.about.version.clone()
        };
        let line1 = format!("QuickPath  {}", ver_name);
        let mut buf1: Vec<u16> = line1.encode_utf16().collect();
        let mut r1 = RECT {
            left: text_x,
            top: top + (10.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (28.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut buf1,
            &mut r1,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );

        // 行 2: 运行系统环境（当前 Windows 具体版本）
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x00bbbbbb));
        let is_chinese = bundle.meta.code.starts_with("zh");
        let os_ver_str = crate::win32::version::format_windows_version(is_chinese);
        let line_os = if !bundle.about.os_environment.is_empty() {
            bundle
                .about
                .os_environment
                .replace("{os_version}", &os_ver_str)
        } else if is_chinese {
            format!("系统环境：{}", os_ver_str)
        } else {
            format!("System: {}", os_ver_str)
        };
        let mut buf_os: Vec<u16> = line_os.encode_utf16().collect();
        let mut r_os = RECT {
            left: text_x,
            top: top + (31.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (48.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut buf_os,
            &mut r_os,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );

        // 行 3: 出品方及官网链接（衢州御风科技有限公司 · qp.yftec.top ↗）
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
            top: top + (51.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (69.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut buf2,
            &mut r2,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );

        // 行 4: GitHub 官方开源地址（GitHub 纯白高清图标 + github.com/clhome/QuickPath ↗）
        let gh_size = (15.0 * scale).round() as i32;
        let gh_y = top + (72.0 * scale).round() as i32 + ((18.0 * scale) as i32 - gh_size) / 2;
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
            top: top + (72.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (90.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut buf3,
            &mut r3,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );

        // 行 5: 版权声明
        SetTextColor(hdc, COLORREF(0x00666666));
        let copyright_text = if bundle.about.copyright.is_empty() {
            "Copyright © 2026 衢州御风科技有限公司. All Rights Reserved."
        } else {
            &bundle.about.copyright
        };
        let mut buf4: Vec<u16> = copyright_text.encode_utf16().collect();
        let mut r4 = RECT {
            left: text_x,
            top: top + (93.0 * scale).round() as i32,
            right: text_right,
            bottom: top + (110.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut buf4,
            &mut r4,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// 当操作系统版本不满足资源管理器多标签增强基准时，在选项卡内容区居中渲染提示信息
unsafe fn render_tab_unsupported_view(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f32,
    bundle: &crate::rules::LocaleBundle,
    font_title: windows::Win32::Graphics::Gdi::HFONT,
    font_text: windows::Win32::Graphics::Gdi::HFONT,
    font_small: windows::Win32::Graphics::Gdi::HFONT,
) {
    unsafe {
        let area_w = right - left;
        let area_h = bottom - top;

        // 适度加宽卡片（如 520px），并预留足够内边距，确保中英文无论单行还是折行都呼吸舒展
        let box_w = ((520.0 * scale).round() as i32).min(area_w - (16.0 * scale).round() as i32);
        let pad_inner_x = (24.0 * scale).round() as i32;
        let text_w = box_w - pad_inner_x * 2;

        // 1. 顶部提示胶囊徽标 (Badge) - 动态测量文本宽度，中英文均完美自适应
        let badge_h = (24.0 * scale).round() as i32;
        let badge_text = if bundle.tab_module.unsupported_badge.is_empty() {
            "系统环境限制"
        } else {
            &bundle.tab_module.unsupported_badge
        };
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        let mut badge_buf: Vec<u16> = badge_text.encode_utf16().collect();
        let mut r_badge_calc = RECT::default();
        DrawTextW(
            hdc,
            &mut badge_buf,
            &mut r_badge_calc,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
        let badge_txt_w = r_badge_calc.right - r_badge_calc.left;
        let badge_pad_x = (14.0 * scale).round() as i32;
        let badge_w = (badge_txt_w + badge_pad_x * 2).max((100.0 * scale).round() as i32);

        // 2. 主标题 - 预先测量折行所需高度 (DT_CALCRECT | DT_WORDBREAK)
        SelectObject(hdc, HGDIOBJ(font_title.0 as _));
        let title_text = if bundle.tab_module.unsupported_title.is_empty() {
            "当前 Windows 系统版本暂不支持多标签增强"
        } else {
            &bundle.tab_module.unsupported_title
        };
        let mut title_buf: Vec<u16> = title_text.encode_utf16().collect();
        let mut r_title_calc = RECT {
            left: 0,
            top: 0,
            right: text_w,
            bottom: 0,
        };
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut r_title_calc,
            DT_CALCRECT | DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );
        let title_h = (r_title_calc.bottom - r_title_calc.top).max((24.0 * scale).round() as i32);

        // 3. 详细说明与当前系统版本提示 - 预先测量自动换行折行所需高度
        SelectObject(hdc, HGDIOBJ(font_text.0 as _));
        let is_chinese = bundle.meta.code.starts_with("zh");
        let os_str = crate::win32::version::format_windows_version(is_chinese);
        let desc_text = if !bundle.tab_module.unsupported_desc.is_empty() {
            bundle
                .tab_module
                .unsupported_desc
                .replace("{os_version}", &os_str)
        } else {
            format!(
                "文件资源管理器多标签增强需要 Windows 11 22H2 (Build 22621) 或更高版本 原生标签架构支持。\n当前检测到您的操作系统为：{}",
                os_str
            )
        };
        let mut desc_buf: Vec<u16> = desc_text.encode_utf16().collect();
        let mut r_desc_calc = RECT {
            left: 0,
            top: 0,
            right: text_w,
            bottom: 0,
        };
        DrawTextW(
            hdc,
            &mut desc_buf,
            &mut r_desc_calc,
            DT_CALCRECT | DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );
        let desc_h = (r_desc_calc.bottom - r_desc_calc.top).max((36.0 * scale).round() as i32);

        // 4. 根据实际测量内容自适应计算卡片整体尺寸与垂直居中坐标
        let top_pad = (22.0 * scale).round() as i32;
        let gap_badge_title = (14.0 * scale).round() as i32;
        let gap_title_desc = (14.0 * scale).round() as i32;
        let bottom_pad = (22.0 * scale).round() as i32;

        let box_h =
            (top_pad + badge_h + gap_badge_title + title_h + gap_title_desc + desc_h + bottom_pad)
                .min(area_h - (16.0 * scale).round() as i32);

        let box_left = left + (area_w - box_w) / 2;
        let box_top = top + (area_h - box_h) / 2;
        let box_right = box_left + box_w;
        let box_bottom = box_top + box_h;

        // 渲染外层 Fluent 居中卡片容器
        draw_fluent_box(
            hdc,
            box_left,
            box_top,
            box_right,
            box_bottom,
            (10.0 * scale).round() as i32,
            COLORREF(0x00212121),
            COLORREF(0x00363636),
        );

        // 渲染 1: 顶部提示胶囊徽标 (Badge)
        let badge_x = box_left + (box_w - badge_w) / 2;
        let badge_y = box_top + top_pad;
        draw_fluent_box(
            hdc,
            badge_x,
            badge_y,
            badge_x + badge_w,
            badge_y + badge_h,
            (12.0 * scale).round() as i32,
            COLORREF(0x002d261e), // 琥珀深色底
            COLORREF(0x005a442a), // 微亮边框
        );
        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x007ad0ff)); // 醒目琥珀金色文字
        let mut r_badge = RECT {
            left: badge_x,
            top: badge_y,
            right: badge_x + badge_w,
            bottom: badge_y + badge_h,
        };
        DrawTextW(
            hdc,
            &mut badge_buf,
            &mut r_badge,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        // 渲染 2: 居中主标题（DT_WORDBREAK 保证长英文自适应换行）
        SelectObject(hdc, HGDIOBJ(font_title.0 as _));
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let title_y = badge_y + badge_h + gap_badge_title;
        let mut r_title = RECT {
            left: box_left + pad_inner_x,
            top: title_y,
            right: box_right - pad_inner_x,
            bottom: title_y + title_h,
        };
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut r_title,
            DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );

        // 渲染 3: 居中说明与当前系统版本（DT_WORDBREAK 自动折行，中英文全显，绝不截断）
        SelectObject(hdc, HGDIOBJ(font_text.0 as _));
        SetTextColor(hdc, COLORREF(0x009e9e9e));
        let desc_y = title_y + title_h + gap_title_desc;
        let mut r_desc = RECT {
            left: box_left + pad_inner_x,
            top: desc_y,
            right: box_right - pad_inner_x,
            bottom: desc_y + desc_h + (4.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut desc_buf,
            &mut r_desc,
            DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );
    }
}

/// 辅助渲染多标签增强分组设置卡片（专业卡片面板，包含板块标题、说明与多行配置子项）
unsafe fn render_tab_group_card(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f32,
    font_title: windows::Win32::Graphics::Gdi::HFONT,
    font_desc: windows::Win32::Graphics::Gdi::HFONT,
    group_title: &str,
    group_desc: &str,
    items: &[(&str, &str, bool, usize)],
    is_master_enabled: bool,
    hovered: Option<usize>,
    status_on: &str,
    status_off: &str,
) {
    unsafe {
        let (bg, border) = if is_master_enabled {
            (COLORREF(0x00242424), COLORREF(0x00353535))
        } else {
            (COLORREF(0x001e1e1e), COLORREF(0x002a2a2a))
        };
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            bg,
            border,
        );

        let inner_x = left + (16.0 * scale).round() as i32;
        let right_pad = (16.0 * scale).round() as i32;
        let right_content = right - right_pad;

        // 1. 头部区域 (Group Header)
        let header_h = (46.0 * scale).round() as i32;
        let header_bottom = top + header_h;

        SelectObject(hdc, HGDIOBJ(font_title.0 as _));
        SetTextColor(
            hdc,
            if is_master_enabled {
                COLORREF(0x00ffffff)
            } else {
                COLORREF(0x00787878)
            },
        );
        let mut g_title_buf: Vec<u16> = group_title.encode_utf16().collect();
        let mut g_title_rect = RECT {
            left: inner_x,
            top: top + (7.0 * scale).round() as i32,
            right: right_content,
            bottom: top + (25.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut g_title_buf,
            &mut g_title_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
        SetTextColor(
            hdc,
            if is_master_enabled {
                COLORREF(0x008e8e8e)
            } else {
                COLORREF(0x00525252)
            },
        );
        let mut g_desc_buf: Vec<u16> = group_desc.encode_utf16().collect();
        let mut g_desc_rect = RECT {
            left: inner_x,
            top: top + (26.0 * scale).round() as i32,
            right: right_content,
            bottom: header_bottom - (4.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut g_desc_buf,
            &mut g_desc_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        // 头部与子项行之间的轻微分割线
        let sep_brush = CreateSolidBrush(COLORREF(0x002c2c2c));
        let h_line_rect = RECT {
            left: inner_x,
            top: header_bottom - 1,
            right: right_content,
            bottom: header_bottom,
        };
        FillRect(hdc, &h_line_rect, sep_brush);

        // 2. 逐行渲染子项
        let row_h = (44.0 * scale).round() as i32;
        let sw_w = (42.0 * scale).round() as i32;
        let sw_h = (22.0 * scale).round() as i32;

        for (idx, &(item_title, item_desc, is_on, hov_id)) in items.iter().enumerate() {
            let row_top = header_bottom + idx as i32 * row_h;
            let row_bottom = row_top + row_h;

            if idx > 0 {
                let r_line_rect = RECT {
                    left: inner_x,
                    top: row_top - 1,
                    right: right_content,
                    bottom: row_top,
                };
                FillRect(hdc, &r_line_rect, sep_brush);
            }

            let is_hov = is_master_enabled && hovered == Some(hov_id);
            if is_hov {
                let hov_rect = RECT {
                    left: left + (4.0 * scale).round() as i32,
                    top: row_top + 1,
                    right: right - (4.0 * scale).round() as i32,
                    bottom: row_bottom - 1,
                };
                let hov_brush = CreateSolidBrush(COLORREF(0x002c2c2c));
                FillRect(hdc, &hov_rect, hov_brush);
                let _ = DeleteObject(HGDIOBJ(hov_brush.0 as _));
            }

            let sw_right = right - right_pad;
            let sw_left = sw_right - sw_w;
            let sw_cy = row_top + row_h / 2;
            let sw_top = sw_cy - sw_h / 2;
            let sw_bottom = sw_top + sw_h;

            let text_right_limit = sw_left - (72.0 * scale).round() as i32;

            // 标题
            SelectObject(hdc, HGDIOBJ(font_desc.0 as _));
            SetTextColor(
                hdc,
                if is_master_enabled {
                    COLORREF(0x00ededed)
                } else {
                    COLORREF(0x00666666)
                },
            );
            let mut t_buf: Vec<u16> = item_title.encode_utf16().collect();
            let mut t_rect = RECT {
                left: inner_x,
                top: row_top + (5.0 * scale).round() as i32,
                right: text_right_limit,
                bottom: row_top + (23.0 * scale).round() as i32,
            };
            DrawTextW(
                hdc,
                &mut t_buf,
                &mut t_rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );

            // 描述
            SetTextColor(
                hdc,
                if is_master_enabled {
                    COLORREF(0x00888888)
                } else {
                    COLORREF(0x004e4e4e)
                },
            );
            let mut d_buf: Vec<u16> = item_desc.encode_utf16().collect();
            let mut d_rect = RECT {
                left: inner_x,
                top: row_top + (24.0 * scale).round() as i32,
                right: text_right_limit,
                bottom: row_bottom - (4.0 * scale).round() as i32,
            };
            DrawTextW(
                hdc,
                &mut d_buf,
                &mut d_rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );

            // 状态文字提示（开关左侧）
            let (txt, txt_color) = if !is_master_enabled {
                (
                    if is_on { status_on } else { status_off },
                    COLORREF(0x004a4a4a),
                )
            } else if is_on {
                (status_on, COLORREF(0x0050d268))
            } else {
                (status_off, COLORREF(0x00787878))
            };
            SetTextColor(hdc, txt_color);
            let mut status_buf: Vec<u16> = txt.encode_utf16().collect();
            let mut status_rect = RECT {
                left: sw_left - (64.0 * scale).round() as i32,
                top: row_top,
                right: sw_left - (8.0 * scale).round() as i32,
                bottom: row_bottom,
            };
            DrawTextW(
                hdc,
                &mut status_buf,
                &mut status_rect,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
            );

            // Toggle 开关槽体
            let (sw_bg, sw_border) = if !is_master_enabled {
                if is_on {
                    (COLORREF(0x004a3e2e), COLORREF(0x00554838))
                } else {
                    (COLORREF(0x00262626), COLORREF(0x00333333))
                }
            } else if is_on {
                (COLORREF(0x00d47800), COLORREF(0x00d47800)) // Win11 Accent Blue
            } else {
                (COLORREF(0x00323232), COLORREF(0x00525252))
            };
            draw_fluent_box(
                hdc, sw_left, sw_top, sw_right, sw_bottom, sw_h, sw_bg, sw_border,
            );

            // Toggle 滑块 Thumb
            let thumb_margin = (2.0 * scale).round() as i32;
            let thumb_radius = (sw_h - thumb_margin * 2) / 2;
            let thumb_cx = if is_on {
                sw_right - thumb_margin - thumb_radius
            } else {
                sw_left + thumb_margin + thumb_radius
            };

            let thumb_color = if is_master_enabled {
                COLORREF(0x00ffffff)
            } else {
                COLORREF(0x00777777)
            };
            let thumb_brush = CreateSolidBrush(thumb_color);
            let thumb_pen = CreatePen(PS_NULL, 0, COLORREF(0));
            let old_b = SelectObject(hdc, HGDIOBJ(thumb_brush.0 as _));
            let old_p = SelectObject(hdc, HGDIOBJ(thumb_pen.0 as _));
            let _ = RoundRect(
                hdc,
                thumb_cx - thumb_radius,
                sw_cy - thumb_radius,
                thumb_cx + thumb_radius,
                sw_cy + thumb_radius,
                thumb_radius * 2,
                thumb_radius * 2,
            );
            let _ = SelectObject(hdc, old_b);
            let _ = SelectObject(hdc, old_p);
            let _ = DeleteObject(HGDIOBJ(thumb_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(thumb_pen.0 as _));
        }

        let _ = DeleteObject(HGDIOBJ(sep_brush.0 as _));
    }
}

/// 辅助渲染多标签页高效技巧指南卡片 (Tips Card)
/// 渲染底部「提示卡片」：标题 + 若干单行提示（空行自动跳过）。
///
/// 审计报告 U-01 要求开关旁必须有风险提示，因此提示卡改为**通用**实现，
/// Tab 增强页与管理器联动页共用（避免为每个页面各写一份绘制代码）。
unsafe fn render_tips_card(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f32,
    font_bold: windows::Win32::Graphics::Gdi::HFONT,
    font_small: windows::Win32::Graphics::Gdi::HFONT,
    title_text: &str,
    lines: &[&str],
    is_master_enabled: bool,
) {
    unsafe {
        let (bg, border) = if is_master_enabled {
            (COLORREF(0x00212121), COLORREF(0x00343434))
        } else {
            (COLORREF(0x001d1d1d), COLORREF(0x002a2a2a))
        };
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            bg,
            border,
        );

        let inner_x = left + (16.0 * scale).round() as i32;

        SelectObject(hdc, HGDIOBJ(font_bold.0 as _));
        let title_color = if is_master_enabled {
            COLORREF(0x0050d268)
        } else {
            COLORREF(0x003e6845)
        };
        SetTextColor(hdc, title_color);
        let mut title_buf: Vec<u16> = title_text.encode_utf16().collect();
        let mut title_rect = RECT {
            left: inner_x,
            top: top + (8.0 * scale).round() as i32,
            right: right - (16.0 * scale).round() as i32,
            bottom: top + (26.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut title_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        let line_color = if is_master_enabled {
            COLORREF(0x009e9e9e)
        } else {
            COLORREF(0x00555555)
        };
        SetTextColor(hdc, line_color);

        for (idx, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let row_top = top + (28.0 * scale).round() as i32 + idx as i32 * (18.0 * scale) as i32;
            let mut line_buf: Vec<u16> = line.encode_utf16().collect();
            let mut line_rect = RECT {
                left: inner_x,
                top: row_top,
                right: right - (16.0 * scale).round() as i32,
                bottom: row_top + (17.0 * scale).round() as i32,
            };
            DrawTextW(
                hdc,
                &mut line_buf,
                &mut line_rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
    }
}

/// 辅助渲染 Fluent 胶囊 Tab 切换栏
unsafe fn render_segmented_tabs(
    hdc: HDC,
    pad_x: i32,
    geo: &TabGeometry,
    scale: f32,
    current_tab: SettingsTab,
    hovered: Option<usize>,
    font: windows::Win32::Graphics::Gdi::HFONT,
    labels: [&str; 4],
) {
    unsafe {
        for (idx, (tab_type, hov_id)) in SETTINGS_TABS.iter().enumerate() {
            let label = labels[idx];
            let tab_left = settings_tab_left(pad_x, geo, idx);
            let tab_right = tab_left + geo.width;
            let top = geo.top;
            let tab_bottom = top + geo.height;

            let is_active = current_tab == *tab_type;
            let is_hov = hovered == Some(*hov_id);

            let (bg, border, text_color) = if is_active {
                (
                    COLORREF(0x002e2e2e),
                    COLORREF(0x004d4d4d),
                    COLORREF(0x00ffffff),
                )
            } else if is_hov {
                (
                    COLORREF(0x00262626),
                    COLORREF(0x00383838),
                    COLORREF(0x00d0d0d0),
                )
            } else {
                (
                    COLORREF(0x001f1f1f),
                    COLORREF(0x002a2a2a),
                    COLORREF(0x00888888),
                )
            };

            draw_fluent_box(
                hdc,
                tab_left,
                top,
                tab_right,
                tab_bottom,
                (6.0 * scale).round() as i32,
                bg,
                border,
            );

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
            DrawTextW(
                hdc,
                &mut buf,
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
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
            draw_fluent_box(
                hdc,
                left,
                top,
                right,
                bottom,
                (4.0 * scale).round() as i32,
                COLORREF(0x00282828),
                COLORREF(0x003a3a3a),
            );
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

        draw_fluent_box(
            hdc,
            box_left,
            box_top,
            box_right,
            box_bottom,
            (4.0 * scale).round() as i32,
            box_bg,
            box_border,
        );

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
            DrawTextW(
                hdc,
                &mut check_buf,
                &mut check_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }

        SelectObject(hdc, HGDIOBJ(font.0 as _));
        SetTextColor(
            hdc,
            if is_checked {
                COLORREF(0x00f0f0f0)
            } else {
                COLORREF(0x00999999)
            },
        );
        let mut text_buf: Vec<u16> = label.encode_utf16().collect();
        let mut text_rect = RECT {
            left: box_right + (8.0 * scale).round() as i32,
            top,
            right,
            bottom,
        };
        DrawTextW(
            hdc,
            &mut text_buf,
            &mut text_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
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
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            COLORREF(0x00242424),
            COLORREF(0x00353535),
        );

        render_card_texts(
            hdc,
            left,
            top,
            right,
            top + (54.0 * scale).round() as i32,
            scale,
            font_title,
            font_desc,
            title,
            desc,
        );

        let items_top = top + (52.0 * scale).round() as i32;
        let item_h = (26.0 * scale).round() as i32;
        let col_w = (right - left - (32.0 * scale).round() as i32) / 2;
        let col1_left = left + (16.0 * scale).round() as i32;
        let col2_left = col1_left + col_w;

        // 左列 (Net, CPU, Mem)
        draw_checkbox_item(
            hdc,
            col1_left,
            items_top,
            col1_left + col_w,
            items_top + item_h,
            scale,
            font_desc,
            &bundle.monitor.metric_net_label,
            config.show_network,
            hovered == Some(31),
        );
        draw_checkbox_item(
            hdc,
            col1_left,
            items_top + item_h,
            col1_left + col_w,
            items_top + item_h * 2,
            scale,
            font_desc,
            &bundle.monitor.metric_cpu_label,
            config.show_cpu,
            hovered == Some(32),
        );
        draw_checkbox_item(
            hdc,
            col1_left,
            items_top + item_h * 2,
            col1_left + col_w,
            items_top + item_h * 3,
            scale,
            font_desc,
            &bundle.monitor.metric_mem_label,
            config.show_memory,
            hovered == Some(33),
        );

        // 右列 (GPU, Disk)
        draw_checkbox_item(
            hdc,
            col2_left,
            items_top,
            col2_left + col_w,
            items_top + item_h,
            scale,
            font_desc,
            &bundle.monitor.metric_gpu_label,
            config.show_gpu,
            hovered == Some(34),
        );
        draw_checkbox_item(
            hdc,
            col2_left,
            items_top + item_h,
            col2_left + col_w,
            items_top + item_h * 2,
            scale,
            font_desc,
            &bundle.monitor.metric_disk_label,
            config.show_disk,
            hovered == Some(35),
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
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            COLORREF(0x00242424),
            COLORREF(0x00353535),
        );

        render_card_texts(
            hdc,
            left,
            top,
            right,
            top + (54.0 * scale).round() as i32,
            scale,
            font_title,
            font_desc,
            title,
            desc,
        );

        let row0_top = top + (54.0 * scale).round() as i32;
        let row1_top = top + (84.0 * scale).round() as i32;
        let row2_top = top + (114.0 * scale).round() as i32;
        let row3_top = top + (144.0 * scale).round() as i32;
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
        DrawTextW(
            hdc,
            &mut r0_lbl,
            &mut r0_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        let pos_w = (138.0 * scale).round() as i32;
        let seg_h = (24.0 * scale).round() as i32;
        let seg_gap = (6.0 * scale).round() as i32;
        let pos_base_right = right - right_pad;
        let pos_opts = [
            (
                1,
                MonitorPosition::TaskbarLeft,
                &bundle.monitor.menu_pos_taskbar_left,
                46usize,
            ),
            (
                0,
                MonitorPosition::TrayLeft,
                &bundle.monitor.menu_pos_tray_left,
                45usize,
            ),
        ];

        for (rev_i, pos_val, label, hov_id) in pos_opts {
            let seg_r = pos_base_right - rev_i * (pos_w + seg_gap);
            let seg_l = seg_r - pos_w;
            let is_sel = config.position == pos_val;
            let is_hov = hovered == Some(hov_id);

            let (s_bg, s_bd, s_txt) = if is_sel {
                (
                    COLORREF(0x00d47800),
                    COLORREF(0x00d47800),
                    COLORREF(0x00ffffff),
                )
            } else if is_hov {
                (
                    COLORREF(0x00333333),
                    COLORREF(0x00555555),
                    COLORREF(0x00e0e0e0),
                )
            } else {
                (
                    COLORREF(0x00282828),
                    COLORREF(0x003d3d3d),
                    COLORREF(0x00909090),
                )
            };

            let seg_rect = RECT {
                left: seg_l,
                top: row0_top,
                right: seg_r,
                bottom: row0_top + seg_h,
            };
            render_rounded_button(
                hdc,
                &seg_rect,
                label,
                font_desc,
                s_bg,
                s_txt,
                Some(s_bd),
                (4.0 * scale).round() as i32,
            );
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
        DrawTextW(
            hdc,
            &mut r1_lbl,
            &mut r1_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        let seg_w = (96.0 * scale).round() as i32;
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
                (
                    COLORREF(0x00d47800),
                    COLORREF(0x00d47800),
                    COLORREF(0x00ffffff),
                )
            } else if is_hov {
                (
                    COLORREF(0x00333333),
                    COLORREF(0x00555555),
                    COLORREF(0x00e0e0e0),
                )
            } else {
                (
                    COLORREF(0x00282828),
                    COLORREF(0x003d3d3d),
                    COLORREF(0x00909090),
                )
            };

            let seg_rect = RECT {
                left: seg_l,
                top: row1_top,
                right: seg_r,
                bottom: row1_top + seg_h,
            };
            render_rounded_button(
                hdc,
                &seg_rect,
                label,
                font_desc,
                s_bg,
                s_txt,
                Some(s_bd),
                (4.0 * scale).round() as i32,
            );
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
        DrawTextW(
            hdc,
            &mut r2_lbl,
            &mut r2_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

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
        draw_fluent_box(
            hdc, sw_left, sw_top, sw_right, sw_bottom, sw_h, sw_bg, sw_border,
        );

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

        // 行 3: 监控面板透明度 Slider (ID 44)
        SetTextColor(hdc, COLORREF(0x00d0d0d0));
        let mut r3_lbl: Vec<u16> = bundle.monitor.opacity_label.encode_utf16().collect();
        let mut r3_rect = RECT {
            left: inner_x,
            top: row3_top,
            right: inner_x + (160.0 * scale).round() as i32,
            bottom: row3_top + (24.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut r3_lbl,
            &mut r3_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

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
        DrawTextW(
            hdc,
            &mut pct_buf,
            &mut pct_rect,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
        );

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
    bundle: &crate::rules::LocaleBundle,
) {
    unsafe {
        draw_fluent_box(
            hdc,
            left,
            top,
            right,
            bottom,
            (8.0 * scale).round() as i32,
            COLORREF(0x00212121),
            COLORREF(0x00343434),
        );

        let inner_x = left + (16.0 * scale).round() as i32;

        SelectObject(hdc, HGDIOBJ(font_bold.0 as _));
        SetTextColor(hdc, COLORREF(0x0050d268));
        let title_text = if !bundle.monitor.tips_title.is_empty() {
            bundle.monitor.tips_title.as_str()
        } else {
            "💡 高效交互与智能特性指南"
        };
        let mut title_buf: Vec<u16> = title_text.encode_utf16().collect();
        let mut title_rect = RECT {
            left: inner_x,
            top: top + (8.0 * scale).round() as i32,
            right: right - (16.0 * scale).round() as i32,
            bottom: top + (26.0 * scale).round() as i32,
        };
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut title_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        SelectObject(hdc, HGDIOBJ(font_small.0 as _));
        SetTextColor(hdc, COLORREF(0x009e9e9e));

        let line1 = if !bundle.monitor.tips_line1.is_empty() {
            bundle.monitor.tips_line1.as_str()
        } else {
            "• 双击直接调出系统任务管理器；右键菜单可快速切换常驻指标与停靠位置"
        };
        let line2 = if !bundle.monitor.tips_line2.is_empty() {
            bundle.monitor.tips_line2.as_str()
        } else {
            "• 鼠标悬停弹出 Fluent 硬件详情看板；全屏应用与 3D 游戏智能避让隐匿"
        };
        let tips = [line1, line2];

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
            DrawTextW(
                hdc,
                &mut buf,
                &mut r,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            tip_top += line_h + (2.0 * scale).round() as i32;
        }
    }
}

/// 鼠标悬停位置探测与手型光标切换
fn handle_settings_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 {
        1.0
    } else {
        (dpi as f32 / 96.0).max(1.0)
    };

    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let pad_x = (28.0 * scale).round() as i32;

    let card_w = rect.right - pad_x * 2;
    let card_gap = (8.0 * scale).round() as i32;

    // 顶部 Tab 栏坐标（与渲染/点击共享同一几何）
    let tab_geo = settings_tab_geometry(scale);
    let tab_y = tab_geo.top;
    let tab_h = tab_geo.height;
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

    let current_tab = CURRENT_TAB
        .lock()
        .map(|t| *t)
        .unwrap_or(SettingsTab::General);

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
        if let Ok(mut h) = HOVER_DROPDOWN_ITEM_IDX.lock()
            && *h != new_hov_item
        {
            *h = new_hov_item;
            changed = true;
        }
        if changed {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }

        let is_over_combo =
            x >= combo_left && x <= combo_right && y >= combo_top && y <= combo_bottom;
        unsafe {
            let cursor_id = if new_hov_item.is_some() || is_over_combo {
                IDC_HAND
            } else {
                IDC_ARROW
            };
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
            DragMode::AutoSwitchToggle {
                start_x,
                initial_state,
                ref mut has_moved,
            } => {
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
            DragMode::AutostartToggle {
                start_x,
                initial_state,
                ref mut has_moved,
            } => {
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
            DragMode::TabMasterToggle {
                start_x,
                initial_state,
                ref mut has_moved,
            } => {
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
                if config.tab_module.enabled != new_state {
                    config.tab_module.enabled = new_state;
                    set_draft_config(config);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                return;
            }
            DragMode::MonitorMasterToggle {
                start_x,
                initial_state,
                ref mut has_moved,
            } => {
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
            DragMode::IntegrationToggle {
                which,
                start_x,
                initial_state,
                ref mut has_moved,
            } => {
                let dx = x - start_x;
                if dx.abs() > 4 {
                    *has_moved = true;
                }
                let new_state = if dx > 8 {
                    true
                } else if dx < -8 {
                    false
                } else {
                    initial_state
                };
                let mut config = get_draft_config();
                let current = if which == 0 {
                    config.enable_totalcmd
                } else {
                    config.enable_xyplorer
                };
                if current != new_state {
                    if which == 0 {
                        config.enable_totalcmd = new_state;
                    } else {
                        config.enable_xyplorer = new_state;
                    }
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
    if let Some((hover_id, _)) = settings_tab_hit(pad_x, &tab_geo, x, y) {
        new_hover = Some(hover_id);
        is_pointer = true;
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
            let about_h = (120.0 * scale).round() as i32;

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
                    let text_x = pad_x
                        + (16.0 * scale).round() as i32
                        + logo_w
                        + (18.0 * scale).round() as i32;
                    let text_right = pad_x + card_w - (16.0 * scale).round() as i32;

                    let web_top = card5_top + (51.0 * scale).round() as i32;
                    let web_bottom = card5_top + (69.0 * scale).round() as i32;
                    let gh_top = card5_top + (72.0 * scale).round() as i32;
                    let gh_bottom = card5_top + (90.0 * scale).round() as i32;

                    if x >= text_x && x <= text_right && y >= web_top && y <= web_bottom {
                        new_hover = Some(20);
                        is_pointer = true;
                    } else if x >= text_x && x <= text_right && y >= gh_top && y <= gh_bottom {
                        new_hover = Some(21);
                        is_pointer = true;
                    }
                } else {
                    let footer = general_footer_layout(
                        pad_x,
                        card_w,
                        card5_top + about_h,
                        rect.bottom,
                        scale,
                    );
                    if let Some(id) = general_footer_hit(x, y, &footer) {
                        new_hover = Some(id);
                        is_pointer = true;
                    }
                }
            }
        }
        SettingsTab::TabModule => {
            if crate::win32::version::is_tab_module_supported() {
                let config = get_draft_config();
                let card0_top = card_top_base;
                let card0_h = (56.0 * scale).round() as i32;
                let card1_top = card0_top + card0_h + card_gap;
                let card1_h = (90.0 * scale).round() as i32;
                let card2_top = card1_top + card1_h + card_gap;
                let card2_h = (134.0 * scale).round() as i32;

                if x >= pad_x && x <= pad_x + card_w {
                    // 卡片 0：总开关
                    if y >= card0_top && y <= card0_top + card0_h {
                        new_hover = Some(60);
                        is_pointer = true;
                    }
                    // 卡片 1：快捷键与窗口接管（仅总开关开启时响应悬停高亮）
                    else if y >= card1_top && y <= card1_top + card1_h {
                        if config.tab_module.enabled {
                            let header_h = (46.0 * scale).round() as i32;
                            let row_h = (44.0 * scale).round() as i32;
                            let r1_top = card1_top + header_h;
                            if y >= r1_top && y <= r1_top + row_h {
                                new_hover = Some(61);
                                is_pointer = true;
                            }
                        }
                    }
                    // 卡片 2：智能行为与鼠标手势（仅总开关开启时响应悬停高亮）
                    else if y >= card2_top
                        && y <= card2_top + card2_h
                        && config.tab_module.enabled
                    {
                        let header_h = (46.0 * scale).round() as i32;
                        let row_h = (44.0 * scale).round() as i32;
                        let r1_top = card2_top + header_h;
                        let r2_top = r1_top + row_h;
                        if y >= r1_top && y <= r1_top + row_h {
                            new_hover = Some(63);
                            is_pointer = true;
                        } else if y >= r2_top && y <= r2_top + row_h {
                            new_hover = Some(64);
                            is_pointer = true;
                        }
                    }
                }
            }
        }
        SettingsTab::Monitor => {
            let card_m0_top = card_top_base;
            let card_m0_h = (56.0 * scale).round() as i32;
            let card_m1_top = card_m0_top + card_m0_h + card_gap;
            let card_m1_h = (140.0 * scale).round() as i32;
            let card_m2_top = card_m1_top + card_m1_h + card_gap;
            let card_m2_h = (182.0 * scale).round() as i32;

            if x >= pad_x && x <= pad_x + card_w {
                if y >= card_m0_top && y <= card_m0_top + card_m0_h {
                    new_hover = Some(30);
                    is_pointer = true;
                } else if y >= card_m1_top && y <= card_m1_top + card_m1_h {
                    let items_top = card_m1_top + (52.0 * scale).round() as i32;
                    let item_h = (26.0 * scale).round() as i32;
                    let col_w = (card_w - (32.0 * scale).round() as i32) / 2;
                    let col1_left = pad_x + (16.0 * scale).round() as i32;
                    let col2_left = col1_left + col_w;

                    if y >= items_top && y <= items_top + item_h * 3 {
                        let row = ((y - items_top) / item_h) as usize;
                        if x >= col1_left && x <= col1_left + col_w {
                            match row {
                                0 => {
                                    new_hover = Some(31);
                                    is_pointer = true;
                                }
                                1 => {
                                    new_hover = Some(32);
                                    is_pointer = true;
                                }
                                2 => {
                                    new_hover = Some(33);
                                    is_pointer = true;
                                }
                                _ => {}
                            }
                        } else if x >= col2_left && x <= col2_left + col_w {
                            match row {
                                0 => {
                                    new_hover = Some(34);
                                    is_pointer = true;
                                }
                                1 => {
                                    new_hover = Some(35);
                                    is_pointer = true;
                                }
                                _ => {}
                            }
                        }
                    }
                } else if y >= card_m2_top && y <= card_m2_top + card_m2_h {
                    let row0_top = card_m2_top + (54.0 * scale).round() as i32;
                    let row1_top = card_m2_top + (84.0 * scale).round() as i32;
                    let row2_top = card_m2_top + (114.0 * scale).round() as i32;
                    let row3_top = card_m2_top + (144.0 * scale).round() as i32;
                    let right_pad = (18.0 * scale).round() as i32;

                    // 行 0: 2个停靠位置胶囊 (ID 45 / 46)
                    if y >= row0_top && y <= row0_top + (24.0 * scale).round() as i32 {
                        let pos_w = (138.0 * scale).round() as i32;
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
                        let seg_w = (96.0 * scale).round() as i32;
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
                        if x >= track_left - (10.0 * scale) as i32
                            && x <= track_right + (10.0 * scale) as i32
                        {
                            new_hover = Some(44);
                            is_pointer = true;
                        }
                    }
                }
            }
        }
        SettingsTab::Integration => {
            // 审计报告 U-01：两个授权开关卡片
            let card_h = (56.0 * scale).round() as i32;
            let card_i0_top = card_top_base;
            let card_i1_top = card_i0_top + card_h + card_gap;
            if x >= pad_x && x <= pad_x + card_w {
                if y >= card_i0_top && y <= card_i0_top + card_h {
                    new_hover = Some(HOVER_ID_INTEGRATION_TC);
                    is_pointer = true;
                } else if y >= card_i1_top && y <= card_i1_top + card_h {
                    new_hover = Some(HOVER_ID_INTEGRATION_XY);
                    is_pointer = true;
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
    if let Ok(mut h) = HOVER_CARD_IDX.lock()
        && *h != new_hover
    {
        *h = new_hover;
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

/// 鼠标松开处理（释放拖拽模式）
fn handle_settings_mouse_up(hwnd: HWND, _x: i32, _y: i32) {
    if let Ok(mut drag) = DRAG_MODE.lock()
        && *drag != DragMode::None
    {
        *drag = DragMode::None;
        unsafe {
            let _ = ReleaseCapture();
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

/// 鼠标点击交互核心引擎
fn handle_settings_mouse_down(hwnd: HWND, x: i32, y: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 {
        1.0
    } else {
        (dpi as f32 / 96.0).max(1.0)
    };

    let mut config = get_draft_config();
    let mut need_redraw = false;

    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let pad_x = (28.0 * scale).round() as i32;

    let card_w = rect.right - pad_x * 2;
    let card_gap = (8.0 * scale).round() as i32;

    // 顶部 Tab 栏坐标（与渲染/悬停共享同一几何）
    let tab_geo = settings_tab_geometry(scale);
    let tab_y = tab_geo.top;
    let tab_h = tab_geo.height;
    let line_y = tab_y + tab_h + (8.0 * scale).round() as i32;
    let card_top_base = line_y + (10.0 * scale).round() as i32;

    let current_tab = CURRENT_TAB
        .lock()
        .map(|t| *t)
        .unwrap_or(SettingsTab::General);

    // 1. 顶部 Tab 选项卡点击（几何与渲染完全一致，避免“点 A 高亮 B”）
    if let Some((_, tab)) = settings_tab_hit(pad_x, &tab_geo, x, y) {
        if let Ok(mut t) = CURRENT_TAB.lock() {
            *t = tab;
        }
        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
        IS_DROPDOWN_OPEN.store(false, Ordering::SeqCst);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
        return;
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
            let about_h = (120.0 * scale).round() as i32;

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
                let mut lang_options =
                    vec![("auto".to_string(), "自动跟随系统 (Auto)".to_string())];
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
            } else if x >= pad_x && x <= pad_x + card_w && y >= card1_top && y <= card1_top + card_h
            {
                let is_rec = IS_RECORDING_HOTKEY.load(Ordering::SeqCst);
                IS_RECORDING_HOTKEY.store(!is_rec, Ordering::SeqCst);
                need_redraw = true;
            } else if x >= pad_x && x <= pad_x + card_w && y >= card2_top && y <= card2_top + card_h
            {
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
            } else if x >= pad_x && x <= pad_x + card_w && y >= card3_top && y <= card3_top + card_h
            {
                IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                let slider_w = (140.0 * scale).round() as i32;
                let track_right = rect.right - pad_x - (18.0 * scale).round() as i32;
                let track_left = track_right - slider_w;

                if x >= track_left - (20.0 * scale) as i32
                    && x <= track_right + (20.0 * scale) as i32
                {
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
                let text_x =
                    pad_x + (16.0 * scale).round() as i32 + logo_w + (18.0 * scale).round() as i32;
                let text_right = pad_x + card_w - (16.0 * scale).round() as i32;

                let web_top = card5_top + (51.0 * scale).round() as i32;
                let web_bottom = card5_top + (69.0 * scale).round() as i32;
                let gh_top = card5_top + (72.0 * scale).round() as i32;
                let gh_bottom = card5_top + (90.0 * scale).round() as i32;

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
                    let url: Vec<u16> = "https://github.com/clhome/QuickPath\0"
                        .encode_utf16()
                        .collect();
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
            } else {
                let footer =
                    general_footer_layout(pad_x, card_w, card5_top + about_h, rect.bottom, scale);
                match general_footer_hit(x, y, &footer) {
                    Some(HOVER_ID_LOG_CLEAR) => {
                        let cleared = crate::dialog::detector::clear_debug_logs();
                        if let Ok(mut result) = LOG_CLEAR_RESULT.lock() {
                            *result = Some(cleared);
                        }
                        IS_RECORDING_HOTKEY.store(false, Ordering::SeqCst);
                        need_redraw = true;
                    }
                    Some(HOVER_ID_DEBUG_CHECKBOX) => {
                        let cur = crate::dialog::detector::is_debug_mode_enabled();
                        crate::dialog::detector::set_debug_mode_enabled(!cur);
                        need_redraw = true;
                    }
                    _ => {}
                }
            }
        }
        SettingsTab::TabModule => {
            if crate::win32::version::is_tab_module_supported() {
                let card0_top = card_top_base;
                let card0_h = (56.0 * scale).round() as i32;
                let card1_top = card0_top + card0_h + card_gap;
                let card1_h = (90.0 * scale).round() as i32;
                let card2_top = card1_top + card1_h + card_gap;
                let card2_h = (134.0 * scale).round() as i32;

                if x >= pad_x && x <= pad_x + card_w {
                    // 点击 卡片 0：总开关
                    if y >= card0_top && y <= card0_top + card0_h {
                        let old_state = config.tab_module.enabled;
                        config.tab_module.enabled = !old_state;
                        set_draft_config(config.clone());
                        need_redraw = true;

                        if let Ok(mut drag) = DRAG_MODE.lock() {
                            *drag = DragMode::TabMasterToggle {
                                start_x: x,
                                initial_state: old_state,
                                has_moved: false,
                            };
                        }
                        unsafe {
                            let _ = SetCapture(hwnd);
                        }
                    }
                    // 点击 卡片 1：快捷键与窗口接管（仅总开关开启时响应）
                    else if y >= card1_top && y <= card1_top + card1_h {
                        if config.tab_module.enabled {
                            let header_h = (46.0 * scale).round() as i32;
                            let row_h = (44.0 * scale).round() as i32;
                            let r1_top = card1_top + header_h;
                            if y >= r1_top && y <= r1_top + row_h {
                                config.tab_module.wine_to_tab = !config.tab_module.wine_to_tab;
                                set_draft_config(config.clone());
                                need_redraw = true;
                            }
                        }
                    }
                    // 点击 卡片 2：智能行为与鼠标手势（仅总开关开启时响应）
                    else if y >= card2_top
                        && y <= card2_top + card2_h
                        && config.tab_module.enabled
                    {
                        let header_h = (46.0 * scale).round() as i32;
                        let row_h = (44.0 * scale).round() as i32;
                        let r1_top = card2_top + header_h;
                        let r2_top = r1_top + row_h;
                        if y >= r1_top && y <= r1_top + row_h {
                            config.tab_module.deduplication = !config.tab_module.deduplication;
                            set_draft_config(config.clone());
                            need_redraw = true;
                        } else if y >= r2_top && y <= r2_top + row_h {
                            config.tab_module.double_click_to_duplicate =
                                !config.tab_module.double_click_to_duplicate;
                            set_draft_config(config.clone());
                            need_redraw = true;
                        }
                    }
                }
            }
        }
        SettingsTab::Monitor => {
            let card_m0_top = card_top_base;
            let card_m0_h = (56.0 * scale).round() as i32;
            let card_m1_top = card_m0_top + card_m0_h + card_gap;
            let card_m1_h = (140.0 * scale).round() as i32;
            let card_m2_top = card_m1_top + card_m1_h + card_gap;
            let card_m2_h = (182.0 * scale).round() as i32;

            // 点击 M0: 任务栏监控总开关
            if x >= pad_x && x <= pad_x + card_w && y >= card_m0_top && y <= card_m0_top + card_m0_h
            {
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
            else if x >= pad_x
                && x <= pad_x + card_w
                && y >= card_m1_top
                && y <= card_m1_top + card_m1_h
            {
                let items_top = card_m1_top + (52.0 * scale).round() as i32;
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
                        if !config.monitor.show_network
                            && !config.monitor.show_cpu
                            && !config.monitor.show_memory
                            && !config.monitor.show_gpu
                            && !config.monitor.show_disk
                        {
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
                        if !config.monitor.show_network
                            && !config.monitor.show_cpu
                            && !config.monitor.show_memory
                            && !config.monitor.show_gpu
                            && !config.monitor.show_disk
                        {
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
            else if x >= pad_x
                && x <= pad_x + card_w
                && y >= card_m2_top
                && y <= card_m2_top + card_m2_h
            {
                let row0_top = card_m2_top + (54.0 * scale).round() as i32;
                let row1_top = card_m2_top + (84.0 * scale).round() as i32;
                let row2_top = card_m2_top + (114.0 * scale).round() as i32;
                let row3_top = card_m2_top + (144.0 * scale).round() as i32;
                let seg_h = (24.0 * scale).round() as i32;
                let right_pad = (18.0 * scale).round() as i32;

                // 行 0: 任务栏停靠位置单选胶囊 (防遮挡)
                if y >= row0_top && y <= row0_top + seg_h {
                    let pos_w = (138.0 * scale).round() as i32;
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
                    let seg_w = (96.0 * scale).round() as i32;
                    let seg_gap = (6.0 * scale).round() as i32;
                    let seg_base_right = pad_x + card_w - right_pad;
                    let seg_opts = [(2, 1000u64), (1, 3000u64), (0, 5000u64)];
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
                    if x >= track_left - (20.0 * scale) as i32
                        && x <= track_right + (20.0 * scale) as i32
                    {
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
        SettingsTab::Integration => {
            // 审计报告 U-01：点击授权开关卡片即切换草稿配置（与其它 Toggle 卡片一致），
            // 但**仅写入配置**，不触发任何探测。
            let card_h = (56.0 * scale).round() as i32;
            let card_i0_top = card_top_base;
            let card_i1_top = card_i0_top + card_h + card_gap;
            if x >= pad_x && x <= pad_x + card_w {
                let which = if y >= card_i0_top && y <= card_i0_top + card_h {
                    Some(0usize)
                } else if y >= card_i1_top && y <= card_i1_top + card_h {
                    Some(1usize)
                } else {
                    None
                };
                if let Some(which) = which {
                    let old_state = if which == 0 {
                        config.enable_totalcmd
                    } else {
                        config.enable_xyplorer
                    };
                    if which == 0 {
                        config.enable_totalcmd = !old_state;
                    } else {
                        config.enable_xyplorer = !old_state;
                    }
                    set_draft_config(config.clone());
                    need_redraw = true;

                    if let Ok(mut drag) = DRAG_MODE.lock() {
                        *drag = DragMode::IntegrationToggle {
                            which,
                            start_x: x,
                            initial_state: old_state,
                            has_moved: false,
                        };
                    }
                    unsafe {
                        let _ = SetCapture(hwnd);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_about_os_version_rendering() {
        let bundles = crate::rules::I18n::get_bundles();
        let zh = bundles.get("zh-CN").expect("zh-CN bundle should exist");
        let os_str_zh = crate::win32::version::format_windows_version(true);
        let line_zh = zh.about.os_environment.replace("{os_version}", &os_str_zh);
        assert!(line_zh.starts_with("系统环境："));
        assert!(line_zh.contains("Windows"));

        let en = bundles.get("en-US").expect("en-US bundle should exist");
        let os_str_en = crate::win32::version::format_windows_version(false);
        let line_en = en.about.os_environment.replace("{os_version}", &os_str_en);
        assert!(line_en.starts_with("System:"));
        assert!(line_en.contains("Windows"));
    }

    #[test]
    fn test_tab_module_unsupported_notices() {
        let bundles = crate::rules::I18n::get_bundles();
        let zh = bundles.get("zh-CN").expect("zh-CN bundle should exist");
        assert_eq!(zh.tab_module.unsupported_badge, "系统环境限制");
        assert_eq!(
            zh.tab_module.unsupported_title,
            "当前 Windows 系统版本暂不支持多标签增强"
        );
        let os_str = crate::win32::version::format_windows_version(true);
        let desc = zh
            .tab_module
            .unsupported_desc
            .replace("{os_version}", &os_str);
        assert!(desc.contains("22621"));
        assert!(desc.contains(&os_str));

        let en = bundles.get("en-US").expect("en-US bundle should exist");
        assert_eq!(en.tab_module.unsupported_badge, "System Unsupported");
        assert_eq!(
            en.tab_module.unsupported_title,
            "File Explorer Tabs Not Supported on Current Windows"
        );
        let desc_en = en
            .tab_module
            .unsupported_desc
            .replace("{os_version}", &os_str);
        assert!(desc_en.contains("22621"));
    }

    #[test]
    fn test_tab_module_mock_unsupported_switch() {
        crate::win32::version::set_mock_tab_unsupported(true);
        assert!(!crate::win32::version::is_tab_module_supported());
        crate::win32::version::set_mock_tab_unsupported(false);
        // 当前运行环境是 Windows 11 Build >= 22621
        assert!(crate::win32::version::is_tab_module_supported());
    }

    #[test]
    fn test_tab_module_unsupported_multiline_format() {
        let bundles = crate::rules::I18n::get_bundles();
        let zh = bundles.get("zh-CN").expect("zh-CN bundle should exist");
        let en = bundles.get("en-US").expect("en-US bundle should exist");

        let os_str_mock = "Windows 10 ServerStandard 1809 (OS 内部版本 17763.107)";
        let desc_zh = zh
            .tab_module
            .unsupported_desc
            .replace("{os_version}", os_str_mock);
        assert!(desc_zh.contains('\n'), "中文描述应包含分段换行符以保持整洁");
        assert!(desc_zh.contains("17763.107"));
        assert!(!desc_zh.is_empty());

        let desc_en = en
            .tab_module
            .unsupported_desc
            .replace("{os_version}", os_str_mock);
        assert!(desc_en.contains('\n'), "英文描述应包含分段换行符以保持整洁");
        assert!(desc_en.contains("17763.107"));
        assert!(!desc_en.is_empty());
    }

    #[test]
    fn test_tab_module_settings_groups_and_tips_i18n() {
        let bundles = crate::rules::I18n::get_bundles();
        let zh = bundles.get("zh-CN").expect("zh-CN bundle should exist");
        assert_eq!(zh.tab_module.shortcuts_group_title, "快捷键与窗口接管");
        assert_eq!(zh.tab_module.behaviors_group_title, "智能行为与鼠标手势");
        assert!(zh.tab_module.tips_title.contains("💡"));
        assert!(zh.tab_module.tips_line1.contains("鼠标中键"));
        assert!(zh.tab_module.tips_line2.contains("Ctrl+Tab"));

        let en = bundles.get("en-US").expect("en-US bundle should exist");
        assert_eq!(
            en.tab_module.shortcuts_group_title,
            "Shortcuts & Window Integration"
        );
        assert_eq!(
            en.tab_module.behaviors_group_title,
            "Smart Behaviors & Mouse Gestures"
        );
        assert!(en.tab_module.tips_title.contains("💡"));
        assert!(en.tab_module.tips_line1.contains("Middle-click"));
        assert!(en.tab_module.tips_line2.contains("Ctrl+Tab"));
    }

    /// 新增日志文案必须在两个语言包中都非空（缺失时由 `i18n_or_fallback` 兜底）。
    #[test]
    fn test_log_card_i18n_keys_present_in_both_locales() {
        let bundles = crate::rules::I18n::get_bundles();
        let zh = bundles.get("zh-CN").expect("zh-CN bundle should exist");
        let en = bundles.get("en-US").expect("en-US bundle should exist");
        assert_eq!(zh.settings.log_title, "日志");
        assert_eq!(zh.settings.log_clear, "清理日志");
        assert_eq!(zh.settings.log_cleared, "已清理");
        assert_eq!(zh.settings.log_clear_failed, "清理失败");
        assert_eq!(zh.settings.log_none, "日志不可用");
        assert_eq!(en.settings.log_title, "Log");
        assert_eq!(en.settings.log_clear, "Clear log");
        assert_eq!(en.settings.log_cleared, "Cleared");
        assert_eq!(en.settings.log_clear_failed, "Clear failed");
        assert_eq!(en.settings.log_none, "Log unavailable");
    }

    /// 日志卡片与调试复选框的几何/命中逻辑（不启动 GUI，纯函数自证）：
    /// 渲染、命中测试与点击处理共用 `general_footer_layout` + `general_footer_hit`，
    /// 断言命中 id 与几何布局一致，防止「点 A 触发 B」。
    #[test]
    fn test_general_footer_layout_and_hit_ids() {
        use super::*;

        // 锁定 id 字面值：渲染/命中/点击三处共用这些常量，改动即测试变红。
        assert_eq!(HOVER_ID_DEBUG_CHECKBOX, 50);
        assert_eq!(HOVER_ID_LOG_CARD, 51);
        assert_eq!(HOVER_ID_LOG_CLEAR, 52);

        let scale = 1.0_f32;
        let pad_x = 28;
        let card_w = 660 - pad_x * 2;
        let card5_bottom = 546; // 与 General Tab 卡片几何一致（此处只需可复现输入）
        let client_bottom = 719;
        let layout = general_footer_layout(pad_x, card_w, card5_bottom, client_bottom, scale);

        // 几何关系：日志卡片在关于卡片下方、宽度撑满；按钮在卡片内右侧；复选框在卡片与底部按钮之间。
        assert!(
            layout.log_card.top >= card5_bottom,
            "日志卡片不得与关于卡片重叠"
        );
        assert_eq!(layout.log_card.left, pad_x);
        assert_eq!(layout.log_card.right, pad_x + card_w);
        assert!(layout.clear_button.left > layout.log_card.left);
        assert!(layout.clear_button.right < layout.log_card.right);
        assert!(layout.clear_button.top >= layout.log_card.top);
        assert!(layout.clear_button.bottom <= layout.log_card.bottom);
        assert!(
            layout.debug_checkbox.top >= layout.log_card.bottom,
            "复选框必须在日志卡片之下"
        );
        let btn_top = client_bottom - 22 - 34;
        assert!(
            layout.debug_checkbox.bottom <= btn_top,
            "复选框不得压到底部按钮"
        );

        // 命中：清理按钮中心 → 清理 id；卡片左区 → 卡片 id；复选框中心 → 调试 id；卡片外 → None。
        let clear_cx = (layout.clear_button.left + layout.clear_button.right) / 2;
        let clear_cy = (layout.clear_button.top + layout.clear_button.bottom) / 2;
        assert_eq!(
            general_footer_hit(clear_cx, clear_cy, &layout),
            Some(HOVER_ID_LOG_CLEAR)
        );
        assert_eq!(
            general_footer_hit(layout.log_card.left + 10, layout.log_card.top + 10, &layout),
            Some(HOVER_ID_LOG_CARD)
        );
        let dbg_cx = (layout.debug_checkbox.left + layout.debug_checkbox.right) / 2;
        let dbg_cy = (layout.debug_checkbox.top + layout.debug_checkbox.bottom) / 2;
        assert_eq!(
            general_footer_hit(dbg_cx, dbg_cy, &layout),
            Some(HOVER_ID_DEBUG_CHECKBOX)
        );
        assert_eq!(general_footer_hit(pad_x - 1, clear_cy, &layout), None);
        // 清理按钮与调试开关命中 id 必须不同，否则点清理会误触调试开关。
        assert_ne!(HOVER_ID_LOG_CLEAR, HOVER_ID_DEBUG_CHECKBOX);
    }

    /// 审计报告 U-01：管理器联动页的文案必须在两个语言包中都非空。
    #[test]
    fn test_integration_settings_i18n_present_in_both_locales() {
        let bundles = crate::rules::I18n::get_bundles();
        let zh = bundles.get("zh-CN").expect("zh-CN bundle should exist");
        let en = bundles.get("en-US").expect("en-US bundle should exist");

        for (name, section) in [("zh-CN", &zh.integration), ("en-US", &en.integration)] {
            for (field, value) in [
                ("tab_title", &section.tab_title),
                ("totalcmd_switch", &section.totalcmd_switch),
                ("totalcmd_desc", &section.totalcmd_desc),
                ("xyplorer_switch", &section.xyplorer_switch),
                ("xyplorer_desc", &section.xyplorer_desc),
                ("tips_title", &section.tips_title),
                ("tips_line1", &section.tips_line1),
                ("tips_line2", &section.tips_line2),
                ("tips_line3", &section.tips_line3),
            ] {
                assert!(
                    !value.trim().is_empty(),
                    "{name} 语言包的 integration.{field} 不得为空"
                );
            }
        }
        assert!(
            zh.integration.tips_line3.contains("不覆盖")
                || zh.integration.tips_line3.contains("保留"),
            "中文提示必须说清“来源不可证明时取消、不覆盖当前剪贴板”"
        );

        // 解析后的文案必须真的被回退逻辑补上（且两语言不同，避免英文界面出现中文）
        let zh_texts = crate::rules::I18n::integration_texts(&crate::rules::Language::ZhCN);
        let en_texts = crate::rules::I18n::integration_texts(&crate::rules::Language::EnUS);
        assert_eq!(zh_texts.tab_title, zh.integration.tab_title);
        assert_eq!(en_texts.tab_title, en.integration.tab_title);
        assert_ne!(zh_texts.tab_title, en_texts.tab_title);
        assert_ne!(zh_texts.totalcmd_desc, en_texts.totalcmd_desc);
    }

    /// 审计报告 U-01：新增的第四个 Tab 必须与渲染、悬停、点击共用同一几何，
    /// 否则会出现“点 A 高亮 B”（旧代码渲染宽 118、悬停宽 136 就已错位）。
    #[test]
    fn test_settings_tab_geometry_and_hit_testing_are_consistent() {
        let scale = 1.0;
        let pad_x = 28;
        let geo = settings_tab_geometry(scale);
        let cy = geo.top + geo.height / 2;

        for (index, (tab, hover_id)) in SETTINGS_TABS.iter().enumerate() {
            let left = settings_tab_left(pad_x, &geo, index);
            assert_eq!(
                settings_tab_hit(pad_x, &geo, left + geo.width / 2, cy),
                Some((*hover_id, *tab)),
                "第 {} 个 Tab 的命中结果必须与其自身一致",
                index
            );
        }

        // 四个 Tab 必须互不重叠：每个 Tab 右边界之后的第一个像素属于下一个 Tab。
        for index in 0..SETTINGS_TABS.len() - 1 {
            let left = settings_tab_left(pad_x, &geo, index);
            let next_left = settings_tab_left(pad_x, &geo, index + 1);
            assert!(
                next_left >= left + geo.width,
                "Tab 之间不得重叠（否则点击会落到错误的页面）"
            );
            assert_eq!(
                settings_tab_hit(pad_x, &geo, next_left, cy).map(|(_, t)| t),
                Some(SETTINGS_TABS[index + 1].0)
            );
        }

        // Tab 栏之外不得命中任何 Tab
        assert_eq!(settings_tab_hit(pad_x, &geo, pad_x, geo.top - 1), None);
        assert_eq!(
            settings_tab_hit(pad_x, &geo, pad_x, geo.top + geo.height + 1),
            None
        );
        let last_right = settings_tab_left(pad_x, &geo, SETTINGS_TABS.len() - 1) + geo.width;
        assert_eq!(settings_tab_hit(pad_x, &geo, last_right + 1, cy), None);
    }

    /// 审计报告 U-01 验收标准 2：设置界面必须能开启/关闭并持久化两个字段。
    ///
    /// 设置页只写**草稿配置**（点“确定”才落盘），绝不从设置页发起探测：
    /// 因此这里同时守住“设置页不得调用探测入口”这条边界。
    #[test]
    fn test_integration_settings_source_guard() {
        const SOURCE: &str = include_str!("settings.rs");
        let production = &SOURCE[..SOURCE.find("#[cfg(test)]").expect("必须存在测试模块")];

        for required in [
            "HOVER_ID_INTEGRATION_TC",
            "HOVER_ID_INTEGRATION_XY",
            "DragMode::IntegrationToggle",
            "config.enable_totalcmd = !old_state",
            "config.enable_xyplorer = !old_state",
        ] {
            assert!(
                production.contains(required),
                "设置页必须提供两个授权开关（缺少 `{}`）",
                required
            );
        }
        assert!(
            !production.contains("request_explicit_probe"),
            "设置页只授权，绝不发起探测（探测只能来自托盘菜单 / 吸附条按钮）"
        );
    }

    /// 审计报告 U-01 验收标准 7：两个管理器都要有 UI + 配置 + 调用链覆盖。
    ///
    /// 这里守住“UI 入口 → 宿主探测入口”的调用链：托盘菜单与吸附条按钮都必须接到
    /// `request_explicit_probe`，且托盘菜单在未授权/无目标时必须置灰。
    #[test]
    fn test_explicit_probe_call_chain_is_wired_from_ui() {
        const MAIN_SRC: &str = include_str!("../main.rs");
        const BAR_SRC: &str = include_str!("floating_bar.rs");
        const TRAY_SRC: &str = include_str!("tray.rs");

        assert!(
            MAIN_SRC.contains("IDM_PROBE_TOTALCMD =>")
                && MAIN_SRC.contains("IDM_PROBE_XYPLORER =>"),
            "托盘菜单的两个探测项必须接到宿主处理分支"
        );
        // 源码断言需对 rustfmt（换行/缩进）鲁棒：先折叠空白再比对
        let main_norm = MAIN_SRC.split_whitespace().collect::<Vec<_>>().join(" ");
        for manager in ["TotalCmd", "XYplorer"] {
            let expected = format!(
                "request_explicit_probe(managers::explicit_probe::ScriptedManager::{});",
                manager
            );
            assert!(
                main_norm.contains(&expected),
                "{} 菜单项必须调用显式探测入口（缺少 `{}`）",
                manager,
                expected
            );
        }
        assert!(
            BAR_SRC.contains("crate::request_explicit_probe(manager)"),
            "吸附条按钮必须调用显式探测入口"
        );
        assert!(
            BAR_SRC.contains("ScriptedManager::TotalCmd")
                && BAR_SRC.contains("ScriptedManager::XYplorer"),
            "吸附条必须分别提供 TC 与 XYplorer 两个入口"
        );
        assert!(
            TRAY_SRC.contains("MF_GRAYED"),
            "托盘菜单在未授权或无可归因目标时必须置灰"
        );
    }
}
