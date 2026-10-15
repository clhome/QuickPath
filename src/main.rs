#![windows_subsystem = "windows"]

mod dialog;
mod managers;
mod monitor;
mod rules;
pub mod tab;
mod telemetry;
mod ui;
mod win32;

use dialog::{InjectDone, detect_file_dialog, inject_path_to_dialog};
use managers::WindowTracker;
use managers::scanner::ScanStatsSnapshot;
use monitor::MonitorManager;
use rules::AppConfig;
use tab::com::ComStatsSnapshot;
use ui::tray::{
    IDM_EXIT, IDM_OPEN_SETTINGS, IDM_PROBE_TOTALCMD, IDM_PROBE_XYPLORER, IDM_SHOW_CANDIDATES,
    IDM_TOGGLE_AUTOSTART, IDM_TOGGLE_AUTOSWITCH, IDM_TOGGLE_MONITOR, TrayIcon, WM_TRAY_CALLBACK,
};
use ui::{FloatingBar, SettingsWindow};
use win32::autostart::set_autostart;
use win32::events::{WM_QUICKPATH_FOREGROUND, WinEventHookGuard, get_active_foreground_window};

use std::cell::RefCell;
use std::sync::Mutex;
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
/// 用户显式触发的一次性探测结果回传（审计报告 U-01）
const WM_QUICKPATH_EXPLICIT_PROBE_RESULT: u32 = 0x0400 + 107;
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

/// 显式探测结果回传通道的发送端（审计报告 U-01），语义与 `MANAGER_SCAN_TX` 一致。
static EXPLICIT_PROBE_TX: OnceLock<Sender<ExplicitProbeResult>> = OnceLock::new();

/// 托盘菜单本次弹出时冻结的触发快照（审计报告 U-01A）。
///
/// 只在 `WM_RBUTTONUP`（弹出菜单前）写入，`WM_COMMAND` 时**取出**（单次消费），
/// 菜单关闭（选中或取消）后立即清空。因此同一 `request_id` 不可能被两条命令复用。
static PENDING_TRAY_TRIGGER: Mutex<[Option<managers::explicit_probe::TriggerSnapshot>; 2]> =
    Mutex::new([None, None]);

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

/// 前台事件计数：每 `TELEMETRY_LOG_EVERY` 次输出一条聚合遥测（低成本、限频，
/// 避免在每条消息上做统计与字符串格式化）。
static FOREGROUND_EVENTS: AtomicU64 = AtomicU64::new(0);

/// 聚合遥测输出频率（每多少次前台事件一条）。
const TELEMETRY_LOG_EVERY: u64 = 500;

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
    /// 过期 generation 丢弃计数：因「世代不匹配或前台窗口已变化」而未采纳的扫描结果。
    /// 放在 `AppState`（而非 scanner 侧）是因为采纳决策由宿主消息线程依据
    /// `manager_scan_generation` 与当前前台窗口做出，scanner 无从得知该判定。
    pub manager_scan_stale_dropped: u64,
    /// 已为哪个对话框补刷过一次候选列表（冷启动补偿只做一次）
    pub refreshed_dialog: Option<HWND>,
    /// 后台文件管理器扫描结果回传通道的接收端（宿主消息线程独占）
    scan_rx: Receiver<ManagerScanResult>,
    /// 显式探测结果回传通道的接收端（宿主消息线程独占，审计报告 U-01）
    probe_rx: Receiver<ExplicitProbeResult>,
}

/// 后台文件管理器扫描结果：经 `mpsc` 通道回传（报告 P1-02）。
struct ManagerScanResult {
    generation: u64,
    hwnd_raw: usize,
    path: Option<String>,
}

/// 一次显式探测的回传结果（审计报告 U-01）。
struct ExplicitProbeResult {
    manager: managers::explicit_probe::ScriptedManager,
    outcome: managers::explicit_probe::ExplicitProbeOutcome,
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

        // 显式探测结果回传通道（审计报告 U-01）：探测在后台线程执行，结果经通道回传，
        // 与扫描结果同构，宿主消息线程绝不因一次 500ms 级跨进程调用而卡顿。
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        let _ = EXPLICIT_PROBE_TX.set(probe_tx);

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
            manager_scan_stale_dropped: 0,
            refreshed_dialog: None,
            scan_rx,
            probe_rx,
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
        //
        // `GetMessageW` 返回的是三态 `BOOL`：>0 有消息、0 收到 `WM_QUIT`、-1 表示出错。
        // `BOOL(-1)` 非零，用 `as_bool()` 判真会把错误态当成“有消息”，使宿主循环在消息
        // 队列出错时无限空转、退出清理永不执行。必须与 `win32/clipboard.rs` 的 owner
        // 消息泵保持同一套语义：0 或 -1 都退出，-1 额外记一条结构化诊断。
        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 == 0 || ret.0 == -1 {
                if ret.0 == -1 {
                    crate::dialog::detector::log_debug(&format!(
                        "HOST: GetMessageW 返回 -1（err={}），消息循环退出",
                        windows::Win32::Foundation::GetLastError().0
                    ));
                }
                break;
            }
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
                    if payload.generation == generation && fg_raw == payload.hwnd_raw {
                        if let Some(path) = payload.path {
                            state
                                .tracker
                                .note_manager_folder(path, HWND(payload.hwnd_raw as *mut _));
                        }
                    } else {
                        // 过期 generation 丢弃：世代不匹配或前台窗口已变化，不再采纳（P-04 遥测）。
                        state.manager_scan_stale_dropped += 1;
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
                        // 审计报告 U-01 / U-01A：按钮只消费「与当前对话框打开事件绑定」的
                        // 不可变快照（generation 复用），授权开关只决定快照是否冻结，
                        // 这里不发起任何探测。
                        let probe_targets = managers::explicit_probe::bar_probe_targets(
                            dlg_info.hwnd.0 as usize,
                            [state.config.enable_totalcmd, state.config.enable_xyplorer],
                        );
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
                                probe_targets,
                            );
                        }
                    }
                });
            }
            LRESULT(0)
        }
        WM_QUICKPATH_EXPLICIT_PROBE_RESULT => {
            // 显式探测已在后台线程完成，这里只排空通道并应用结果（零等待）。
            with_state(|state| {
                let results: Vec<ExplicitProbeResult> = state.probe_rx.try_iter().collect();
                for result in results {
                    apply_explicit_probe_result(state, result);
                }
                // 探测结束：让吸附条重绘，清除忙碌态并恢复按钮可点。
                if let Some(bar) = &state.floating_bar {
                    bar.invalidate();
                }
            });
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
                // 审计报告 U-01A（托盘方案）：在**弹出菜单之前**严格取当前前台窗口。
                // 菜单弹出会把前台短暂交给 QuickPath 自身窗口，因此必须在
                // `SetForegroundWindow` 之前抓取；前台不是目标管理器时对应菜单项置灰，
                // **不做任何墙钟回退**（600 秒旧记录已删除）。
                let snapshots = [
                    managers::explicit_probe::capture_foreground_snapshot(
                        managers::explicit_probe::ScriptedManager::TotalCmd,
                    ),
                    managers::explicit_probe::capture_foreground_snapshot(
                        managers::explicit_probe::ScriptedManager::XYplorer,
                    ),
                ];
                store_pending_tray_trigger(snapshots);
                // 先取出菜单所需配置再释放借用：`TrackPopupMenu` 会进入模态消息
                // 循环，期间可能重入宿主窗口过程；若此时仍持有 `APP_STATE` 借用
                // 会触发 `RefCell` panic。
                let menu_ctx = with_state(|state| {
                    (
                        state.config.auto_switch_enabled,
                        state.config.autostart_enabled,
                        state.config.monitor.enabled,
                        explicit_probe_menu_ready(state, &snapshots),
                        state.config.language.clone(),
                        state.config.hotkey.clone(),
                    )
                });
                if let Some((auto_switch, autostart, monitor, probe_ready, language, hotkey)) =
                    menu_ctx
                {
                    TrayIcon::show_context_menu(
                        hwnd,
                        auto_switch,
                        autostart,
                        monitor,
                        probe_ready,
                        language,
                        &hotkey,
                    );
                }
                // 菜单关闭（选中或取消）后清空快照：同一 request_id 只能被消费一次。
                clear_pending_tray_trigger();
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
                IDM_PROBE_TOTALCMD => {
                    request_explicit_probe_with_snapshot(take_pending_tray_trigger(0));
                }
                IDM_PROBE_XYPLORER => {
                    request_explicit_probe_with_snapshot(take_pending_tray_trigger(1));
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

/// 构造一条聚合遥测日志（P-04）：扫描四指标 + 过期 generation 丢弃 + STA COM worker 状态。
/// 纯函数，便于单测（尤其是 COM 不可用时的行为）。
fn format_scan_telemetry(
    scan: &ScanStatsSnapshot,
    stale_dropped: u64,
    com: Option<&ComStatsSnapshot>,
) -> String {
    let com_part = match com {
        Some(stats) => stats.describe(),
        None => "不可用（tab_module 未创建或已停止）".to_string(),
    };
    format!(
        "SCAN_TELEMETRY: {} stale_generation_dropped={} | COM_STA: {}",
        scan.describe(),
        stale_dropped,
        com_part
    )
}

/// 输出一条聚合遥测（限频调用）。COM stats 从 `tab_module` 取；实例不存在时如实输出“不可用”。
fn log_scan_telemetry(state: &AppState) {
    let scan = managers::scanner::stats();
    let com = state
        .tab_module
        .as_ref()
        .map(|tab_mod| tab_mod.coordinator().com_manager().stats());
    crate::dialog::detector::log_debug(&format_scan_telemetry(
        &scan,
        state.manager_scan_stale_dropped,
        com.as_ref(),
    ));
}

fn on_foreground_window_changed_impl(state: &mut AppState, fg_hwnd: HWND, retry_count: u32) {
    // 审计报告 U-01：记录「用户刚刚以哪个第三方管理器窗口为目标」。
    // 显式触发的目标解析优先取当前前台窗口，其次才回退到这份**有界时效**的记录，
    // 因此这里只做记，不做任何探测、不碰剪贴板。
    managers::explicit_probe::note_foreground_window(fg_hwnd);

    // P-04 聚合遥测：限频输出（每 500 次前台事件一条），避免在每条消息上做统计与格式化。
    let event_seq = FOREGROUND_EVENTS.fetch_add(1, Ordering::Relaxed) + 1;
    if event_seq.is_multiple_of(TELEMETRY_LOG_EVERY) {
        log_scan_telemetry(state);
    }

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
        // 审计报告 U-01A：吸附条是 `WS_EX_NOACTIVATE`，点击时前台永远是对话框，
        // 因此目标改用「对话框打开事件冻结的最近来源快照」（generation 绑定，
        // 对话框变化即重新冻结），文档已如实标注它不是严格前台触发。
        let probe_targets = managers::explicit_probe::bar_probe_targets(
            dlg_info.hwnd.0 as usize,
            [state.config.enable_totalcmd, state.config.enable_xyplorer],
        );
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
                probe_targets,
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

    thread::spawn(move || {
        // 这轮扫描在后台线程完成（可能因挂起的管理器而用到整个预算），
        // 完成后只投递一个“重绘”消息，绝不阻塞宿主消息线程。
        let _ = managers::scanner::scan_within(explorer, dopus, budget);
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

/// Win+E 接管后等待新标签在 Shell COM 中注册的轮询次数与间隔（合计 800ms）。
/// 只有“计数确实可读且严格大于基线”才判定成功；轮询耗尽仍不满足则走原生兜底。
const WINE_TO_TAB_POLL_ATTEMPTS: usize = 8;
const WINE_TO_TAB_POLL_INTERVAL_MS: u64 = 100;

/// Win+E 接管序列的决策结果。
///
/// 核心不变量：除 `NewTab` 外的任何结果都**恰好**调用过一次补偿动作，
/// 即“每一次 Win+E 都必须有可见结果”，绝不静默失败、也不重复弹窗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WineToTabOutcome {
    /// 新标签已在 Shell COM 中确认出现（计数可读且大于基线），无需补偿。
    NewTab,
    /// 未能确认新标签出现，已调用一次补偿动作打开原生资源管理器窗口。
    NativeFallback(WineToTabFallback),
}

/// 走原生兜底的具体原因（只影响诊断日志，不改变“补偿恰好一次”的动作）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WineToTabFallback {
    /// 基线计数不可读（COM 超时 / STA 不可用）：不冒险新建，直接补偿。
    BaselineUnknown,
    /// 接管动作未被受理（模块禁用 / 目标非资源管理器窗口 / 触发失败）。
    NotAccepted,
    /// 接管已受理，但轮询期内计数始终不可读或不大于基线。
    CountNotIncreased,
}

/// Win+E 接管的可注入决策序列（纯决策逻辑：无 I/O、无 Win32 调用）。
///
/// 语义与内联实现逐条等价：
/// - 基线 `count_before == None`（COM 超时 / 不可读）时**不**调用 `try_accept`，
///   直接补偿一次（绝不冒险新建）；
/// - `try_accept()` 返回 false（接管被拒）时补偿一次；
/// - 接管受理后最多轮询 `WINE_TO_TAB_POLL_ATTEMPTS` 次，每次先 `wait()` 再
///   `count_after()`；只有“计数确实可读且严格大于基线”才判定成功并提前返回；
/// - 轮询耗尽仍不满足则补偿一次。
///
/// 生产路径传 `thread::sleep(WINE_TO_TAB_POLL_INTERVAL_MS)` 与
/// `launch_native_explorer_window`；测试传 no-op 等待与计数闭包，
/// 从而在不弹窗、不依赖真实 COM/Explorer 的前提下验证每条失败分支。
fn run_wine_to_tab_sequence(
    count_before: Option<usize>,
    mut try_accept: impl FnMut() -> bool,
    mut count_after: impl FnMut() -> Option<usize>,
    mut wait: impl FnMut(),
    mut compensate: impl FnMut(),
) -> WineToTabOutcome {
    let Some(count_before) = count_before else {
        // 基线未知：不冒险新建，直接补偿原生窗口
        compensate();
        return WineToTabOutcome::NativeFallback(WineToTabFallback::BaselineUnknown);
    };

    if !try_accept() {
        compensate();
        return WineToTabOutcome::NativeFallback(WineToTabFallback::NotAccepted);
    }

    for _ in 0..WINE_TO_TAB_POLL_ATTEMPTS {
        wait();
        if let Some(count) = count_after()
            && count > count_before
        {
            return WineToTabOutcome::NewTab;
        }
    }

    compensate();
    WineToTabOutcome::NativeFallback(WineToTabFallback::CountNotIncreased)
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
        let count_before = com.get_tabs_count_for_hwnd_checked(target);

        let outcome = run_wine_to_tab_sequence(
            count_before,
            // 接管动作延迟到「基线可读」之后才执行（由决策函数保证）
            || coordinator.wine_to_tab_in(target),
            || com.get_tabs_count_for_hwnd_checked(target),
            || thread::sleep(Duration::from_millis(WINE_TO_TAB_POLL_INTERVAL_MS)),
            launch_native_explorer_window,
        );

        match outcome {
            WineToTabOutcome::NewTab => {}
            WineToTabOutcome::NativeFallback(WineToTabFallback::BaselineUnknown) => {
                dialog::detector::log_debug(
                    "WINE_TO_TAB: 首个标签计数查询超时，无法证明新建成功，补偿打开原生窗口",
                );
            }
            WineToTabOutcome::NativeFallback(WineToTabFallback::CountNotIncreased) => {
                dialog::detector::log_debug(&format!(
                    "WINE_TO_TAB: host=0x{:X} 新标签未出现或计数不可读，补偿打开原生窗口",
                    target_raw
                ));
            }
            WineToTabOutcome::NativeFallback(WineToTabFallback::NotAccepted) => {}
        }
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
        let probe_targets = managers::explicit_probe::bar_probe_targets(
            dlg_info.hwnd.0 as usize,
            [state.config.enable_totalcmd, state.config.enable_xyplorer],
        );
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
                probe_targets,
            );
        }
    }
}

fn open_settings(state: &AppState) {
    if let Some(settings) = &state.settings_window {
        settings.show(state.config.clone());
    }
}

/// 托盘菜单里两个「获取当前路径」条目的可用性（`[Total Commander, XYplorer]`）。
///
/// 只有「已授权 **且** 当前没有探测在跑 **且** 该快照有效」时才可点击；否则置灰
/// （审计报告 U-01 / U-01A / P0-06）。`snapshots` 来自**右键那一刻**的严格前台抓取。
fn explicit_probe_menu_ready(
    state: &AppState,
    snapshots: &[Option<managers::explicit_probe::TriggerSnapshot>; 2],
) -> [bool; 2] {
    let authorized = [state.config.enable_totalcmd, state.config.enable_xyplorer];
    // P0-06 → P0-07:已有剪贴板事务在跑时两个入口同时置灰。
    // 读唯一事实来源 `clipboard::is_transaction_in_flight()` —— 事务自持 lease,
    // 因此 **WPS 进入剪贴板兜底或文件名恢复事务时**也会置灰。
    // 注意范围:WPS 注入若经 UIA `SetValue` 完成则不建剪贴板事务,此处仍为可点
    // (报告 D-01C);两把 lease 的职责不同,UI 置灰只是反馈而非安全边界。
    let in_flight = crate::win32::clipboard::is_transaction_in_flight();
    [
        authorized[0] && !in_flight && snapshots[0].is_some(),
        authorized[1] && !in_flight && snapshots[1].is_some(),
    ]
}

/// 写入本次托盘菜单的触发快照。
fn store_pending_tray_trigger(snapshots: [Option<managers::explicit_probe::TriggerSnapshot>; 2]) {
    let mut pending = PENDING_TRAY_TRIGGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *pending = snapshots;
}

/// 取出本次托盘菜单的快照（单次消费：取出后立即置空）。
fn take_pending_tray_trigger(index: usize) -> Option<managers::explicit_probe::TriggerSnapshot> {
    let mut pending = PENDING_TRAY_TRIGGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.get_mut(index).and_then(Option::take)
}

/// 菜单关闭（选中或取消）后清空快照。
fn clear_pending_tray_trigger() {
    let mut pending = PENDING_TRAY_TRIGGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *pending = [None, None];
}

/// 用户显式触发一次第三方管理器路径探测（托盘菜单 / 悬浮吸附条按钮，审计报告 U-01）。
///
/// `snapshot` 必须是 UI 触发事件冻结的**不可变快照**（U-01A）：本函数不再去猜目标窗口。
///
/// - **授权开关只授权**：未授权时直接返回，绝不发起探测（验收标准 3）；
/// - 探测在后台线程执行（单次协议消息上限 `MESSAGE_TIMEOUT_MS`），结果经通道回传，
///   绝不阻塞宿主消息线程；
/// - 进程级独占 lease 在探测线程内获取（P0-06）：已有探测在跑时第二个请求立即被拒绝。
pub fn request_explicit_probe_with_snapshot(
    snapshot: Option<managers::explicit_probe::TriggerSnapshot>,
) {
    let Some(snapshot) = snapshot else {
        dialog::detector::log_debug(
            "EXPLICIT_PROBE_NO_SNAPSHOT: 没有有效的触发快照（菜单已取消或未在管理器窗口触发），本次未发起探测",
        );
        return;
    };
    // 读配置用 `get_global_config()` 而不是 `with_state`：吸附条的鼠标处理就在宿主消息
    // 线程上，若此刻恰好有活动借用（例如宿主正在 `with_state` 闭包里泵消息），
    // `with_state` 会降级为 `None` 并让本次点击静默失效；`get_global_config()` 在
    // 状态忙时会回退到磁盘配置，保证“已授权”判定不会因重入而丢失。
    let config = crate::get_global_config();
    let authorized = match snapshot.manager {
        managers::explicit_probe::ScriptedManager::TotalCmd => config.enable_totalcmd,
        managers::explicit_probe::ScriptedManager::XYplorer => config.enable_xyplorer,
    };
    if !authorized {
        dialog::detector::log_debug(&format!(
            "EXPLICIT_PROBE_NOT_AUTHORIZED({}): 用户尚未在设置中心授权该能力，本次未发起探测",
            snapshot.manager.label()
        ));
        return;
    }

    let host_raw = HOST_HWND.load(Ordering::SeqCst) as usize;
    if host_raw == 0 {
        return;
    }

    thread::spawn(move || {
        let outcome = managers::explicit_probe::probe_with_snapshot(&snapshot, true);
        if let Some(tx) = EXPLICIT_PROBE_TX.get()
            && tx
                .send(ExplicitProbeResult {
                    manager: snapshot.manager,
                    outcome,
                })
                .is_ok()
        {
            let host_ptr = HOST_HWND.load(Ordering::SeqCst);
            if !host_ptr.is_null() {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(host_ptr as *mut _)),
                        WM_QUICKPATH_EXPLICIT_PROBE_RESULT,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        }
    });
}

/// 应用一次显式探测的结果（宿主消息线程）。
///
/// - 命中路径且当前有活着的目标对话框 → 直接注入该目录（“从 TC 获取当前路径”的语义就是
///   让对话框跳过去），同时记入历史；
/// - 没有对话框 → **只**记入历史供下次候选，绝不偷偷改动对话框；
/// - 其余结局（未授权 / 无可归因目标 / 冷却 / 未取得可证明路径）只记结构化日志，
///   日志中**不出现**任何路径正文与剪贴板内容。
fn apply_explicit_probe_result(state: &mut AppState, result: ExplicitProbeResult) {
    use managers::explicit_probe::ExplicitProbeOutcome as Outcome;

    let label = result.manager.label();
    let lang = state.config.language.clone();
    // 审计报告 D-01A：未取得路径的结局必须给出**可见反馈**（不阻塞、不回显任何剪贴板正文）。
    let notify_failure = |lang: &rules::Language| {
        ui::tray::notify("QuickPath", rules::I18n::tray_probe_result(lang, false));
    };
    let path = match result.outcome {
        Outcome::Path(path) => path,
        Outcome::NotAuthorized => {
            dialog::detector::log_debug(&format!("EXPLICIT_PROBE({}): 未授权，已放弃", label));
            return;
        }
        Outcome::NoForegroundTarget => {
            dialog::detector::log_debug(&format!(
                "EXPLICIT_PROBE({}): 目标窗口已不可归因（关闭 / 易主 / 进程重启），已取消",
                label
            ));
            notify_failure(&lang);
            return;
        }
        Outcome::CoolingDown => {
            dialog::detector::log_debug(&format!(
                "EXPLICIT_PROBE({}): 处于失败冷却窗口，已跳过",
                label
            ));
            notify_failure(&lang);
            return;
        }
        Outcome::AlreadyRunning => {
            // P0-06：进程级独占 lease 拒绝。这不是错误，只是一次重复点击。
            dialog::detector::log_debug(&format!(
                "EXPLICIT_PROBE({}): 已有一次探测在进行，本次请求被拒绝（未触碰剪贴板）",
                label
            ));
            return;
        }
        Outcome::NoPath => {
            dialog::detector::log_debug(&format!(
                "EXPLICIT_PROBE({}): 未取得可证明的路径（超时 / 来源不可证明），已放弃",
                label
            ));
            notify_failure(&lang);
            return;
        }
    };

    dialog::detector::log_debug(&format!("EXPLICIT_PROBE({}): 已取得路径，准备应用", label));

    let mut injected = false;
    if let Some(bar) = &state.floating_bar
        && let Some((dlg, edit, kind)) = bar.target_dialog()
        && dialog_still_active(dlg)
    {
        inject_path_to_dialog(dlg, edit, &path, kind, None);
        injected = true;
    }
    state.config.record_history(&path);
    if !injected {
        // 严格前台托盘流程：此刻没有可注入的对话框，路径只进历史，
        // 因此必须用气泡给出可见反馈（审计报告 D-01A 验收标准 4）。
        ui::tray::notify("QuickPath", rules::I18n::tray_probe_result(&lang, true));
    }
}

/// 目标对话框是否仍与当前前台相关（与候选刷新的判定保持一致）。
fn dialog_still_active(dlg: HWND) -> bool {
    if !unsafe { IsWindow(Some(dlg)).as_bool() } {
        return false;
    }
    let current_fg = get_active_foreground_window();
    current_fg == dlg
        || dialog::detector::find_dialog_root(current_fg) == dialog::detector::find_dialog_root(dlg)
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
        let (_probe_tx, probe_rx) = std::sync::mpsc::channel();
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
            manager_scan_stale_dropped: 0,
            refreshed_dialog: None,
            scan_rx: rx,
            probe_rx,
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

    /// P-04：COM 不可用时聚合遥测必须如实标注“不可用”，不得伪造数据。
    #[test]
    fn test_format_scan_telemetry_reports_unavailable_com() {
        let scan = managers::scanner::stats();
        let line = format_scan_telemetry(&scan, 7, None);
        assert!(line.contains("stale_generation_dropped=7"), "{line}");
        assert!(line.contains("不可用"), "COM 不可用时必须如实标注：{line}");
    }

    /// P-04：COM 可用时聚合遥测必须包含重建次数/降级状态与分位。
    #[test]
    fn test_format_scan_telemetry_includes_com_stats() {
        let com = ComStatsSnapshot {
            requests: 3,
            timeouts: 1,
            worker_rebuilds: 2,
            queue_depth: 0,
            p50_ms: 1.0,
            p95_ms: 2.0,
            p99_ms: 3.0,
            degraded: true,
            rebuilds_since_success: 1,
            skipped_while_degraded: 4,
            degraded_rounds: 1,
        };
        let scan = managers::scanner::stats();
        let line = format_scan_telemetry(&scan, 0, Some(&com));
        assert!(line.contains("rebuild=2"), "{line}");
        assert!(line.contains("degraded=true"), "{line}");
        assert!(line.contains("p50="), "{line}");
    }

    /// 一次 `run_wine_to_tab_sequence` 调用观测到的各注入点调用次数。
    struct WineToTabProbe {
        outcome: WineToTabOutcome,
        accept_calls: usize,
        wait_calls: usize,
        count_calls: usize,
        compensate_calls: usize,
    }

    /// 用计数闭包驱动决策函数：`after_counts` 按轮询顺序给出 `count_after` 的返回值，
    /// 越界（调用次数超过切片长度）一律返回 `None`（模拟“计数始终不可读”）。
    fn run_wine_to_tab_probe(
        count_before: Option<usize>,
        accept_result: bool,
        after_counts: &[Option<usize>],
    ) -> WineToTabProbe {
        use std::cell::Cell;
        let accept_calls = Cell::new(0usize);
        let wait_calls = Cell::new(0usize);
        let count_calls = Cell::new(0usize);
        let compensate_calls = Cell::new(0usize);
        let mut idx = 0usize;
        let outcome = run_wine_to_tab_sequence(
            count_before,
            || {
                accept_calls.set(accept_calls.get() + 1);
                accept_result
            },
            || {
                count_calls.set(count_calls.get() + 1);
                let value = after_counts.get(idx).copied().flatten();
                idx += 1;
                value
            },
            || {
                wait_calls.set(wait_calls.get() + 1);
            },
            || {
                compensate_calls.set(compensate_calls.get() + 1);
            },
        );
        WineToTabProbe {
            outcome,
            accept_calls: accept_calls.get(),
            wait_calls: wait_calls.get(),
            count_calls: count_calls.get(),
            compensate_calls: compensate_calls.get(),
        }
    }

    /// 报告 §10-4：基线计数未知（模拟 COM 超时）→ 必须原生兜底，
    /// 补偿恰好一次，且**不尝试新建**（`try_accept` 从未被调用）。
    #[test]
    fn wine_to_tab_baseline_unknown_falls_back_without_trying_new_tab() {
        let probe = run_wine_to_tab_probe(None, true, &[]);
        assert_eq!(
            probe.outcome,
            WineToTabOutcome::NativeFallback(WineToTabFallback::BaselineUnknown)
        );
        assert_eq!(probe.compensate_calls, 1, "基线未知必须补偿恰好一次");
        assert_eq!(probe.accept_calls, 0, "基线未知时绝不尝试新建标签");
        assert_eq!(probe.wait_calls, 0, "基线未知时不进入轮询");
        assert_eq!(probe.count_calls, 0, "基线未知时不再查计数");
    }

    /// 报告 §10-4：接管被拒（`wine_to_tab_in` 返回 false）→ 原生兜底 1 次，不进入轮询。
    #[test]
    fn wine_to_tab_not_accepted_falls_back_once() {
        let probe = run_wine_to_tab_probe(Some(2), false, &[Some(3); 8]);
        assert_eq!(
            probe.outcome,
            WineToTabOutcome::NativeFallback(WineToTabFallback::NotAccepted)
        );
        assert_eq!(probe.compensate_calls, 1);
        assert_eq!(probe.accept_calls, 1);
        assert_eq!(probe.wait_calls, 0, "接管被拒不应进入轮询");
    }

    /// 报告 §10-4：接管受理但计数始终不可读（COM 挂起）→ 轮询满 8 次后原生兜底 1 次。
    #[test]
    fn wine_to_tab_count_always_unreadable_falls_back_once() {
        let probe = run_wine_to_tab_probe(Some(2), true, &[]);
        assert_eq!(
            probe.outcome,
            WineToTabOutcome::NativeFallback(WineToTabFallback::CountNotIncreased)
        );
        assert_eq!(probe.compensate_calls, 1);
        assert_eq!(probe.wait_calls, WINE_TO_TAB_POLL_ATTEMPTS, "必须轮询 8 次");
        assert_eq!(probe.count_calls, WINE_TO_TAB_POLL_ATTEMPTS);
    }

    /// 报告 §10-4：接管受理但计数始终等于基线（模拟 Explorer 重启/源窗口关闭后计数不增）
    /// → 原生兜底 1 次。
    #[test]
    fn wine_to_tab_count_never_increases_falls_back_once() {
        let probe = run_wine_to_tab_probe(Some(3), true, &[Some(3); 8]);
        assert_eq!(
            probe.outcome,
            WineToTabOutcome::NativeFallback(WineToTabFallback::CountNotIncreased)
        );
        assert_eq!(probe.compensate_calls, 1);
        assert_eq!(probe.wait_calls, WINE_TO_TAB_POLL_ATTEMPTS);
    }

    /// 报告 §10-4：接管受理，计数先不可读、后大于基线 → 判定成功、补偿 0 次、提前返回。
    #[test]
    fn wine_to_tab_count_increases_after_unreadable_succeeds_without_compensation() {
        let probe = run_wine_to_tab_probe(Some(1), true, &[None, Some(2)]);
        assert_eq!(probe.outcome, WineToTabOutcome::NewTab);
        assert_eq!(probe.compensate_calls, 0, "判定成功后绝不补偿");
        assert_eq!(probe.wait_calls, 2, "第二轮才确认，应提前返回");
        assert_eq!(probe.count_calls, 2);
    }

    /// 报告 §10-4：接管受理且计数立即大于基线 → 判定成功、补偿 0 次、仅等待一次。
    #[test]
    fn wine_to_tab_count_increases_immediately_succeeds_without_compensation() {
        let probe = run_wine_to_tab_probe(Some(1), true, &[Some(2)]);
        assert_eq!(probe.outcome, WineToTabOutcome::NewTab);
        assert_eq!(probe.compensate_calls, 0);
        assert_eq!(probe.wait_calls, 1, "首轮即确认，只等待一次");
        assert_eq!(probe.count_calls, 1);
    }

    /// 核心不变量（报告 §10-4）：每一种失败输入下补偿次数**恰好为 1**——
    /// 既不是 0 次“静默失败”，也不是多次重复弹窗。
    #[test]
    fn wine_to_tab_failure_inputs_compensate_exactly_once() {
        type FailingInput<'a> = (&'a str, Option<usize>, bool, &'a [Option<usize>]);
        let failing_inputs: &[FailingInput] = &[
            ("基线未知", None, true, &[]),
            ("接管被拒", Some(2), false, &[Some(3); 8]),
            ("计数始终不可读", Some(2), true, &[]),
            ("计数始终等于基线", Some(3), true, &[Some(3); 8]),
            ("计数始终小于基线", Some(5), true, &[Some(2); 8]),
        ];
        for (name, count_before, accept_result, after_counts) in failing_inputs {
            let probe = run_wine_to_tab_probe(*count_before, *accept_result, after_counts);
            assert!(
                matches!(probe.outcome, WineToTabOutcome::NativeFallback(_)),
                "{name}: 失败输入必须走原生兜底，实际 {:?}",
                probe.outcome
            );
            assert_eq!(probe.compensate_calls, 1, "{name}: 补偿必须恰好一次");
        }
    }

    /// 架构不变式守卫：消息泵**不得**用 `.as_bool()` 判真 `GetMessageW` 的返回值。
    ///
    /// `GetMessageW` 的三态 `BOOL`（>0 有消息 / 0 收到 `WM_QUIT` / -1 出错）中 `-1` 非零，
    /// 判真会把错误态当成“有消息”，让消息泵在队列出错时无限空转、退出清理永不执行。
    /// 全仓消息泵只有两处（本文件宿主循环、`win32/clipboard.rs` owner 泵，已由 grep 确认），
    /// 两处都必须显式按 0 / -1 判定。
    #[test]
    fn test_message_pump_result_is_never_judged_with_as_bool() {
        // 探针在运行时拼装，避免本测试源码自身出现连续 token 而自命中
        let call = concat!("GetMessage", "W");
        let truthy = concat!(".as_", "bool()");
        let sources = [
            ("main.rs", include_str!("main.rs")),
            ("win32/clipboard.rs", include_str!("win32/clipboard.rs")),
        ];

        for (name, source) in sources {
            // 先剔除整行注释，避免架构说明文字本身触发误报
            let code = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");

            let mut from = 0usize;
            while let Some(offset) = code[from..].find(call) {
                let start = from + offset;
                // 只检查本条语句（到最近的 `;` 为止），避免跨语句误判
                let end = code[start..]
                    .find(';')
                    .map_or(code.len(), |semi| start + semi);
                let statement = &code[start..end];
                assert!(
                    !statement.contains(truthy),
                    "{name} 对 {call} 的返回值使用了 {truthy}（三态 BOOL，-1 是错误态，\
                     判真会让消息泵在错误态死循环）。命中片段：{statement}"
                );
                from = end.max(start + call.len());
            }
        }
    }

    /// 审计报告 U-01A 验收标准 4：一次快照不能被两条命令或两次点击复用。
    ///
    /// 托盘菜单的快照只在 `WM_RBUTTONUP` 写入，`WM_COMMAND` **取出**（单次消费），
    /// 菜单关闭（选中或取消）后立即清空。三个动作共用一个进程级静态，
    /// 因此必须写在**同一个用例**里（并行执行时不会互相打断）。
    #[test]
    fn test_tray_trigger_snapshot_is_single_use() {
        use managers::explicit_probe::{ScriptedManager, TriggerSnapshot};
        use std::time::Instant;

        fn snapshot(manager: ScriptedManager, request_id: u64) -> TriggerSnapshot {
            TriggerSnapshot {
                hwnd_raw: 0x1234,
                pid: 42,
                manager,
                captured_at: Instant::now(),
                request_id,
            }
        }

        clear_pending_tray_trigger();
        store_pending_tray_trigger([
            Some(snapshot(ScriptedManager::TotalCmd, 7)),
            Some(snapshot(ScriptedManager::XYplorer, 8)),
        ]);
        assert_eq!(
            take_pending_tray_trigger(0).map(|s| s.request_id),
            Some(7),
            "第一次消费必须拿到本菜单冻结的快照"
        );
        assert!(
            take_pending_tray_trigger(0).is_none(),
            "同一快照不得被第二条命令复用"
        );
        assert_eq!(
            take_pending_tray_trigger(1).map(|s| s.request_id),
            Some(8),
            "两个管理器的快照互不影响"
        );
        assert!(take_pending_tray_trigger(1).is_none());

        // 菜单关闭（选中或取消）后必须清空：残留快照不得在下次弹菜单前被消费
        store_pending_tray_trigger([Some(snapshot(ScriptedManager::TotalCmd, 11)), None]);
        clear_pending_tray_trigger();
        assert!(
            take_pending_tray_trigger(0).is_none(),
            "菜单关闭后残留快照必须失效"
        );
        clear_pending_tray_trigger();
    }
}
