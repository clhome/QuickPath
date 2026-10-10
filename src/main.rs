#![windows_subsystem = "windows"]

mod dialog;
mod managers;
mod monitor;
mod rules;
mod ui;
mod win32;

use dialog::{detect_file_dialog, inject_path_to_dialog};
use managers::WindowTracker;
use monitor::MonitorManager;
use rules::AppConfig;
use ui::tray::{
    TrayIcon, IDM_EXIT, IDM_OPEN_SETTINGS, IDM_SHOW_CANDIDATES, IDM_TOGGLE_AUTOSTART,
    IDM_TOGGLE_AUTOSWITCH, IDM_TOGGLE_MONITOR, WM_TRAY_CALLBACK,
};
use ui::{FloatingBar, SettingsWindow};
use win32::autostart::set_autostart;
use win32::events::{get_active_foreground_window, WinEventHookGuard, WM_QUICKPATH_FOREGROUND};

use std::sync::atomic::{AtomicPtr, Ordering};
use std::thread;
use std::time::Duration;
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;

use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, MSG, WM_COMMAND, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP,
    WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

const HOTKEY_ID: i32 = 101;
static MAIN_APP_STATE: AtomicPtr<AppState> = AtomicPtr::new(std::ptr::null_mut());
static HOST_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub struct AppState {
    pub config: AppConfig,
    pub tracker: WindowTracker,
    pub floating_bar: Option<FloatingBar>,
    pub settings_window: Option<SettingsWindow>,
    pub monitor_manager: Option<MonitorManager>,
    pub last_switched_dialog: Option<HWND>,
    pub last_switched_path: Option<String>,
}

fn main() {
    unsafe {
        // 1. 单实例互斥体防护，避免多个 QuickPath 驻留冲突
        let mutex_name = w!("Local\\QuickPath_SingleInstance_Mutex");
        let mutex = CreateMutexW(None, true, mutex_name);
        if let Ok(_m) = mutex {
            if windows::Win32::Foundation::GetLastError()
                == windows::Win32::Foundation::ERROR_ALREADY_EXISTS
            {
                return;
            }
        }

        // 2. 初始化 COM 库与 GDI+ 环境
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        crate::win32::icon::ensure_gdiplus();

        let config = AppConfig::load();

        // 异步同步自启动设置，杜绝阻塞主线程 UI 初始化
        if config.autostart_enabled {
            let use_sched = config.autostart_task_scheduler;
            thread::spawn(move || {
                let _ = set_autostart(true, use_sched);
            });
        }

        // 3. 注册主事件接收宿主隐藏窗口
        let class_name = w!("QuickPath_Host_Class");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(host_wnd_proc),
            hInstance: HINSTANCE::default(),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let host_hwnd = match CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
            class_name,
            w!("QuickPath Host"),
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
            _ => {
                CoUninitialize();
                return;
            }
        };

        HOST_HWND.store(host_hwnd.0 as *mut _, Ordering::SeqCst);

        // 4. 注册全局唤出快捷键 (根据配置动态注册，默认为 Ctrl + Q)
        let _ = win32::hotkey::register_global_hotkey(host_hwnd, HOTKEY_ID, &config.hotkey);

        // 5. 初始化托盘、悬浮吸附条与设置窗口
        let tray_res = TrayIcon::new(host_hwnd);
        let floating_bar_res = FloatingBar::new();
        let settings_window_res = SettingsWindow::new();
        let monitor_mgr = MonitorManager::new(&config.monitor, config.language.clone());

        let state = Box::new(AppState {
            config,
            tracker: WindowTracker::new(),
            floating_bar: floating_bar_res.ok(),
            settings_window: settings_window_res.ok(),
            monitor_manager: Some(monitor_mgr),
            last_switched_dialog: None,
            last_switched_path: None,
        });

        MAIN_APP_STATE.store(Box::into_raw(state), Ordering::SeqCst);

        // 6. 开启全局系统前台窗口事件监听钩子
        let _hook_guard = WinEventHookGuard::new(host_hwnd);

        // 7. 进入 Win32 标准消息驱动循环
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 退出前清理
        let _ = UnregisterHotKey(Some(host_hwnd), HOTKEY_ID);
        drop(tray_res);

        let raw_ptr = MAIN_APP_STATE.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !raw_ptr.is_null() {
            let _ = Box::from_raw(raw_ptr);
        }

        CoUninitialize();
    }
}

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_QUICKPATH_FOREGROUND => {
            let fg_hwnd = HWND(wparam.0 as *mut _);
            on_foreground_window_changed(fg_hwnd);
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 == HOTKEY_ID as usize {
                on_hotkey_triggered();
            }
            LRESULT(0)
        }
        WM_TRAY_CALLBACK => {
            let event = lparam.0 as u32;
            if event == WM_RBUTTONUP {
                let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
                if !state_ptr.is_null() {
                    unsafe {
                        let state = &*state_ptr;
                        TrayIcon::show_context_menu(
                            hwnd,
                            state.config.auto_switch_enabled,
                            state.config.autostart_enabled,
                            state.config.monitor.enabled,
                            state.config.language.clone(),
                            &state.config.hotkey,
                        );

                    }
                }
            } else if event == WM_LBUTTONUP {
                open_settings();
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd_id = (wparam.0 & 0xffff) as usize;
            match cmd_id {
                IDM_TOGGLE_AUTOSWITCH => {
                    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
                    if !state_ptr.is_null() {
                        unsafe {
                            let state = &mut *state_ptr;
                            state.config.auto_switch_enabled = !state.config.auto_switch_enabled;
                            let _ = state.config.save();
                        }
                    }
                }
                IDM_TOGGLE_AUTOSTART => {
                    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
                    if !state_ptr.is_null() {
                        unsafe {
                            let state = &mut *state_ptr;
                            state.config.autostart_enabled = !state.config.autostart_enabled;
                            let _ = set_autostart(
                                state.config.autostart_enabled,
                                state.config.autostart_task_scheduler,
                            );
                            let _ = state.config.save();
                        }
                    }
                }
                IDM_TOGGLE_MONITOR => {
                    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
                    if !state_ptr.is_null() {
                        unsafe {
                            let state = &mut *state_ptr;
                            state.config.monitor.enabled = !state.config.monitor.enabled;
                            let _ = state.config.save();
                            if let Some(mgr) = &mut state.monitor_manager {
                                mgr.sync_config(&state.config.monitor);
                            }
                        }
                    }
                }
                IDM_SHOW_CANDIDATES => {
                    on_hotkey_triggered();
                }
                IDM_OPEN_SETTINGS => {
                    open_settings();
                }
                IDM_EXIT => unsafe {
                    PostQuitMessage(0);
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => unsafe {
            PostQuitMessage(0);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn on_foreground_window_changed(fg_hwnd: HWND) {
    on_foreground_window_changed_impl(fg_hwnd, 0);
}

fn on_foreground_window_changed_impl(fg_hwnd: HWND, retry_count: u32) {
    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
    if state_ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *state_ptr };

    // 0. 如果前台窗口是悬浮吸附条自身，直接忽略，避免用户点击或交互时将吸附条误隐藏
    if let Some(bar) = &state.floating_bar {
        if bar.hwnd() == fg_hwnd {
            return;
        }
    }

    let class_name = dialog::detector::get_window_class_name(fg_hwnd);

    // 1. 如果切入的是文件管理器，更新追踪器时间戳与路径
    state.tracker.on_foreground_change(fg_hwnd, &class_name, &state.config);

    // 2. 检查是否切入了文件对话框
    if let Some(dlg_info) = detect_file_dialog(fg_hwnd) {
        // 黑名单过滤（若配置了特定黑名单进程）
        if state.config.is_blacklisted(&dlg_info.process_name) {
            return;
        }

        let candidates = state.tracker.get_all_candidates(&state.config);

        crate::dialog::detector::log_debug(&format!(
            "MAIN: Dlg detected! hwnd=0x{:X} title='{}' bar_is_some={} candidates={}",
            dlg_info.hwnd.0 as usize, dlg_info.window_title, state.floating_bar.is_some(), candidates.len()
        ));

        // 3. 需求 3：无论何时打开文件对话框，悬浮吸附条均直接自动展示！
        if let Some(bar) = &mut state.floating_bar {
            let edit_target = dlg_info.file_name_edit_hwnd.unwrap_or(dlg_info.hwnd);
            bar.show(
                dlg_info.hwnd,
                edit_target,
                dlg_info.rect,
                candidates,
                state.config.floating_bar_opacity,
                state.config.language.clone(),
            );
        }

        // 避免对同一个对话框实例重复触发秒切
        if state.last_switched_dialog == Some(dlg_info.hwnd) {
            return;
        }

        // 4. 执行自动秒切逻辑 (AutoSwitch)
        if state.config.auto_switch_enabled {
            if let Some(target_folder) = state.tracker.get_auto_switch_target(&state.config) {
                let should_inject = match dlg_info.kind {
                    dialog::DialogKind::StandardWin32 => dlg_info.file_name_edit_hwnd.is_some(),
                    dialog::DialogKind::WpsOffice => true,
                };

                if should_inject {
                    let delay_ms = state.config.auto_switch_delay_ms;
                    let dlg_raw = dlg_info.hwnd.0 as usize;
                    let edit_raw = dlg_info.file_name_edit_hwnd.unwrap_or(dlg_info.hwnd).0 as usize;
                    let target = target_folder.clone();

                    state.last_switched_dialog = Some(dlg_info.hwnd);
                    state.last_switched_path = Some(target_folder.clone());
                    state.config.record_history(&target_folder);

                    // 延时秒切，确保宿主应用对话框完成初始化绘制与控件就绪
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(delay_ms));
                        inject_path_to_dialog(
                            HWND(dlg_raw as *mut _),
                            HWND(edit_raw as *mut _),
                            &target,
                        );
                    });
                }
            }
        }
    } else {
        // 如果检测未立即命中，分析当前窗口或其顶层根窗口是否属于潜在对话框（可能子控件尚未完成异步绘制）
        const MAX_RETRIES: u32 = 8;
        const RETRY_INTERVAL_MS: u64 = 40;

        if retry_count < MAX_RETRIES {
            let root_hwnd = dialog::detector::find_dialog_root(fg_hwnd);
            let root_class = dialog::detector::get_window_class_name(root_hwnd);
            let root_title = dialog::detector::get_window_title(root_hwnd);
            let (_pid, process_name) = dialog::detector::get_window_process_info(root_hwnd);

            let is_wps = dialog::detector::is_wps_process_name(&process_name);
            let is_wps_main_doc = root_class == "OpusApp" || root_class == "XLMAIN" || root_class == "PP9FrameClass";

            let is_potential_dialog = root_class == "#32770"
                || class_name == "#32770"
                || root_class == "KcfdFileDialog"
                || root_class == "Qt5QWindowIcon"
                || root_class.contains("Kcfd")
                || dialog::detector::is_file_dialog_title(&root_title)
                || (is_wps && !is_wps_main_doc);

            if is_potential_dialog {
                let hwnd_raw = fg_hwnd.0 as usize;
                let next_retry = retry_count + 1;
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(RETRY_INTERVAL_MS));
                    let target_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
                    if !target_ptr.is_null() {
                        let h = HWND(hwnd_raw as *mut _);
                        // 确认当前前台窗口依然是该窗口或其派生窗口，若用户已切走则提前终止轮询
                        let current_fg = win32::events::get_active_foreground_window();
                        let current_root = dialog::detector::find_dialog_root(current_fg);
                        if current_fg == h || current_root == dialog::detector::find_dialog_root(h) {
                            on_foreground_window_changed_impl(h, next_retry);
                        }
                    }
                });
                return;
            }
        }

        // 明确离开对话框或多次重试超时后仍非对话框时，隐藏悬浮吸附条
        if let Some(bar) = &state.floating_bar {
            bar.hide();
        }
    }
}

fn on_hotkey_triggered() {
    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
    if state_ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *state_ptr };

    let fg_hwnd = get_active_foreground_window();
    if let Some(dlg_info) = detect_file_dialog(fg_hwnd) {
        let candidates = state.tracker.get_all_candidates(&state.config);
        if let Some(bar) = &mut state.floating_bar {
            let edit_target = dlg_info.file_name_edit_hwnd.unwrap_or(dlg_info.hwnd);
            bar.show(
                dlg_info.hwnd,
                edit_target,
                dlg_info.rect,
                candidates,
                state.config.floating_bar_opacity,
                state.config.language.clone(),
            );
        }
    }
}

fn open_settings() {
    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
    if !state_ptr.is_null() {
        let state = unsafe { &*state_ptr };
        if let Some(settings) = &state.settings_window {
            settings.show();
        }
    }
}

pub fn get_global_config() -> AppConfig {
    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
    if !state_ptr.is_null() {
        unsafe {
            return (*state_ptr).config.clone();
        }
    }
    AppConfig::load()
}

pub fn update_global_config(new_config: AppConfig) {
    let state_ptr = MAIN_APP_STATE.load(Ordering::SeqCst);
    if !state_ptr.is_null() {
        unsafe {
            let state = &mut *state_ptr;
            let old_hotkey = state.config.hotkey.clone();
            state.config = new_config.clone();

            if let Some(mgr) = &mut state.monitor_manager {
                mgr.sync_config(&new_config.monitor);
                mgr.update_language(new_config.language.clone());
            }

            if old_hotkey != new_config.hotkey {
                let host_ptr = HOST_HWND.load(Ordering::SeqCst);
                if !host_ptr.is_null() {
                    let h = HWND(host_ptr);
                    let _ = win32::hotkey::register_global_hotkey(h, HOTKEY_ID, &new_config.hotkey);
                }
            }
        }
    }
}


