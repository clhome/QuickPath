#![windows_subsystem = "windows"]

mod dialog;
mod managers;
mod monitor;
mod rules;
pub mod tab;
mod ui;
mod win32;

use dialog::{InjectDone, detect_file_dialog, inject_path_to_dialog};
use managers::WindowTracker;
use monitor::MonitorManager;
use rules::AppConfig;
use ui::tray::{
    IDM_EXIT, IDM_OPEN_SETTINGS, IDM_SHOW_CANDIDATES, IDM_TOGGLE_AUTOSTART, IDM_TOGGLE_AUTOSWITCH,
    IDM_TOGGLE_MONITOR, TrayIcon, WM_TRAY_CALLBACK,
};
use ui::{FloatingBar, SettingsWindow};
use win32::autostart::set_autostart;
use win32::events::{WM_QUICKPATH_FOREGROUND, WinEventHookGuard, get_active_foreground_window};

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
use windows::core::w;

use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, IsWindow, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, TranslateMessage, WM_COMMAND, WM_DESTROY, WM_HOTKEY,
    WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

const HOTKEY_ID: i32 = 101;
const WM_QUICKPATH_RETRY_CHECK: u32 = 0x0400 + 102;
const WM_QUICKPATH_INJECT_FAILED: u32 = 0x0400 + 103;
/// Win+E 接管请求（由低级键盘钩子投递；钩子自身只做识别与投递，不做重活）
const WM_QUICKPATH_WINE_TO_TAB: u32 = tab::keyboard::WM_QUICKPATH_WINE_TO_TAB;
/// 后台文件管理器扫描结果回传（携带 generation + 目标 HWND，报告 P1-02）
const WM_QUICKPATH_MANAGER_SCAN_RESULT: u32 = 0x0400 + 105;
/// 后台首轮扫描完成，请宿主重绘一次对话框候选列表（P1-02 冷启动补偿）
const WM_QUICKPATH_CANDIDATES_REFRESH: u32 = 0x0400 + 106;
/// 对话框控件异步初始化期间的最大探测次数与间隔
/// （合计约 1.2s：慢速机器/首次加载的 shell 扩展常需数百毫秒才出现
///  DirectUIHWND / ComboBoxEx32 / Edit，过短会导致完全不弹窗）
const MAX_RETRIES: u32 = 20;
const RETRY_INTERVAL_MS: u64 = 60;
/// 自动秒切注入失败后的延迟重试等待
const INJECT_RETRY_DELAY_MS: u64 = 250;
static HOST_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// 后台文件管理器扫描结果回传通道的发送端。
///
/// 宿主消息线程持有接收端（`AppState::scan_rx`）；后台扫描线程 `send` 成功后
/// 用 `PostMessageW` 唤醒宿主排空队列。宿主已退出（接收端被 drop）时 `send`
/// 失败，结果由发送方自然丢弃，绝不泄漏。
static MANAGER_SCAN_TX: OnceLock<Sender<ManagerScanResult>> = OnceLock::new();

thread_local! {
    /// 宿主消息线程独占的 `AppState` 拥有值。
    ///
    /// 审计报告 R-05：原先以裸指针（`AtomicPtr` → `thread_local` + `Cell<*mut _>`）
    /// 表达“全局可变入口”，类型系统无法表达所有权，访问器还返回 `&'static mut`，
    /// 一旦重入即 UB。改为线程内自持的 `RefCell<Option<AppState>>` 后，状态由
    /// 宿主消息线程拥有、在 `main()` 作用域内正常 drop，对外只经作用域化的
    /// `with_state` 闭包访问；重入由 `RefCell` 在运行时拦截而非未定义行为。
    static APP_STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// 宿主状态重入被跳过（降级为 `None`）的累计次数；用于诊断日志限频与汇总。
static APP_STATE_REENTRY_COUNT: AtomicU64 = AtomicU64::new(0);

/// 重入诊断日志的限频间隔：首次立即告警，之后每 `APP_STATE_REENTRY_LOG_EVERY` 次
/// 汇总一条，避免同一时刻高频重入刷屏写盘。
const APP_STATE_REENTRY_LOG_EVERY: u64 = 100;

/// 在宿主消息线程内以闭包作用域访问 `AppState`；非宿主线程或已卸载时返回 `None`。
///
/// 闭包持有 `RefCell` 的可变借用。设计约定：正常路径必须避免重入——需要把状态
/// 交给下游函数时改为显式传入 `&mut AppState` / `&AppState`，并在调用可能泵消息的
/// Win32 API（如 `TrackPopupMenu`、STA COM/UIA 调用）前释放借用。
///
/// 但重入无法在类型层面彻底排除：`WM_TRAY_CALLBACK` 之外仍可能有 Win32 API 在持
/// 借用期间泵消息并重入 `host_wnd_proc`。因此这里用 `try_borrow_mut()` 让重入
/// **降级**而非 panic——重入时记录一条限频结构化诊断并返回 `None`。这一点至关重要：
/// `[profile.release] panic = "abort"` 且访问器经 `extern "system" fn host_wnd_proc`
/// 跨 FFI 边界调用，任何 panic 都会直接终止整个托盘进程，即一次重入 = 程序崩溃退出。
fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> Option<R> {
    APP_STATE.with(|cell| {
        let mut guard = match cell.try_borrow_mut() {
            Ok(guard) => guard,
            Err(_) => {
                // 当前线程已有活动借用 = 重入。降级为 `None` + 限频诊断，绝不 panic。
                let count = APP_STATE_REENTRY_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                if count == 1 || count.is_multiple_of(APP_STATE_REENTRY_LOG_EVERY) {
                    crate::dialog::detector::log_debug(&format!(
                        "APP_STATE_REENTRANT: 宿主状态正被占用，跳过本次重入访问（累计 {count} 次）"
                    ));
                }
                return None;
            }
        };
        guard.as_mut().map(f)
    })
}

pub struct AppState {
    pub config: AppConfig,
    pub tracker: WindowTracker,
    pub floating_bar: Option<FloatingBar>,
    pub settings_window: Option<SettingsWindow>,
    pub monitor_manager: Option<MonitorManager>,
    pub tab_module: Option<tab::TabModule>,
    pub last_switched_dialog: Option<HWND>,
    pub last_switched_path: Option<String>,
    /// 已为哪个对话框实例补过一次失败重试（防止无限重试）
    pub retried_dialog: Option<HWND>,
    /// 文件管理器后台扫描的单调世代号：只有仍对应当前前台窗口的结果才会被采纳
    pub manager_scan_generation: u64,
    /// 已为哪个对话框补刷过一次候选列表（冷启动补偿只做一次）
    pub refreshed_dialog: Option<HWND>,
    /// 后台文件管理器扫描结果回传通道的接收端（宿主消息线程独占）
    scan_rx: Receiver<ManagerScanResult>,
}

/// 后台文件管理器扫描结果：经 `mpsc` 通道回传（报告 P1-02）。
struct ManagerScanResult {
    generation: u64,
    hwnd_raw: usize,
    path: Option<String>,
}

fn main() {
    unsafe {
        // 1. 单实例互斥体防护，避免多个 QuickPath 驻留冲突
        let mutex_name = w!("Local\\QuickPath_SingleInstance_Mutex");
        let mutex = CreateMutexW(None, true, mutex_name);
        if let Ok(_m) = mutex
            && windows::Win32::Foundation::GetLastError()
                == windows::Win32::Foundation::ERROR_ALREADY_EXISTS
        {
            return;
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

        let mut tab_module = tab::TabModule::new();
        let tab_cfg = tab::TabConfig {
            enabled: config.tab_module.enabled,
            wine_to_tab: config.tab_module.wine_to_tab,
            deduplication: config.tab_module.deduplication,
            double_click_to_duplicate: config.tab_module.double_click_to_duplicate,
        };
        tab_module.update_config(tab_cfg);
        if config.tab_module.enabled && win32::version::is_tab_module_supported() {
            let _ = tab_module.start(host_hwnd);
        }

        // 建立后台扫描结果回传通道：发送端放入静态供后台线程取用，接收端由宿主线程持有
        let (scan_tx, scan_rx) = std::sync::mpsc::channel();
        let _ = MANAGER_SCAN_TX.set(scan_tx);

        let state = AppState {
            config,
            tracker: WindowTracker::new(),
            floating_bar: floating_bar_res.ok(),
            settings_window: settings_window_res.ok(),
            monitor_manager: Some(monitor_mgr),
            tab_module: Some(tab_module),
            last_switched_dialog: None,
            last_switched_path: None,
            retried_dialog: None,
            manager_scan_generation: 0,
            refreshed_dialog: None,
            scan_rx,
        };

        APP_STATE.with(|cell| *cell.borrow_mut() = Some(state));

        // 6. 开启全局系统前台窗口事件监听钩子
        let _hook_guard = WinEventHookGuard::new(host_hwnd);

        // 解析命令行参数（例如快速打开设置中心，或模拟低版本系统）
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--mock-unsupported") {
            win32::version::set_mock_tab_unsupported(true);
        }
        if args.iter().any(|a| a == "--settings") {
            with_state(|state| open_settings(state));
        }

        // 7. 进入 Win32 标准消息驱动循环
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 退出前清理：先置空宿主窗口句柄，拦截所有未决投递消息
        let _ = HOST_HWND.swap(std::ptr::null_mut(), Ordering::SeqCst);
        let _ = UnregisterHotKey(Some(host_hwnd), HOTKEY_ID);
        drop(tray_res);

        // 取出并正常 drop `AppState`：后台线程若仍有扫描结果未送达，其 `send`
        // 会失败并自然丢弃，不产生泄漏；退出路径不再依赖裸指针手动回收。
        drop(APP_STATE.with(|cell| cell.borrow_mut().take()));

        // 审计报告 R-06：常驻程序退出时显式收尾剪贴板 owner 线程（
        // `WM_CLOSE` → `WM_DESTROY` → `PostQuitMessage`，附有界 join），
        // 不再依赖进程结束时的系统回收。
        win32::clipboard::shutdown_clipboard_owner();

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
        WM_QUICKPATH_WINE_TO_TAB => {
            let target = HWND(wparam.0 as *mut _);
            let handled = with_state(|state| handle_wine_to_tab_request(state, target));
            if handled.is_none() {
                // 宿主状态尚未安装：与原先 `state_mut()` 返回 None 时一致，补偿原生窗口
                launch_native_explorer_window();
            }
            LRESULT(0)
        }
        WM_QUICKPATH_MANAGER_SCAN_RESULT => {
            // 后台线程只负责 `send` + 唤醒；这里排空通道，按原有
            // 「generation + 前台窗口一致性」规则采纳，陈旧结果自然丢弃。
            with_state(|state| {
                let current_fg = win32::events::get_active_foreground_window();
                let fg_raw = current_fg.0 as usize;
                let generation = state.manager_scan_generation;
                let payloads: Vec<ManagerScanResult> = state.scan_rx.try_iter().collect();
                for payload in payloads {
                    // 只接受“仍对应当前前台窗口”的本次世代结果，
                    // 避免用户已切走后把一个陈旧目录写进追踪器
                    if payload.generation == generation
                        && fg_raw == payload.hwnd_raw
                        && let Some(path) = payload.path
                    {
                        state
                            .tracker
                            .note_manager_folder(path, HWND(payload.hwnd_raw as *mut _));
                    }
                }
            });
            LRESULT(0)
        }
        WM_QUICKPATH_CANDIDATES_REFRESH => {
            // 后台首轮扫描完成后的冷启动补刷：仅在仍停留在同一对话框时重绘候选列表。
            let raw = wparam.0;
            if raw != 0 {
                with_state(|state| {
                    let dlg = HWND(raw as *mut _);
                    let current_fg = win32::events::get_active_foreground_window();
                    let still_same = current_fg == dlg
                        || dialog::detector::find_dialog_root(current_fg)
                            == dialog::detector::find_dialog_root(dlg);
                    if still_same
                        && let Some(dlg_info) = detect_file_dialog(dlg)
                        && !state.config.is_blacklisted(&dlg_info.process_name)
                    {
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
                                dlg_info.kind,
                            );
                        }
                    }
                });
            }
            LRESULT(0)
        }
        WM_QUICKPATH_RETRY_CHECK => {
            let h = HWND(wparam.0 as *mut _);
            let next_retry = lparam.0 as u32;
            let current_fg = win32::events::get_active_foreground_window();
            let current_root = dialog::detector::find_dialog_root(current_fg);
            if current_fg == h || current_root == dialog::detector::find_dialog_root(h) {
                with_state(|state| on_foreground_window_changed_impl(state, h, next_retry));
            }
            LRESULT(0)
        }
        WM_QUICKPATH_INJECT_FAILED => {
            // 秒切注入失败：清除去重标记，使该对话框可被重新触发；至多补一次延迟重试，
            // 杜绝“点了没反应、且此后再也不重试”的静默失效。
            let dlg = HWND(wparam.0 as *mut _);
            let already_retried = lparam.0 != 0;
            with_state(|state| {
                if state.last_switched_dialog == Some(dlg) {
                    state.last_switched_dialog = None;
                }
                if !already_retried {
                    state.retried_dialog = Some(dlg);
                    let host_ptr = HOST_HWND.load(Ordering::SeqCst);
                    if !host_ptr.is_null() {
                        let host_raw = host_ptr as usize;
                        let dlg_raw = dlg.0 as usize;
                        thread::spawn(move || {
                            thread::sleep(Duration::from_millis(INJECT_RETRY_DELAY_MS));
                            if !HOST_HWND.load(Ordering::SeqCst).is_null() {
                                unsafe {
                                    let _ = PostMessageW(
                                        Some(HWND(host_raw as *mut _)),
                                        WM_QUICKPATH_RETRY_CHECK,
                                        WPARAM(dlg_raw),
                                        LPARAM(MAX_RETRIES as isize),
                                    );
                                }
                            }
                        });
                    }
                }
            });
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 == HOTKEY_ID as usize {
                with_state(on_hotkey_triggered);
            }
            LRESULT(0)
        }
        WM_TRAY_CALLBACK => {
            let event = lparam.0 as u32;
            if event == WM_RBUTTONUP {
                // 先取出菜单所需配置再释放借用：`TrackPopupMenu` 会进入模态消息
                // 循环，期间可能重入宿主窗口过程；若此时仍持有 `APP_STATE` 借用
                // 会触发 `RefCell` panic。
                let menu_ctx = with_state(|state| {
                    (
                        state.config.auto_switch_enabled,
                        state.config.autostart_enabled,
                        state.config.monitor.enabled,
                        state.config.language.clone(),
                        state.config.hotkey.clone(),
                    )
                });
                if let Some((auto_switch, autostart, monitor, language, hotkey)) = menu_ctx {
                    TrayIcon::show_context_menu(
                        hwnd,
                        auto_switch,
                        autostart,
                        monitor,
                        language,
                        &hotkey,
                    );
                }
            } else if event == WM_LBUTTONUP {
                with_state(|state| open_settings(state));
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd_id = wparam.0 & 0xffff;
            match cmd_id {
                IDM_TOGGLE_AUTOSWITCH => {
                    with_state(|state| {
                        state.config.auto_switch_enabled = !state.config.auto_switch_enabled;
                        let _ = state.config.save();
                    });
                }
                IDM_TOGGLE_AUTOSTART => {
                    with_state(|state| {
                        state.config.autostart_enabled = !state.config.autostart_enabled;
                        let _ = set_autostart(
                            state.config.autostart_enabled,
                            state.config.autostart_task_scheduler,
                        );
                        let _ = state.config.save();
                    });
                }
                IDM_TOGGLE_MONITOR => {
                    with_state(|state| {
                        state.config.monitor.enabled = !state.config.monitor.enabled;
                        let _ = state.config.save();
                        if let Some(mgr) = &mut state.monitor_manager {
                            mgr.sync_config(&state.config.monitor);
                        }
                    });
                }
                IDM_SHOW_CANDIDATES => {
                    with_state(on_hotkey_triggered);
                }
                IDM_OPEN_SETTINGS => {
                    with_state(|state| open_settings(state));
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
    with_state(|state| on_foreground_window_changed_impl(state, fg_hwnd, 0));
}

fn on_foreground_window_changed_impl(state: &mut AppState, fg_hwnd: HWND, retry_count: u32) {
    // 0. 如果前台窗口是悬浮吸附条自身，直接忽略，避免用户点击或交互时将吸附条误隐藏
    if let Some(bar) = &state.floating_bar
        && bar.hwnd() == fg_hwnd
    {
        return;
    }

    let class_name = dialog::detector::get_window_class_name(fg_hwnd);

    // 1. 如果切入的是文件管理器，更新追踪器状态。
    //
    // 审计报告 P1-02：第三方管理器（DOpus / TotalCmd / XYplorer）的探测是跨进程
    // 有界调用（单次上限可达数百毫秒），绝不能在宿主消息线程里同步等待。
    // 这里只在“Win11 资源管理器极速路径”上保留同步（微秒级、无跨进程等待），
    // 其余一律投递后台线程扫描，结果携带 generation 回传，
    // 只有仍对应当前前台窗口的结果才会被采纳。
    if managers::tracker::is_file_manager_class(&class_name) {
        state.manager_scan_generation = state.manager_scan_generation.wrapping_add(1);
        let generation = state.manager_scan_generation;

        let mut resolved_fast = false;
        if state.config.enable_win11_tabs
            && class_name == "CabinetWClass"
            && let Some(folder) = managers::explorer::get_active_explorer_folder_fast()
        {
            state.tracker.note_manager_folder(folder, fg_hwnd);
            resolved_fast = true;
        }

        if !resolved_fast {
            let host_raw = HOST_HWND.load(Ordering::SeqCst) as usize;
            if host_raw != 0 {
                let config = state.config.clone();
                let hwnd_raw = fg_hwnd.0 as usize;
                thread::spawn(move || {
                    // 统一走后台 scanner：一次扫描同时刷新资源管理器与第三方候选快照，
                    // 结果既用于回填 tracker，也供吸附条候选列表复用，避免重复跨进程探测。
                    let budget = if managers::scanner::has_cached_result() {
                        managers::scanner::DEFAULT_BUDGET
                    } else {
                        managers::scanner::FIRST_SCAN_BUDGET
                    };
                    let candidates = managers::scanner::scan_within(
                        config.enable_win11_tabs,
                        config.enable_dopus,
                        config.enable_totalcmd,
                        config.enable_xyplorer,
                        budget,
                    );
                    let path = candidates
                        .into_iter()
                        .find(|c| c.is_active && managers::tracker::is_fast_valid_folder(&c.path))
                        .map(|c| c.path);
                    let result = ManagerScanResult {
                        generation,
                        hwnd_raw,
                        path,
                    };
                    // 经通道回传：宿主已退出（接收端已 drop）时 `send` 失败，
                    // 结果在此自然 drop，绝不泄漏；成功后再唤醒宿主排空队列。
                    if let Some(tx) = MANAGER_SCAN_TX.get()
                        && tx.send(result).is_ok()
                    {
                        let host_ptr = HOST_HWND.load(Ordering::SeqCst);
                        if !host_ptr.is_null() {
                            unsafe {
                                let _ = PostMessageW(
                                    Some(HWND(host_ptr as *mut _)),
                                    WM_QUICKPATH_MANAGER_SCAN_RESULT,
                                    WPARAM(0),
                                    LPARAM(0),
                                );
                            }
                        }
                    }
                });
            }
        }
    }

    // 2. 检查是否切入了文件对话框
    if let Some(dlg_info) = detect_file_dialog(fg_hwnd) {
        // 黑名单过滤（若配置了特定黑名单进程）
        if state.config.is_blacklisted(&dlg_info.process_name) {
            return;
        }

        let candidates = state.tracker.get_all_candidates(&state.config);
        // 是否已经包含“跨进程来源”（资源管理器/第三方管理器）。
        // 若没有，说明后台快照尚未就绪：先展示历史/收藏/系统目录，稍后补刷一次。
        let has_cross_process_source = candidates.iter().any(|c| {
            matches!(
                c.source.as_str(),
                "资源管理器" | "Directory Opus" | "Total Commander" | "XYplorer"
            )
        });

        crate::dialog::detector::log_debug(&format!(
            "MAIN: Dlg detected! hwnd=0x{:X} title='{}' bar_is_some={} candidates={}",
            dlg_info.hwnd.0 as usize,
            dlg_info.window_title,
            state.floating_bar.is_some(),
            candidates.len()
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
                dlg_info.kind,
            );
        } else {
            crate::dialog::detector::log_debug(
                "MAIN: 检测到对话框，但悬浮吸附条未创建（floating_bar=None），无法展示",
            );
        }

        // 冷启动补偿：首轮后台扫描可能尚未完成（例如用户没先聚焦过任何文件管理器），
        // 此时先展示历史/收藏/系统目录，并在扫描完成后请宿主重绘一次候选列表。
        // 每个对话框只补刷一次，避免重绘干扰用户操作。
        if !has_cross_process_source && state.refreshed_dialog != Some(dlg_info.hwnd) {
            state.refreshed_dialog = Some(dlg_info.hwnd);
            schedule_candidates_refresh(dlg_info.hwnd, &state.config);
        }

        // 避免对同一个对话框实例重复触发秒切
        if state.last_switched_dialog == Some(dlg_info.hwnd) {
            return;
        }

        // 4. 执行自动秒切逻辑 (AutoSwitch)
        if state.config.auto_switch_enabled
            && let Some(target_folder) = state.tracker.get_auto_switch_target(&state.config)
        {
            let should_inject = match dlg_info.kind {
                dialog::DialogKind::StandardWin32 => dlg_info.file_name_edit_hwnd.is_some(),
                dialog::DialogKind::WpsOffice => true,
            };

            if should_inject {
                let delay_ms = state.config.auto_switch_delay_ms;
                let dlg_raw = dlg_info.hwnd.0 as usize;
                let edit_raw = dlg_info.file_name_edit_hwnd.unwrap_or(dlg_info.hwnd).0 as usize;
                let target = target_folder.clone();
                let kind = dlg_info.kind;
                let already_retried = state.retried_dialog == Some(dlg_info.hwnd);
                let host_raw = HOST_HWND.load(Ordering::SeqCst) as usize;

                state.last_switched_dialog = Some(dlg_info.hwnd);
                state.last_switched_path = Some(target_folder.clone());
                state.config.record_history(&target_folder);

                // 延时秒切，确保宿主应用对话框完成初始化绘制与控件就绪
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(delay_ms));

                    // 注入**真实结果**回调：失败必须回报主线程，否则去重标记会让该对话框永远不再尝试秒切。
                    // 标准对话框为同步回调；WPS 自绘对话框为异步回调（启动线程不等于注入成功）。
                    let on_done: InjectDone = Box::new(move |injected| {
                        if !injected && host_raw != 0 {
                            unsafe {
                                let _ = PostMessageW(
                                    Some(HWND(host_raw as *mut _)),
                                    WM_QUICKPATH_INJECT_FAILED,
                                    WPARAM(dlg_raw),
                                    LPARAM(if already_retried { 1 } else { 0 }),
                                );
                            }
                        }
                    });

                    let accepted = inject_path_to_dialog(
                        HWND(dlg_raw as *mut _),
                        HWND(edit_raw as *mut _),
                        &target,
                        kind,
                        Some(on_done),
                    );
                    if !accepted {
                        dialog::detector::log_debug(
                            "MAIN: 注入请求未被受理（对话框已失效或路径非法）",
                        );
                    }
                });
            }
        }
    } else {
        // 如果检测未立即命中，分析当前窗口或其顶层根窗口是否属于潜在对话框（可能子控件尚未完成异步绘制）
        if retry_count < MAX_RETRIES {
            let root_hwnd = dialog::detector::find_dialog_root(fg_hwnd);
            let root_class = dialog::detector::get_window_class_name(root_hwnd);
            let root_title = dialog::detector::get_window_title(root_hwnd);
            let fg_title = dialog::detector::get_window_title(fg_hwnd);
            let (_pid, process_name) = dialog::detector::get_window_process_info(root_hwnd);

            let is_wps = dialog::detector::is_wps_process_name(&process_name);
            let is_potential_dialog = if is_wps {
                let is_wps_main =
                    dialog::detector::is_wps_main_window(root_hwnd, &root_class, &root_title)
                        || dialog::detector::is_wps_main_window(fg_hwnd, &class_name, &fg_title);
                !is_wps_main
                    && (root_class == "KcfdFileDialog"
                        || root_class.contains("Kcfd")
                        || root_class == "Qt5QWindowIcon"
                        || class_name == "Qt5QWindowIcon"
                        || root_class == "#32770"
                        || class_name == "#32770")
            } else {
                // 非 WPS 进程：严格仅针对尚未初始化完成的 #32770 窗口进行有界渐进式重试
                root_class == "#32770" || class_name == "#32770"
            };

            if is_potential_dialog {
                let hwnd_raw = fg_hwnd.0 as usize;
                let next_retry = retry_count + 1;
                let host_ptr = HOST_HWND.load(Ordering::SeqCst);
                if !host_ptr.is_null() {
                    let host_raw = host_ptr as usize;
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(RETRY_INTERVAL_MS));
                        if !HOST_HWND.load(Ordering::SeqCst).is_null() {
                            unsafe {
                                let host_h = HWND(host_raw as *mut _);
                                let _ = PostMessageW(
                                    Some(host_h),
                                    WM_QUICKPATH_RETRY_CHECK,
                                    WPARAM(hwnd_raw),
                                    LPARAM(next_retry as isize),
                                );
                            }
                        }
                    });
                }
                return;
            }
        }

        // 明确离开对话框或多次重试超时后仍非对话框时，隐藏悬浮吸附条；
        // 同时清理已销毁对话框残留的秒切标记，避免窗口句柄被复用后 AutoSwitch 被永久拦截
        if let Some(prev) = state.refreshed_dialog
            && !unsafe { IsWindow(Some(prev)).as_bool() }
        {
            state.refreshed_dialog = None;
        }
        if let Some(prev) = state.last_switched_dialog
            && !unsafe { IsWindow(Some(prev)).as_bool() }
        {
            state.last_switched_dialog = None;
            state.last_switched_path = None;
            state.retried_dialog = None;
        }
        if let Some(bar) = &state.floating_bar {
            bar.hide();
        }
    }
}

/// 后台等待一轮扫描完成后，请宿主消息线程重绘一次候选列表（P1-02 冷启动补偿）。
fn schedule_candidates_refresh(dialog_hwnd: HWND, config: &AppConfig) {
    let host_raw = HOST_HWND.load(Ordering::SeqCst) as usize;
    if host_raw == 0 {
        return;
    }
    let dlg_raw = dialog_hwnd.0 as usize;
    let budget = if managers::scanner::has_cached_result() {
        managers::scanner::DEFAULT_BUDGET
    } else {
        managers::scanner::FIRST_SCAN_BUDGET
    };
    let explorer = config.enable_win11_tabs;
    let dopus = config.enable_dopus;
    let totalcmd = config.enable_totalcmd;
    let xyplorer = config.enable_xyplorer;

    thread::spawn(move || {
        // 这轮扫描在后台线程完成（可能因挂起的管理器而用到整个预算），
        // 完成后只投递一个“重绘”消息，绝不阻塞宿主消息线程。
        let _ = managers::scanner::scan_within(explorer, dopus, totalcmd, xyplorer, budget);
        unsafe {
            let host_ptr = HOST_HWND.load(Ordering::SeqCst);
            if host_ptr.is_null() {
                return;
            }
            let _ = PostMessageW(
                Some(HWND(host_ptr as *mut _)),
                WM_QUICKPATH_CANDIDATES_REFRESH,
                WPARAM(dlg_raw),
                LPARAM(0),
            );
        }
    });
}

/// 处理 Win+E 接管请求（由低级键盘钩子投递，系统原生按键已被吞掉）。
///
/// 关键闭环：接管动作在工作线程执行，并在 800ms 内确认“新标签确实出现”；
/// 一旦接管动作未被接受、或标签始终未出现，立即补偿打开原生资源管理器窗口，
/// 保证每一次 Win+E 都有可见结果（绝不出现“按键被吞且毫无反应”）。
fn handle_wine_to_tab_request(state: &AppState, target: HWND) {
    let coordinator = state
        .tab_module
        .as_ref()
        .map(|tab_mod| tab_mod.coordinator());
    let Some(coordinator) = coordinator else {
        launch_native_explorer_window();
        return;
    };

    // HWND 不是 Send，跨线程只传递裸值，进入线程后再重建句柄
    let target_raw = target.0 as usize;

    thread::spawn(move || {
        let target = HWND(target_raw as *mut _);
        let com = coordinator.com_manager().clone();

        // 基线计数必须“确实读到”：STA 超时时 `get_all_tabs` 会退化为空列表，
        // 若把查询失败当 0，就会用宿主原有标签误判“新标签已出现”（报告 P1-01）。
        // 基线未知时不冒险新建，直接补偿原生窗口，保证每次 Win+E 都有可见结果。
        let Some(count_before) = com.get_tabs_count_for_hwnd_checked(target) else {
            dialog::detector::log_debug(
                "WINE_TO_TAB: 首个标签计数查询超时，无法证明新建成功，补偿打开原生窗口",
            );
            launch_native_explorer_window();
            return;
        };

        if !coordinator.wine_to_tab_in(target) {
            launch_native_explorer_window();
            return;
        }

        // 等待新标签在 Shell COM 中注册（最多 800ms）；
        // 只有“计数确实可读且大于基线”才认定为成功，超时/读不到均走原生兜底
        for _ in 0..8 {
            thread::sleep(Duration::from_millis(100));
            if let Some(count) = com.get_tabs_count_for_hwnd_checked(target)
                && count > count_before
            {
                return;
            }
        }
        dialog::detector::log_debug(&format!(
            "WINE_TO_TAB: host=0x{:X} 新标签未出现或计数不可读，补偿打开原生窗口",
            target_raw
        ));
        launch_native_explorer_window();
    });
}

/// 已吞掉 Win+E 但标签接管未成功时的补偿动作：打开一个原生资源管理器窗口。
/// 使用 System32 绝对路径，绝不使用裸程序名（避免命中 PATH 中的同名程序）。
fn launch_native_explorer_window() {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::HSTRING;

    let Some(exe) = win32::trusted::system_tool("explorer.exe") else {
        dialog::detector::log_debug("WINE_TO_TAB: 无可信 explorer.exe，无法补偿原生窗口");
        return;
    };
    let file = HSTRING::from(exe.as_os_str());
    unsafe {
        ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL);
    }
}

fn on_hotkey_triggered(state: &mut AppState) {
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
                dlg_info.kind,
            );
        }
    }
}

fn open_settings(state: &AppState) {
    if let Some(settings) = &state.settings_window {
        settings.show(state.config.clone());
    }
}

pub fn get_global_config() -> AppConfig {
    if let Some(config) = with_state(|state| state.config.clone()) {
        return config;
    }
    AppConfig::load()
}

pub fn update_global_config(new_config: AppConfig) {
    with_state(|state| {
        let old_hotkey = state.config.hotkey.clone();
        state.config = new_config.clone();

        if let Some(mgr) = &mut state.monitor_manager {
            mgr.sync_config(&new_config.monitor);
            mgr.update_language(new_config.language.clone());
        }

        if let Some(tab_mod) = &mut state.tab_module {
            let tab_cfg = tab::TabConfig {
                enabled: new_config.tab_module.enabled,
                wine_to_tab: new_config.tab_module.wine_to_tab,
                deduplication: new_config.tab_module.deduplication,
                double_click_to_duplicate: new_config.tab_module.double_click_to_duplicate,
            };
            tab_mod.update_config(tab_cfg);
            if new_config.tab_module.enabled && win32::version::is_tab_module_supported() {
                let host_hwnd = HOST_HWND.load(Ordering::SeqCst);
                if !host_hwnd.is_null() {
                    let _ = tab_mod.start(HWND(host_hwnd));
                }
            } else {
                tab_mod.stop();
            }
        }

        if old_hotkey != new_config.hotkey {
            let host_ptr = HOST_HWND.load(Ordering::SeqCst);
            if !host_ptr.is_null() {
                let h = HWND(host_ptr);
                let _ = win32::hotkey::register_global_hotkey(h, HOTKEY_ID, &new_config.hotkey);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个不含任何 Win32 资源的最小 `AppState`，仅用于验证 `with_state` 语义。
    fn test_state() -> AppState {
        let (_tx, rx) = std::sync::mpsc::channel();
        AppState {
            config: AppConfig::default(),
            tracker: WindowTracker::new(),
            floating_bar: None,
            settings_window: None,
            monitor_manager: None,
            tab_module: None,
            last_switched_dialog: None,
            last_switched_path: None,
            retried_dialog: None,
            manager_scan_generation: 0,
            refreshed_dialog: None,
            scan_rx: rx,
        }
    }

    fn reset_state() {
        APP_STATE.with(|cell| {
            let _ = cell.borrow_mut().take();
        });
    }

    fn install_state() {
        APP_STATE.with(|cell| *cell.borrow_mut() = Some(test_state()));
    }

    #[test]
    fn with_state_returns_none_when_empty() {
        reset_state();
        assert!(with_state(|state| state.manager_scan_generation).is_none());
    }

    #[test]
    fn with_state_reads_and_persists_mutation_after_install() {
        reset_state();
        install_state();

        assert_eq!(with_state(|state| state.manager_scan_generation), Some(0));

        let new_gen = with_state(|state| {
            state.manager_scan_generation = 42;
            state.manager_scan_generation
        });
        assert_eq!(new_gen, Some(42));
        // 修改跨调用持久：再次访问读到 42 而非初始 0
        assert_eq!(with_state(|state| state.manager_scan_generation), Some(42));

        reset_state();
    }

    #[test]
    fn with_state_take_then_reinstall_is_idempotent() {
        reset_state();
        install_state();
        assert!(with_state(|state| state.manager_scan_generation).is_some());

        // take() 后回到空
        let taken = APP_STATE.with(|cell| cell.borrow_mut().take());
        assert!(taken.is_some());
        assert!(with_state(|state| state.manager_scan_generation).is_none());

        // 可再次安装并访问（幂等）
        install_state();
        assert!(with_state(|state| state.manager_scan_generation).is_some());

        reset_state();
    }

    #[test]
    fn with_state_reentrant_access_degrades_to_none_without_panic() {
        reset_state();
        install_state();

        // 模拟 WndProc 重入：外层闭包仍持有 `AppState` 借用时再次进入 `with_state`。
        // 现实触发路径是「持借用期间泵消息」——如 `on_foreground_window_changed_impl`
        // 内的 `get_active_explorer_folder_fast()`（IShellWindows COM）/`detect_file_dialog`
        // （UIA）在 STA 上内部泵消息，从而重入 `host_wnd_proc` → 再次 `with_state`。
        // 这正是本次修复的可用性回归点：`[profile.release] panic = "abort"` 下
        // `borrow_mut()` 在重入时 panic，而访问器又经 `extern "system" fn` 跨 FFI 调用，
        // 会让整个托盘程序直接崩溃退出。修复后必须降级而非 abort：
        // 内层因重入返回 `None`，外层正常返回 `Some(None)`。
        let outer = with_state(|_| with_state(|state| state.manager_scan_generation));
        assert_eq!(outer, Some(None));

        reset_state();
    }
}
