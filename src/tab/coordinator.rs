#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    PostMessageW, SetForegroundWindow, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, WM_CLOSE,
};

use super::com::{is_physical_dir_path, ShellComManager};
use super::select::{async_select_file_item, try_select_tab_item};
use super::uia::{trigger_new_tab, trigger_new_tab_safe};
use windows::core::BOOL;

/// 多标签增强配置
#[derive(Debug, Clone)]
pub struct TabConfig {
    pub enabled: bool,
    pub wine_to_tab: bool,
    pub deduplication: bool,
    pub double_click_to_duplicate: bool,
}

impl Default for TabConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wine_to_tab: true,
            deduplication: true,
            double_click_to_duplicate: true,
        }
    }
}

pub struct TabCoordinator {
    config: Arc<parking_lot_or_std::RwLock<TabConfig>>,
    com_manager: Arc<ShellComManager>,
    /// 记录最近一次由于“新建标签页继承当前目录”预存的目录路径及时间戳
    pending_inherit_path: Arc<parking_lot_or_std::Mutex<Option<(String, Instant)>>>,
    /// 是否正处于旁路去重模式（防止新建同路径 Tab 时被去重拦截）
    bypass_deduplication: Arc<AtomicBool>,
    /// 正在被合并捕获处理中的外部窗口集合（防重触发）
    processing_new_windows: Arc<parking_lot_or_std::Mutex<std::collections::HashSet<usize>>>,
}

mod parking_lot_or_std {
    pub use std::sync::{Mutex, RwLock};
}

impl TabCoordinator {
    pub fn new(com_manager: Arc<ShellComManager>) -> Self {
        Self {
            config: Arc::new(parking_lot_or_std::RwLock::new(TabConfig::default())),
            com_manager,
            pending_inherit_path: Arc::new(parking_lot_or_std::Mutex::new(None)),
            bypass_deduplication: Arc::new(AtomicBool::new(false)),
            processing_new_windows: Arc::new(parking_lot_or_std::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// 更新配置
    pub fn update_config(&self, new_cfg: TabConfig) {
        if let Ok(mut lock) = self.config.write() {
            *lock = new_cfg;
        }
    }

    pub fn get_config(&self) -> TabConfig {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// 检查指定的窗口句柄是否为资源管理器顶级窗口 (CabinetWClass)
    pub fn is_explorer_window(hwnd: HWND) -> bool {
        if hwnd.0.is_null() || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return false;
        }
        let mut buf = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len > 0 {
            let cls = String::from_utf16_lossy(&buf[..len as usize]);
            return cls == "CabinetWClass";
        }
        false
    }

    /// 预存当前活动目录（功能已废止，保留空实现兼容）
    pub fn prepare_new_tab_inheritance(&self, _explorer_hwnd: HWND) {
    }

    /// 用户双击标签页克隆特定标签页到右侧 (Duplicate Tab)
    /// 独立旁路去重，绝对不会被智能去重机制消除
    pub fn duplicate_tab(&self, explorer_hwnd: HWND, target_title: Option<&str>, target_index: Option<usize>) {
        let cfg = self.get_config();
        if !cfg.enabled || !cfg.double_click_to_duplicate {
            return;
        }

        // 1. 精准解析被点击 Tab 对应的物理路径
        let curr_path = match self.com_manager.resolve_tab_path_by_target(explorer_hwnd, target_title, target_index) {
            Some(p) => p,
            None => {
                // 回退到当前活动路径
                match self.com_manager.get_active_tab_path(Some(explorer_hwnd)) {
                    Some(p) => p,
                    None => return,
                }
            }
        };

        // 2. 启动 Bypass 去重保护，绝不被去重机制误吞
        self.bypass_deduplication.store(true, Ordering::SeqCst);

        // 3. 统计当前窗口既有 Tab 数量。
        //
        // 基线必须“确实读到”：`get_all_tabs` 在 STA 超时时退化为空列表，
        // 若按 0 处理，`try_navigate_new_tab` 的 `count_before == 0` 分支会
        // 把宿主机有标签误认为“新标签已出现”并导航错误对象（报告 P1-01 同族）。
        // 未知基线用 `usize::MAX` 表示“永不能用计数增长确认”，
        // 交给 `wait_and_navigate_new_tab` 内建的 `navigate_latest` 兜底。
        let initial_count = self
            .com_manager
            .get_tabs_count_for_hwnd_checked(explorer_hwnd)
            .unwrap_or(usize::MAX);

        // 4. 激活宿主窗口并触发新建 Tab
        unsafe {
            let _ = SetForegroundWindow(explorer_hwnd);
        }

        if trigger_new_tab(explorer_hwnd) {
            let com = self.com_manager.clone();
            let bypass = self.bypass_deduplication.clone();
            let raw_hwnd = explorer_hwnd.0 as usize;
            let target_path = curr_path.clone();

            std::thread::spawn(move || {
                let h = HWND(raw_hwnd as *mut _);
                if is_physical_dir_path(&target_path) {
                    let _ = com.wait_and_navigate_new_tab(h, initial_count, &target_path);
                }

                // 维持旁路状态一段时间，杜绝并发去重消除
                std::thread::sleep(Duration::from_millis(400));
                bypass.store(false, Ordering::SeqCst);
            });
        } else {
            self.bypass_deduplication.store(false, Ordering::SeqCst);
        }
    }

    /// 当检测到新 Tab 加载完成，执行重定向并消除闪烁
    pub fn on_tab_created(&self, _explorer_hwnd: HWND) {
        self.bypass_deduplication.store(false, Ordering::SeqCst);
    }

    /// 廉价判定（纯 Win32，无 COM/UIA，微秒级）：当前是否存在可作为标签宿主的
    /// 资源管理器顶级窗口。供低级键盘钩子回调在“吞键 / 放行系统原生”之间快速决策。
    pub fn quick_wine_target() -> Option<HWND> {
        unsafe {
            let fg = GetForegroundWindow();
            if Self::is_explorer_window(fg) && IsWindowVisible(fg).as_bool() {
                return Some(fg);
            }

            let found = windows::Win32::UI::WindowsAndMessaging::FindWindowExW(
                None,
                None,
                windows::core::w!("CabinetWClass"),
                windows::core::PCWSTR::null(),
            )
            .ok()?;
            if found.0.is_null() || !IsWindowVisible(found).as_bool() {
                return None;
            }
            Some(found)
        }
    }

    /// Win+E 接管：在目标宿主窗口中新建标签页。
    ///
    /// 必须在工作线程/宿主消息线程执行（内部含 COM 枚举与 UIA），绝不可放进低级钩子回调。
    /// 只走「窗口消息 + UIA 语义动作」这条安全链路，不做任何全局键鼠模拟：
    /// 失败时返回 false，由宿主用原生资源管理器窗口兜底，保证“按键必有结果”。
    pub fn wine_to_tab_in(&self, target_hwnd: HWND) -> bool {
        let cfg = self.get_config();
        if !cfg.enabled || !cfg.wine_to_tab {
            return false;
        }
        if !Self::is_explorer_window(target_hwnd) {
            return false;
        }

        // 工作线程的 SetForegroundWindow 会被前台锁静默拒绝，必须走输入桥接
        unsafe {
            bring_window_to_front(target_hwnd);
        }
        trigger_new_tab_safe(target_hwnd)
    }

    /// 当外部或用户请求打开某个目录时的去重判定与处理
    /// 返回 true 表示已定位到现有 Tab 并激活（已去重）；返回 false 表示无重复或已旁路
    pub fn handle_tab_deduplication(&self, target_path: &str, host_hwnd: HWND) -> bool {
        let cfg = self.get_config();
        if !cfg.enabled || !cfg.deduplication {
            return false;
        }

        // 若当前正处于克隆/继承当前目录旁路模式，不执行去重拦截
        if self.bypass_deduplication.load(Ordering::SeqCst) {
            return false;
        }

        let tabs = self.com_manager.get_all_tabs();
        let target_norm = normalize_path(target_path);

        for tab in tabs {
            if tab.hwnd() == host_hwnd && normalize_path(&tab.path) == target_norm {
                // 找到既有相同路径的 Tab，直接聚焦并激活
                unsafe {
                    let _ = SetForegroundWindow(host_hwnd);
                }
                return true;
            }
        }

        false
    }

    pub fn com_manager(&self) -> &Arc<ShellComManager> {
        &self.com_manager
    }

    /// 查找除 exclude_hwnd 之外真实存活且可见的 Explorer 宿主窗口 (CabinetWClass)
    pub fn find_host_explorer_window(exclude_hwnd: HWND) -> Option<HWND> {
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetForegroundWindow, IsWindow, IsWindowVisible,
        };

        // 优先检查当前前台聚焦窗口是否为合法的宿主窗口
        let fg = unsafe { GetForegroundWindow() };
        if !fg.0.is_null() && fg != exclude_hwnd && Self::is_explorer_window(fg) && unsafe { IsWindowVisible(fg).as_bool() } {
            return Some(fg);
        }

        struct SearchCtx {
            exclude: HWND,
            found: Option<HWND>,
        }

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let ctx = &mut *(lparam.0 as *mut SearchCtx);
                if hwnd == ctx.exclude {
                    return BOOL(1);
                }
                if IsWindow(Some(hwnd)).as_bool() && IsWindowVisible(hwnd).as_bool() {
                    let mut buf = [0u16; 64];
                    let len = GetClassNameW(hwnd, &mut buf);
                    if len > 0 {
                        let cls = String::from_utf16_lossy(&buf[..len as usize]);
                        if cls == "CabinetWClass" {
                            ctx.found = Some(hwnd);
                            return BOOL(0);
                        }
                    }
                }
                BOOL(1)
            }
        }

        let mut ctx = SearchCtx {
            exclude: exclude_hwnd,
            found: None,
        };

        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
        }

        ctx.found
    }

    /// 捕获新产生的外部顶级窗口并合并为 Tab
    pub fn handle_new_explorer_window(&self, new_hwnd: HWND) {
        let cfg = self.get_config();
        if !cfg.enabled {
            return;
        }

        // 仅处理真实的 CabinetWClass 顶级窗口
        if !Self::is_explorer_window(new_hwnd) {
            return;
        }

        let raw_new = new_hwnd.0 as usize;
        // 防重检查：如果该窗口已经在异步合并处理中，直接跳过
        {
            if let Ok(mut set) = self.processing_new_windows.lock() {
                if !set.insert(raw_new) {
                    return;
                }
            }
        }

        // 1. 查找除 new_hwnd 之外的真实存活且可见的宿主窗口（原生 Win32 枚举，0ms 延迟）
        let host_hwnd = match Self::find_host_explorer_window(new_hwnd) {
            Some(h) => h,
            None => {
                // 关键原则：若当前系统没有其它存活且可见的宿主窗口，坚决不隐藏、不干涉，完全原生放行
                let _ = self.processing_new_windows.lock().map(|mut set| set.remove(&raw_new));
                return;
            }
        };

        crate::dialog::detector::log_debug(&format!(
            "TAB_CAPTURE: new=0x{:X} host=0x{:X}",
            raw_new, host_hwnd.0 as usize
        ));

        // 2. 确认存在真实有效宿主，采用分层窗口 Alpha=0 透明度 + 离屏移位，杜绝弹窗闪烁同时保持 COM 活跃
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                GetWindowLongW, SetLayeredWindowAttributes, SetWindowLongW, GWL_EXSTYLE,
                LWA_ALPHA, WS_EX_LAYERED,
            };
            let ex_style = GetWindowLongW(new_hwnd, GWL_EXSTYLE);
            let _ = SetWindowLongW(new_hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED.0 as i32);
            let _ = SetLayeredWindowAttributes(
                new_hwnd,
                windows::Win32::Foundation::COLORREF(0),
                0,
                LWA_ALPHA,
            );

            let _ = SetWindowPos(
                new_hwnd,
                None,
                -32000,
                -32000,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }

        let raw_host = host_hwnd.0 as usize;
        let com = self.com_manager.clone();
        let processing = self.processing_new_windows.clone();
        let cfg_clone = cfg.clone();
        let bypass = self.bypass_deduplication.clone();

        std::thread::spawn(move || {
            let start_time = std::time::Instant::now();
            let new_h = HWND(raw_new as *mut _);
            let host_h = HWND(raw_host as *mut _);

            let mut target_info: Option<(String, Option<String>)> = None;

            // 1. 尝试从新窗口对应的进程命令行提取目标路径（优先识别外部快捷 /select 或直接参数）
            let mut pid = 0u32;
            unsafe {
                let _ = GetWindowThreadProcessId(new_h, Some(&mut pid));
            }
            if pid > 0 {
                if let Some(cmd) = unsafe { query_process_cmdline(pid) } {
                    if let Some(info) = parse_select_target_from_cmdline(&cmd) {
                        target_info = Some(info);
                    }
                }
            }

            // 2. COM 穿透定向探测：有界等待预算 800ms，识别到有效物理路径立即短路跳出
            if target_info.is_none() {
                for _ in 0..25 {
                    if start_time.elapsed() > Duration::from_millis(800) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(30));
                    if !unsafe { IsWindow(Some(new_h)) }.as_bool() {
                        let _ = processing.lock().map(|mut set| set.remove(&raw_new));
                        return; // 窗口已被外部自行关闭
                    }
                    if let Some(info) = com.get_window_target_info(new_h) {
                        let (ref path, ref _sel) = info;
                        let is_blank_or_home = path == "shell:Home"
                            || path == "shell:::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}"
                            || path.is_empty();
                        // 无效路径（blank/home/空）永远不接受：宁可超时走 fail-safe 恢复，
                        // 也绝不拿一个假路径去"合并成功"并关闭源窗口（否则用户点击完全无响应）
                        if !is_blank_or_home {
                            target_info = Some(info);
                            break;
                        }
                    }
                }
            }

            let mut merged_success = false;

            match target_info {
                Some((ref path, ref file)) => crate::dialog::detector::log_debug(&format!(
                    "TAB_TARGET: new=0x{:X} path='{}' file={:?}",
                    raw_new, path, file
                )),
                None => crate::dialog::detector::log_debug(&format!(
                    "TAB_TARGET: new=0x{:X} -> none (fail-safe 恢复原生窗口)",
                    raw_new
                )),
            }

            if let Some((ref folder_path, _)) = target_info {
                // 校验宿主窗口依然存活
                if unsafe { IsWindow(Some(host_h)).as_bool() && IsWindowVisible(host_h).as_bool() } {
                    // 工作线程的 SetForegroundWindow 会被前台锁静默拒绝（后台进程无前台权限），
                    // 必须用输入桥接，否则合并成功了用户也看不见宿主窗口
                    unsafe {
                        bring_window_to_front(host_h);
                    }

                    // 检查去重配置：若开启去重且非旁路状态，检测宿主中是否已存在该 Tab
                    let mut matched_tab_index = None;
                    if cfg_clone.deduplication && !bypass.load(Ordering::SeqCst) {
                        matched_tab_index = com.find_tab_index_by_path(host_h, folder_path);
                    }

                    crate::dialog::detector::log_debug(&format!(
                        "TAB_DEDUP: host=0x{:X} path='{}' matched_idx={:?} dedup={} bypass={}",
                        host_h.0 as usize,
                        folder_path,
                        matched_tab_index,
                        cfg_clone.deduplication,
                        bypass.load(Ordering::SeqCst)
                    ));

                    if let Some(tab_idx) = matched_tab_index {
                        // 宿主已有该 Tab：必须真正激活它，而不仅是"认为"激活了。
                        // 0xA221 是未公开魔法指令，在部分构建上无响应且从不报错；
                        // 改为 UIA 直选优先、0xA221 兜底，每次都核验活动标签，
                        // 全部失败则回退 fail-safe（保留源窗口），绝不出现"点了像没反应"。
                        merged_success =
                            Self::activate_tab_in_host(&com, host_h, folder_path, tab_idx);
                    } else if is_physical_dir_path(folder_path) {
                        // 宿主没有该 Tab 或未开启去重，在宿主中新建 Tab 并精准导航。
                        // 基线未知（查询超时）时用 `usize::MAX` 禁用“计数增长即成功”的判据，
                        // 避免把宿主原有标签当成新标签导航（报告 P1-01 同族）。
                        let count_before = com
                            .get_tabs_count_for_hwnd_checked(host_h)
                            .unwrap_or(usize::MAX);
                        if trigger_new_tab(host_h) {
                            merged_success = com.wait_and_navigate_new_tab(host_h, count_before, folder_path);
                        }
                    } else {
                        // 非物理路径（shell: 等）无法在标签页中重建：不创建空标签、不关闭源窗口，
                        // merged_success 保持 false，交由 fail-safe 恢复原生窗口
                    }
                }
            }

            crate::dialog::detector::log_debug(&format!(
                "TAB_MERGE: new=0x{:X} host=0x{:X} success={}",
                raw_new, raw_host, merged_success
            ));

            // 外部 /select 携带文件名：文件夹落地已由 wait_and_navigate 确认，
            // 文件高亮改为后台异步重试——视图加载需要时间，同步等待是合并链路里最贵的一段
            // （实测可达数百毫秒），而高亮失败只是少一个选中态，不影响"窗口弹出来了"这个核心诉求
            if merged_success {
                if let Some((_, Some(ref file_name))) = target_info {
                    async_select_file_item(host_h, file_name.clone());
                }
            }

            if merged_success {
                // 稳健关闭源窗口：WM_CLOSE 投递后轮询确认窗口真正销毁（每 200ms 补发一次，
                // 最长 3 秒）。年轻窗口（刚创建几百毫秒）响应销毁较慢，固定短宽限期会把
                // "正在关闭"误判为"关闭失败"并将其复活——只有宽限期耗尽仍存活，才认定
                // 为关闭被拒并恢复可见（绝不留隐身窗口）。补发前校验 PID，防止 HWND 被
                // 复用后误关用户的新窗口。
                let mut wnd_pid = 0u32;
                unsafe {
                    let _ = GetWindowThreadProcessId(new_h, Some(&mut wnd_pid));
                    let _ = PostMessageW(Some(new_h), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                let mut destroyed = false;
                for _ in 0..15 {
                    std::thread::sleep(Duration::from_millis(200));
                    unsafe {
                        if !IsWindow(Some(new_h)).as_bool() {
                            destroyed = true;
                            break;
                        }
                        // HWND 复用守卫：句柄已指向另一个进程的窗口时，视为原窗口已消失
                        let mut cur_pid = 0u32;
                        let _ = GetWindowThreadProcessId(new_h, Some(&mut cur_pid));
                        if wnd_pid != 0 && cur_pid != wnd_pid {
                            destroyed = true;
                            break;
                        }
                        let _ = PostMessageW(Some(new_h), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                if !destroyed {
                    // 3 秒后依然存活：关闭被拒，恢复不透明度并移回可见区域
                    unsafe {
                        use windows::Win32::Foundation::COLORREF;
                        use windows::Win32::UI::WindowsAndMessaging::{
                            SetLayeredWindowAttributes, LWA_ALPHA,
                        };
                        let _ = SetLayeredWindowAttributes(new_h, COLORREF(0), 255, LWA_ALPHA);
                        let _ = SetWindowPos(
                            new_h,
                            None,
                            100,
                            100,
                            0,
                            0,
                            SWP_NOSIZE | SWP_SHOWWINDOW,
                        );
                        bring_window_to_front(new_h);
                    }
                }
            } else {
                // 【核心 Fail-Safe 兜底】：未能成功合并或超时未就绪，绝对不关窗，立即恢复显示源窗口并恢复不透明度！
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::{
                        SetLayeredWindowAttributes, LWA_ALPHA,
                    };
                    let _ = SetLayeredWindowAttributes(
                        new_h,
                        windows::Win32::Foundation::COLORREF(0),
                        255,
                        LWA_ALPHA,
                    );
                    let _ = SetWindowPos(
                        new_h,
                        None,
                        100,
                        100,
                        0,
                        0,
                        SWP_NOSIZE | SWP_SHOWWINDOW,
                    );
                    // 同样走输入桥接：fail-safe 恢复的窗口是用户当前最期望看到的，必须尽力前台化
                    bring_window_to_front(new_h);
                }
            }

            // 处理结束，从防重集合中移除记录
            let _ = processing.lock().map(|mut set| set.remove(&raw_new));
        });
    }

    /// 激活宿主窗口中指定路径的标签页（带核验）。
    ///
    /// 策略：
    /// 1. UIA `SelectionItemPattern` 只负责“选择”动作（唯一精确同名优先，否则按 COM 索引）；
    /// 2. 选择后必须回查 Shell COM，确认**活动标签的规范化完整路径**等于 `folder_path`，
    ///    标题（末级目录名）绝不作为身份依据（`C:\ProjectA\src` 与 `D:\ProjectB\src` 同名）；
    /// 3. UIA 路径未证实才尝试 0xA221（Ctrl+1..N）魔法指令兜底，同样必须通过 COM 核验。
    ///
    /// 返回 true 当且仅当目标完整路径已是活动标签；否则返回 false，调用方回退 fail-safe。
    fn activate_tab_in_host(
        com: &ShellComManager,
        host_h: HWND,
        folder_path: &str,
        tab_idx: usize,
    ) -> bool {
        let folder_name = std::path::Path::new(folder_path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| folder_path.to_string());
        if folder_name.is_empty() {
            return false;
        }

        // 尝试 1：UIA 选择 + Shell COM 完整路径核验
        if try_select_tab_item(host_h, tab_idx, &folder_name)
            && Self::verify_active_tab_path(com, host_h, folder_path)
        {
            crate::dialog::detector::log_debug(&format!(
                "TAB_ACTIVATE: host=0x{:X} target='{}' via=UIA idx={} -> ok",
                host_h.0 as usize, folder_path, tab_idx
            ));
            return true;
        }

        // 尝试 2：0xA221 魔法指令兜底（未公开指令，部分构建有效；
        // COM 枚举序号未必等于视觉标签顺序，因此仅作兜底且必须核验完整路径）
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_COMMAND};
            let _ = SendMessageW(
                host_h,
                WM_COMMAND,
                Some(WPARAM(0xA221)),
                Some(LPARAM((tab_idx + 1) as isize)),
            );
        }
        if Self::verify_active_tab_path(com, host_h, folder_path) {
            crate::dialog::detector::log_debug(&format!(
                "TAB_ACTIVATE: host=0x{:X} target='{}' via=0xA221 idx={} -> ok",
                host_h.0 as usize, folder_path, tab_idx
            ));
            return true;
        }

        // 全部尝试后目标完整路径仍非活动标签：返回 false，交由调用方 fail-safe 保留原生窗口，
        // 绝不静默关闭源窗口（那会让用户以为“完全没反应”）
        crate::dialog::detector::log_debug(&format!(
            "TAB_ACTIVATE: host=0x{:X} target='{}' idx={} -> FAILED (fail-safe)",
            host_h.0 as usize, folder_path, tab_idx
        ));
        false
    }

    /// 回查 Shell COM，核验宿主窗口当前活动标签的规范化完整路径是否等于期望路径。
    /// 选择生效需要时间，分两次核验，避免“已选中但尚未切换完成”的假失败。
    fn verify_active_tab_path(com: &ShellComManager, host_h: HWND, expected_path: &str) -> bool {
        let expected = normalize_path(expected_path);
        for _ in 0..2 {
            std::thread::sleep(Duration::from_millis(150));
            if let Some(active) = com.get_active_tab_path(Some(host_h)) {
                if normalize_path(&active) == expected {
                    return true;
                }
            }
        }
        false
    }

    fn clone_ref(&self) -> Arc<Self> {
        Arc::new(Self {
            config: self.config.clone(),
            com_manager: self.com_manager.clone(),
            pending_inherit_path: self.pending_inherit_path.clone(),
            bypass_deduplication: self.bypass_deduplication.clone(),
            processing_new_windows: self.processing_new_windows.clone(),
        })
    }
}

/// 尝试将窗口真正带到前台。
///
/// 背景：后台进程在工作线程直接调 SetForegroundWindow 会被 Windows 前台锁静默拒绝
/// （返回 FALSE），导致"合并成功了但用户什么都看不见"。本函数按以下顺序尽力而为：
/// 1) 常规 SetForegroundWindow；2) AttachThreadInput 桥接输入状态后重试；
/// 3) 退化为任务栏闪烁 + SWP_SHOWWINDOW 确保窗口至少可见。
unsafe fn bring_window_to_front(hwnd: HWND) -> bool {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, FlashWindowEx, GetForegroundWindow, GetWindowThreadProcessId,
        IsWindow, SetForegroundWindow, SetWindowPos, FLASHWINFO, FLASHW_ALL,
        FLASHW_TIMERNOFG, HWND_TOP, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    // 仓库风格：unsafe fn 内部再包一层 unsafe 块，避免 unsafe_op_in_unsafe_fn lint
    unsafe {
        if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }

        // 1. 常规尝试（前台进程/刚收过输入时可直接成功）
        if SetForegroundWindow(hwnd).as_bool() {
            return true;
        }

        // 2. AttachThreadInput 桥接：将当前线程输入状态挂到前台线程上再抢前台
        let fg = GetForegroundWindow();
        if !fg.0.is_null() {
            let mut _pid = 0u32;
            let fg_tid = GetWindowThreadProcessId(fg, Some(&mut _pid));
            let cur_tid = GetCurrentThreadId();
            if fg_tid != 0 && fg_tid != cur_tid
                && AttachThreadInput(cur_tid, fg_tid, true).as_bool()
            {
                let ok = SetForegroundWindow(hwnd).as_bool();
                let _ = BringWindowToTop(hwnd);
                let _ = AttachThreadInput(cur_tid, fg_tid, false);
                if ok {
                    return true;
                }
            }
        }

        // 3. 退化方案：任务栏闪烁提示 + 确保窗口可见（不抢焦点也至少让用户感知到）
        let mut fwi: FLASHWINFO = std::mem::zeroed();
        fwi.cbSize = std::mem::size_of::<FLASHWINFO>() as u32;
        fwi.hwnd = hwnd;
        fwi.dwFlags = FLASHW_ALL | FLASHW_TIMERNOFG;
        fwi.uCount = 3;
        fwi.dwTimeout = 0;
        let _ = FlashWindowEx(&fwi);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
        false
    }
}

/// 路径规范化比对辅助函数
fn normalize_path(path: &str) -> String {
    let mut s = path.trim().to_lowercase().replace('/', "\\");
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

#[repr(C)]
struct UNICODE_STRING {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

/// 快速查询指定 PID 进程的命令行参数 (利用 NtQueryInformationProcess)
unsafe fn query_process_cmdline(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    type NtQueryInformationProcessFn = unsafe extern "system" fn(
        process_handle: windows::Win32::Foundation::HANDLE,
        process_information_class: i32,
        process_information: *mut core::ffi::c_void,
        process_information_length: u32,
        return_length: *mut u32,
    ) -> i32;

    const STATUS_INFO_LENGTH_MISMATCH: i32 = -1073741820; // 0xC0000004
    const MAX_ALLOWED_CMDLINE_BYTES: u32 = 64 * 1024;

    unsafe {
        let h_process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let ntdll = windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("ntdll.dll")).ok()?;
        let proc_addr = windows::Win32::System::LibraryLoader::GetProcAddress(ntdll, windows::core::s!("NtQueryInformationProcess"))?;
        let nt_query: NtQueryInformationProcessFn = core::mem::transmute(proc_addr);

        let mut buf = vec![0u8; 2048];
        let mut ret_len = 0u32;
        // ProcessCommandLineInformation = 60
        let mut status = nt_query(
            h_process,
            60,
            buf.as_mut_ptr() as *mut _,
            buf.len() as u32,
            &mut ret_len,
        );

        // 若缓冲区不足且返回了所需长度，动态重试一次（受控最大 64KB）
        if (status == STATUS_INFO_LENGTH_MISMATCH || status != 0)
            && ret_len > buf.len() as u32
            && ret_len <= MAX_ALLOWED_CMDLINE_BYTES
        {
            buf.resize(ret_len as usize, 0);
            status = nt_query(
                h_process,
                60,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                &mut ret_len,
            );
        }

        let _ = CloseHandle(h_process);

        if status == 0 && ret_len >= size_of::<UNICODE_STRING>() as u32 {
            let us = &*(buf.as_ptr() as *const UNICODE_STRING);
            if us.length > 0 && !us.buffer.is_null() {
                let buf_start = buf.as_ptr() as usize;
                let buf_end = buf_start + buf.len();
                let str_start = us.buffer as usize;
                let str_bytes = us.length as usize;

                if let Some(str_end) = str_start.checked_add(str_bytes) {
                    if str_start >= buf_start && str_end <= buf_end && str_bytes % 2 == 0 {
                        let slice = core::slice::from_raw_parts(us.buffer, str_bytes / 2);
                        return Some(String::from_utf16_lossy(slice));
                    }
                }
            }
        }

        None
    }
}

/// 从外部 Explorer 命令行提取目标文件与父目录（支持 /select 与常规路径参数，过滤 COM 宿主模式与 exe 自身）
pub fn parse_select_target_from_cmdline(cmd: &str) -> Option<(String, Option<String>)> {
    let lower = cmd.to_lowercase();

    // 1. 过滤 Windows COM 宿主模式（/factory, {...} -Embedding），此类命令无直接路径参数，交由后续 COM 穿透探测
    if lower.contains("/factory") || lower.contains("-embedding") {
        return None;
    }

    // 2. 优先处理显式的 /select,"<path>" 模式
    if let Some(pos) = lower.find("/select,") {
        let remainder = cmd[pos + 8..].trim();
        let target_str = if remainder.starts_with('"') {
            if let Some(end) = remainder[1..].find('"') {
                &remainder[1..1 + end]
            } else {
                remainder.trim_matches('"')
            }
        } else {
            remainder.split_whitespace().next().unwrap_or(remainder)
        };

        let target_str = target_str.trim();
        if is_valid_select_path(target_str) {
            let p = std::path::Path::new(target_str);
            if p.is_file() {
                let folder = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| target_str.to_string());
                let file = p.file_name().map(|f| f.to_string_lossy().to_string());
                return Some((folder, file));
            } else if p.is_dir() {
                return Some((target_str.to_string(), None));
            } else if let Some(parent) = p.parent() {
                let folder = parent.to_string_lossy().to_string();
                let file = p.file_name().map(|f| f.to_string_lossy().to_string());
                return Some((folder, file));
            }
        }
        return None;
    }

    // 3. 处理外部程序直接带路径唤起（如 explorer.exe "D:\Projects" 或 "C:\Windows\explorer.exe" D:\Projects）
    let args_str = strip_exe_prefix(cmd);

    // 首先尝试在引号部分提取候选路径
    for part in args_str.split('"') {
        let mut candidate = part.trim();
        if let Some(comma_pos) = candidate.find(',') {
            if candidate.starts_with('/') || candidate.starts_with('-') {
                candidate = candidate[comma_pos + 1..].trim();
            }
        }
        let candidate = candidate.trim_matches('"').trim();
        if is_valid_target_dir_path(candidate) {
            let p = std::path::Path::new(candidate);
            if p.is_file() {
                let folder = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| candidate.to_string());
                let file = p.file_name().map(|f| f.to_string_lossy().to_string());
                return Some((folder, file));
            }
            return Some((candidate.to_string(), None));
        }
    }

    // 若未被双引号包裹，按空白切分参数逐项检查
    for token in args_str.split_whitespace() {
        let mut candidate = token.trim();
        if let Some(comma_pos) = candidate.find(',') {
            if candidate.starts_with('/') || candidate.starts_with('-') {
                candidate = candidate[comma_pos + 1..].trim();
            }
        }
        let candidate = candidate.trim_matches('"').trim();
        if is_valid_target_dir_path(candidate) {
            let p = std::path::Path::new(candidate);
            if p.is_file() {
                let folder = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| candidate.to_string());
                let file = p.file_name().map(|f| f.to_string_lossy().to_string());
                return Some((folder, file));
            }
            return Some((candidate.to_string(), None));
        }
    }

    None
}

/// 剥离命令行开头的可执行文件部分
fn strip_exe_prefix(cmd: &str) -> &str {
    let trimmed = cmd.trim();
    if trimmed.starts_with('"') {
        if let Some(end_quote) = trimmed[1..].find('"') {
            return trimmed[1 + end_quote + 1..].trim();
        }
    }

    if let Some(space_pos) = trimmed.find(char::is_whitespace) {
        let first_token = &trimmed[..space_pos];
        if first_token.to_lowercase().ends_with(".exe") || first_token.eq_ignore_ascii_case("explorer") {
            return trimmed[space_pos..].trim();
        }
    }

    ""
}

/// 检查是否为 /select 候选路径（支持 .exe 等各类目标文件）
fn is_valid_select_path(path: &str) -> bool {
    let s = path.trim();
    if s.len() < 2 || s.starts_with('/') || s.starts_with('-') {
        return false;
    }
    (s.len() >= 2 && &s[1..2] == ":") || s.starts_with("\\\\")
}

/// 检查是否为直接唤起的目标目录/文件路径（严格排除 .exe 与命令行开关）
fn is_valid_target_dir_path(path: &str) -> bool {
    let s = path.trim();
    if s.len() < 2 || s.starts_with('/') || s.starts_with('-') {
        return false;
    }
    if s.to_lowercase().ends_with(".exe") {
        return false;
    }
    (s.len() >= 2 && &s[1..2] == ":") || s.starts_with("\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_select_target_from_cmdline() {
        let cmd1 = r#"explorer.exe /select,"C:\Windows\System32\notepad.exe""#;
        let res1 = parse_select_target_from_cmdline(cmd1);
        assert_eq!(
            res1,
            Some(("C:\\Windows\\System32".to_string(), Some("notepad.exe".to_string())))
        );

        let cmd2 = r#"C:\WINDOWS\explorer.exe /select, "D:\My Project\Cargo.toml""#;
        let res2 = parse_select_target_from_cmdline(cmd2);
        assert_eq!(
            res2,
            Some(("D:\\My Project".to_string(), Some("Cargo.toml".to_string())))
        );

        let cmd3 = r#"explorer.exe "C:\Users""#;
        let res3 = parse_select_target_from_cmdline(cmd3);
        assert_eq!(res3, Some(("C:\\Users".to_string(), None)));

        let cmd4 = r#"explorer.exe"#;
        let res4 = parse_select_target_from_cmdline(cmd4);
        assert_eq!(res4, None);

        // 测试 COM 宿主模式启动命令过滤（核心防御）
        let cmd_factory1 = r#"C:\WINDOWS\explorer.exe /factory,{ceff45ee-c862-41de-aee2-a022c81eda92} -Embedding"#;
        assert_eq!(parse_select_target_from_cmdline(cmd_factory1), None);

        let cmd_factory2 = r#""C:\WINDOWS\explorer.exe" /factory,{75dff2b7-6936-4c06-a8bb-676a7b00b24b} -Embedding"#;
        assert_eq!(parse_select_target_from_cmdline(cmd_factory2), None);

        // 测试可执行文件独立启动过滤
        let cmd_exe_only1 = r#"C:\WINDOWS\Explorer.EXE"#;
        assert_eq!(parse_select_target_from_cmdline(cmd_exe_only1), None);

        let cmd_exe_only2 = r#""C:\WINDOWS\explorer.exe""#;
        assert_eq!(parse_select_target_from_cmdline(cmd_exe_only2), None);

        // 测试直接带路径唤起
        let cmd_direct1 = r#"C:\WINDOWS\explorer.exe "D:\My Project""#;
        assert_eq!(parse_select_target_from_cmdline(cmd_direct1), Some(("D:\\My Project".to_string(), None)));

        let cmd_direct2 = r#"explorer.exe D:\WorkSpace"#;
        assert_eq!(parse_select_target_from_cmdline(cmd_direct2), Some(("D:\\WorkSpace".to_string(), None)));
    }
}

