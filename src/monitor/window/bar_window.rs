#![allow(unsafe_op_in_unsafe_fn)]

use crate::monitor::metrics::MetricsSnapshot;
use crate::monitor::window::render::{calculate_layout, render_bar_window};
use crate::monitor::window::tooltip::MonitorTooltip;
use crate::rules::{I18n, Language, MonitorConfig, MonitorPosition};
use arc_swap::ArcSwap;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::GetMonitorInfoW;
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TRACKMOUSEEVENT, TME_HOVER, TME_LEAVE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    FindWindowExW, FindWindowW, GetCursorPos, GetForegroundWindow, GetWindowRect,
    KillTimer, LoadCursorW, PostMessageW, RegisterClassW, RegisterWindowMessageW,
    SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
    TrackPopupMenu, HWND_TOPMOST, IDC_ARROW, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_MOUSEMOVE, WM_POWERBROADCAST, WM_RBUTTONUP, WM_SETCURSOR,
    WM_SETTINGCHANGE, WM_TIMER,
};

const WM_MOUSEHOVER: u32 = 0x02A1;
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_REFRESH_ID: usize = 2001;
const TIMER_FULLSCREEN_CHECK_ID: usize = 2002;

// 菜单命令 ID
const IDM_MONITOR_OPEN_SETTINGS: usize = 3001;
const IDM_TOGGLE_NET: usize = 3002;
const IDM_TOGGLE_CPU: usize = 3003;
const IDM_TOGGLE_MEM: usize = 3004;
const IDM_TOGGLE_GPU: usize = 3005;
const IDM_TOGGLE_DISK: usize = 3006;
const IDM_TOGGLE_GRAPH_BG: usize = 3007;
const IDM_REFRESH_ADAPTERS: usize = 3008;
const IDM_HIDE_MONITOR: usize = 3009;
const IDM_MONITOR_EXIT: usize = 3010;
const IDM_POS_TRAY_LEFT: usize = 3011;
const IDM_POS_TASKBAR_LEFT: usize = 3012;

static BAR_INSTANCE: AtomicPtr<BarWindowInner> = AtomicPtr::new(std::ptr::null_mut());

pub struct BarWindow {
    hwnd: HWND,
}

struct BarWindowInner {
    hwnd: HWND,
    snapshot_handle: Arc<ArcSwap<MetricsSnapshot>>,
    config: MonitorConfig,
    language: Language,
    tooltip: Option<MonitorTooltip>,
    is_fullscreen_hidden: bool,
    is_dark_theme: bool,
    taskbar_created_msg: u32,
    last_rect: RECT,
}

impl BarWindow {
    pub fn new(
        snapshot_handle: Arc<ArcSwap<MetricsSnapshot>>,
        config: MonitorConfig,
        language: Language,
    ) -> Result<Self, String> {
        crate::win32::icon::ensure_gdiplus();

        let class_name = w!("QuickPath_Taskbar_Monitor_Class");
        unsafe {
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(bar_wnd_proc),
                hInstance: HINSTANCE::default(),
                hCursor: cursor,
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let hwnd = match CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                class_name,
                w!("QuickPath Taskbar Monitor"),
                WS_POPUP,
                0,
                0,
                130,
                36,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => return Err("创建任务栏监控分层窗口失败".to_string()),
            };

            let taskbar_msg = RegisterWindowMessageW(w!("TaskbarCreated"));
            let is_dark = check_is_dark_theme();
            let tooltip_res = MonitorTooltip::new().ok();

            let inner = Box::new(BarWindowInner {
                hwnd,
                snapshot_handle,
                config,
                language,
                tooltip: tooltip_res,
                is_fullscreen_hidden: false,
                is_dark_theme: is_dark,
                taskbar_created_msg: taskbar_msg,
                last_rect: RECT::default(),
            });

            BAR_INSTANCE.store(Box::into_raw(inner), Ordering::SeqCst);

            // 启动定时刷新与全屏应用避让监测
            let _ = SetTimer(Some(hwnd), TIMER_REFRESH_ID, 1000, None);
            let _ = SetTimer(Some(hwnd), TIMER_FULLSCREEN_CHECK_ID, 1000, None);

            // 初始锚定位置并展示
            update_position_and_render(hwnd);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            Ok(Self { hwnd })
        }
    }

    pub fn update_config(&self, new_config: MonitorConfig) {
        let ptr = BAR_INSTANCE.load(Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let inner = &mut *ptr;
                inner.config = new_config.clone();
                let _ = KillTimer(Some(inner.hwnd), TIMER_REFRESH_ID);
                let _ = SetTimer(
                    Some(inner.hwnd),
                    TIMER_REFRESH_ID,
                    new_config.refresh_interval_ms as u32,
                    None,
                );
                update_position_and_render(inner.hwnd);
            }
        }
    }

    pub fn update_language(&self, lang: Language) {
        let ptr = BAR_INSTANCE.load(Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let inner = &mut *ptr;
                inner.language = lang;
            }
        }
    }

    pub fn destroy(self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

impl Drop for BarWindow {
    fn drop(&mut self) {
        let ptr = BAR_INSTANCE.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let _ = Box::from_raw(ptr);
            }
        }
    }
}

unsafe extern "system" fn bar_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = BAR_INSTANCE.load(Ordering::SeqCst);
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let inner = &mut *ptr;

    // Explorer 重启重锚定自愈
    if inner.taskbar_created_msg != 0 && msg == inner.taskbar_created_msg {
        update_position_and_render(hwnd);
        return LRESULT(0);
    }

    match msg {
        WM_TIMER => {
            if wparam.0 == TIMER_REFRESH_ID {
                if !inner.is_fullscreen_hidden {
                    update_position_and_render(hwnd);
                }
            } else if wparam.0 == TIMER_FULLSCREEN_CHECK_ID {
                check_fullscreen_and_avoid(inner);
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            inner.is_dark_theme = check_is_dark_theme();
            update_position_and_render(hwnd);
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            // 休眠唤醒，重新定位并触发刷新
            update_position_and_render(hwnd);
            LRESULT(1)
        }
        WM_DPICHANGED => {
            update_position_and_render(hwnd);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
            let _ = SetCursor(Some(cursor));
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_HOVER | TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 400, // 400ms 悬停弹出 Tooltip
            };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        WM_MOUSEHOVER => {
            // 弹出 Fluent Tooltip 硬件详情看板
            let snap = inner.snapshot_handle.load();
            let dpi = GetDpiForWindow(hwnd);
            let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };

            if let Some(tooltip) = &mut inner.tooltip {
                tooltip.show(inner.last_rect, &snap, &inner.language, scale, inner.is_dark_theme);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(tooltip) = &mut inner.tooltip {
                tooltip.hide();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // 左键单击：切换折线背景图显示
            inner.config.show_graph_bg = !inner.config.show_graph_bg;
            let mut cfg = crate::get_global_config();
            cfg.monitor = inner.config.clone();
            crate::update_global_config(cfg);
            update_position_and_render(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            // 左键双击：启动系统任务管理器 (taskmgr.exe)
            let taskmgr = w!("taskmgr.exe");
            let op = w!("open");
            let _ = ShellExecuteW(
                None,
                op,
                taskmgr,
                PCWSTR::null(),
                PCWSTR::null(),
                windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD(1),
            );
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // 弹出 Win32 原生右键菜单
            show_monitor_context_menu(inner);
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd = (wparam.0 & 0xFFFF) as usize;
            handle_context_command(inner, cmd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), TIMER_REFRESH_ID);
            let _ = KillTimer(Some(hwnd), TIMER_FULLSCREEN_CHECK_ID);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 弹出任务栏状态监控右键上下文菜单 (PRD 3.3)
unsafe fn show_monitor_context_menu(inner: &mut BarWindowInner) {
    let hmenu = match CreatePopupMenu() {
        Ok(m) if !m.0.is_null() => m,
        _ => return,
    };

    let bundle = I18n::get_bundle(&inner.language);

    let str_settings = to_utf16(&bundle.monitor.menu_settings);
    let str_metrics = to_utf16(&bundle.monitor.menu_metrics);
    let str_pos = to_utf16(&bundle.monitor.menu_position);
    let str_pos_tray = to_utf16(&bundle.monitor.menu_pos_tray_left);
    let str_pos_taskbar = to_utf16(&bundle.monitor.menu_pos_taskbar_left);
    let str_net = to_utf16(&bundle.monitor.menu_metric_net);
    let str_cpu = to_utf16(&bundle.monitor.menu_metric_cpu);
    let str_mem = to_utf16(&bundle.monitor.menu_metric_mem);
    let str_gpu = to_utf16(&bundle.monitor.menu_metric_gpu);
    let str_disk = to_utf16(&bundle.monitor.menu_metric_disk);
    let str_graph_bg = to_utf16(&bundle.monitor.menu_graph_bg);
    let str_refresh = to_utf16(&bundle.monitor.menu_refresh_net);
    let str_hide = to_utf16(&bundle.monitor.menu_hide);
    let str_exit = to_utf16(&bundle.monitor.menu_exit);

    // 1. 打开设置
    let _ = AppendMenuW(hmenu, MF_STRING, IDM_MONITOR_OPEN_SETTINGS, PCWSTR(str_settings.as_ptr()));
    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());

    // 2. 指标子菜单
    if let Ok(hsub) = CreatePopupMenu() {
        if !hsub.0.is_null() {
            let flag = |checked: bool| if checked { MF_CHECKED } else { MF_UNCHECKED };

            let _ = AppendMenuW(hsub, MF_STRING | flag(inner.config.show_network), IDM_TOGGLE_NET, PCWSTR(str_net.as_ptr()));
            let _ = AppendMenuW(hsub, MF_STRING | flag(inner.config.show_cpu), IDM_TOGGLE_CPU, PCWSTR(str_cpu.as_ptr()));
            let _ = AppendMenuW(hsub, MF_STRING | flag(inner.config.show_memory), IDM_TOGGLE_MEM, PCWSTR(str_mem.as_ptr()));
            let _ = AppendMenuW(hsub, MF_STRING | flag(inner.config.show_gpu), IDM_TOGGLE_GPU, PCWSTR(str_gpu.as_ptr()));
            let _ = AppendMenuW(hsub, MF_STRING | flag(inner.config.show_disk), IDM_TOGGLE_DISK, PCWSTR(str_disk.as_ptr()));

            let _ = AppendMenuW(hmenu, MF_POPUP, hsub.0 as usize, PCWSTR(str_metrics.as_ptr()));
        }
    }

    // 3. 停靠位置子菜单（防遮挡）
    if let Ok(hpos) = CreatePopupMenu() {
        if !hpos.0.is_null() {
            let is_tray = inner.config.position == MonitorPosition::TrayLeft;
            let is_taskbar = inner.config.position == MonitorPosition::TaskbarLeft;
            let flag = |checked: bool| if checked { MF_CHECKED } else { MF_UNCHECKED };

            let _ = AppendMenuW(hpos, MF_STRING | flag(is_tray), IDM_POS_TRAY_LEFT, PCWSTR(str_pos_tray.as_ptr()));
            let _ = AppendMenuW(hpos, MF_STRING | flag(is_taskbar), IDM_POS_TASKBAR_LEFT, PCWSTR(str_pos_taskbar.as_ptr()));

            let _ = AppendMenuW(hmenu, MF_POPUP, hpos.0 as usize, PCWSTR(str_pos.as_ptr()));
        }
    }

    let flag_bg = if inner.config.show_graph_bg { MF_CHECKED } else { MF_UNCHECKED };
    let _ = AppendMenuW(hmenu, MF_STRING | flag_bg, IDM_TOGGLE_GRAPH_BG, PCWSTR(str_graph_bg.as_ptr()));
    let _ = AppendMenuW(hmenu, MF_STRING, IDM_REFRESH_ADAPTERS, PCWSTR(str_refresh.as_ptr()));
    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(hmenu, MF_STRING, IDM_HIDE_MONITOR, PCWSTR(str_hide.as_ptr()));
    let _ = AppendMenuW(hmenu, MF_STRING, IDM_MONITOR_EXIT, PCWSTR(str_exit.as_ptr()));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    let _ = SetForegroundWindow(inner.hwnd);
    let _ = TrackPopupMenu(
        hmenu,
        TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        Some(0),
        inner.hwnd,
        None,
    );

    let _ = DestroyMenu(hmenu);
}

/// 处理右键菜单响应
unsafe fn handle_context_command(inner: &mut BarWindowInner, cmd: usize) {
    let mut changed = false;
    match cmd {
        IDM_MONITOR_OPEN_SETTINGS => {
            let host = crate::HOST_HWND.load(Ordering::SeqCst);
            if !host.is_null() {
                let _ = PostMessageW(
                    Some(HWND(host)),
                    WM_COMMAND,
                    WPARAM(crate::ui::tray::IDM_OPEN_SETTINGS),
                    LPARAM(0),
                );
            }
        }
        IDM_TOGGLE_NET => {
            inner.config.show_network = !inner.config.show_network;
            changed = true;
        }
        IDM_TOGGLE_CPU => {
            inner.config.show_cpu = !inner.config.show_cpu;
            changed = true;
        }
        IDM_TOGGLE_MEM => {
            inner.config.show_memory = !inner.config.show_memory;
            changed = true;
        }
        IDM_TOGGLE_GPU => {
            inner.config.show_gpu = !inner.config.show_gpu;
            changed = true;
        }
        IDM_TOGGLE_DISK => {
            inner.config.show_disk = !inner.config.show_disk;
            changed = true;
        }
        IDM_POS_TRAY_LEFT => {
            if inner.config.position != MonitorPosition::TrayLeft {
                inner.config.position = MonitorPosition::TrayLeft;
                changed = true;
            }
        }
        IDM_POS_TASKBAR_LEFT => {
            if inner.config.position != MonitorPosition::TaskbarLeft {
                inner.config.position = MonitorPosition::TaskbarLeft;
                changed = true;
            }
        }
        IDM_TOGGLE_GRAPH_BG => {
            inner.config.show_graph_bg = !inner.config.show_graph_bg;
            changed = true;
        }
        IDM_REFRESH_ADAPTERS => {
            // 刷新网卡列表并重绘
            update_position_and_render(inner.hwnd);
        }
        IDM_HIDE_MONITOR => {
            inner.config.enabled = false;
            let mut cfg = crate::get_global_config();
            cfg.monitor.enabled = false;
            let _ = cfg.save();
            crate::update_global_config(cfg);
        }
        IDM_MONITOR_EXIT => {
            let host = crate::HOST_HWND.load(Ordering::SeqCst);
            if !host.is_null() {
                let _ = PostMessageW(
                    Some(HWND(host)),
                    WM_COMMAND,
                    WPARAM(crate::ui::tray::IDM_EXIT),
                    LPARAM(0),
                );
            }
        }
        _ => {}
    }

    if changed {
        let mut cfg = crate::get_global_config();
        cfg.monitor = inner.config.clone();
        let _ = cfg.save();
        crate::update_global_config(cfg);
        update_position_and_render(inner.hwnd);
    }
}

/// 更新位置与重绘
unsafe fn update_position_and_render(hwnd: HWND) {
    let ptr = BAR_INSTANCE.load(Ordering::SeqCst);
    if ptr.is_null() {
        return;
    }
    let inner = &mut *ptr;

    let dpi = GetDpiForWindow(hwnd);
    let scale = if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(1.0) };
    let layout = calculate_layout(&inner.config, scale);

    if layout.total_width <= 0 {
        let _ = ShowWindow(hwnd, SW_HIDE);
        return;
    }

    // 寻找任务栏与托盘位置
    let tray_wnd = FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default();
    if tray_wnd.0.is_null() {
        return;
    }

    let mut tray_rect = RECT::default();
    let _ = GetWindowRect(tray_wnd, &mut tray_rect);

    // 托盘通知区域 TrayNotifyWnd
    let notify_wnd = FindWindowExW(Some(tray_wnd), None, w!("TrayNotifyWnd"), None).unwrap_or_default();
    let mut notify_rect = RECT::default();
    if !notify_wnd.0.is_null() {
        let _ = GetWindowRect(notify_wnd, &mut notify_rect);
    } else {
        notify_rect = tray_rect;
    }

    let bar_w = layout.total_width;
    let bar_h = layout.total_height;
    let margin = (6.0 * scale).round() as i32;

    // 根据配置计算停靠位置：
    // 1. TaskbarLeft：停靠在任务栏左侧（开始按钮/搜索框/小组件之后），彻底避免窗口数量过多时被任务栏任务列表挤压遮挡
    // 2. TrayLeft：停靠在系统托盘通知区左侧
    let target_x = match inner.config.position {
        MonitorPosition::TaskbarLeft => {
            let mut left_offset = (56.0 * scale).round() as i32;
            let start_btn = FindWindowExW(Some(tray_wnd), None, w!("Start"), None).unwrap_or_default();
            if !start_btn.0.is_null() {
                let mut start_rect = RECT::default();
                if GetWindowRect(start_btn, &mut start_rect).is_ok() && start_rect.right > tray_rect.left {
                    left_offset = (start_rect.right - tray_rect.left) + margin;
                }
            }
            tray_rect.left + left_offset
        }
        MonitorPosition::TrayLeft => {
            notify_rect.left - bar_w - margin
        }
    };

    let target_y = tray_rect.top + (tray_rect.bottom - tray_rect.top - bar_h) / 2;

    inner.last_rect = RECT {
        left: target_x,
        top: target_y,
        right: target_x + bar_w,
        bottom: target_y + bar_h,
    };

    // 必须传入 HWND_TOPMOST 保持顶层，杜绝 WS_EX_TOPMOST 属性被剥夺导致窗口过多时沉降被遮挡
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        target_x,
        target_y,
        bar_w,
        bar_h,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );

    let snap = inner.snapshot_handle.load();
    render_bar_window(hwnd, &snap, &inner.config, scale, inner.is_dark_theme);
}

/// PRD 4.2 全屏应用主动隐匿避让检测
unsafe fn check_fullscreen_and_avoid(inner: &mut BarWindowInner) {
    let fg_hwnd = GetForegroundWindow();
    if fg_hwnd.0.is_null() {
        return;
    }

    let mut fg_rect = RECT::default();
    if GetWindowRect(fg_hwnd, &mut fg_rect).is_err() {
        return;
    }

    let hmon = MonitorFromWindow(fg_hwnd, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(hmon, &mut mi).as_bool() {
        let mon_rect = mi.rcMonitor;
        // 如果前台窗口大小覆盖了整个屏幕
        let is_fullscreen = fg_rect.left <= mon_rect.left
            && fg_rect.top <= mon_rect.top
            && fg_rect.right >= mon_rect.right
            && fg_rect.bottom >= mon_rect.bottom;

        if is_fullscreen {
            let class_name = crate::dialog::detector::get_window_class_name(fg_hwnd);
            // 排除桌面和任务栏窗口
            if class_name != "Progman" && class_name != "WorkerW" && class_name != "Shell_TrayWnd" {
                if !inner.is_fullscreen_hidden {
                    inner.is_fullscreen_hidden = true;
                    let _ = ShowWindow(inner.hwnd, SW_HIDE);
                    if let Some(tooltip) = &inner.tooltip {
                        tooltip.hide();
                    }
                }
                return;
            }
        }
    }

    if inner.is_fullscreen_hidden {
        inner.is_fullscreen_hidden = false;
        let _ = ShowWindow(inner.hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            inner.hwnd,
            Some(HWND_TOPMOST),
            inner.last_rect.left,
            inner.last_rect.top,
            inner.last_rect.right - inner.last_rect.left,
            inner.last_rect.bottom - inner.last_rect.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        update_position_and_render(inner.hwnd);
    }
}

/// 检测系统当前是否为深色任务栏主题
fn check_is_dark_theme() -> bool {
    unsafe {
        let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey, Some(0), KEY_READ, &mut hkey).is_ok() {
            let val_name = w!("SystemUsesLightTheme");
            let mut val: u32 = 0;
            let mut val_size: u32 = 4;
            let res = RegQueryValueExW(
                hkey,
                val_name,
                None,
                None,
                Some(&mut val as *mut _ as *mut _),
                Some(&mut val_size),
            );
            let _ = RegCloseKey(hkey);
            if res.is_ok() {
                return val == 0; // 0 表示深色，1 表示浅色
            }
        }
        true // 默认深色
    }
}

fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
