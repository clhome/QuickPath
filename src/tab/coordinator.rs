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
use super::history::TabHistoryStack;
use super::select::{async_select_file_item, try_select_tab_item_by_title};
use super::uia::trigger_new_tab;

/// 多标签增强配置
#[derive(Debug, Clone)]
pub struct TabConfig {
    pub enabled: bool,
    pub wine_to_tab: bool,
    pub inherit_current_path: bool,
    pub deduplication: bool,
    pub reopen_closed: bool,
    pub double_click_to_duplicate: bool,
}

impl Default for TabConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wine_to_tab: true,
            inherit_current_path: true,
            deduplication: true,
            reopen_closed: true,
            double_click_to_duplicate: true,
        }
    }
}

pub struct TabCoordinator {
    config: Arc<parking_lot_or_std::RwLock<TabConfig>>,
    com_manager: Arc<ShellComManager>,
    history_stack: TabHistoryStack,
    /// 记录最近一次由于“新建标签页继承当前目录”预存的目录路径及时间戳
    pending_inherit_path: Arc<parking_lot_or_std::Mutex<Option<(String, Instant)>>>,
    /// 是否正处于旁路去重模式（防止新建同路径 Tab 时被去重拦截）
    bypass_deduplication: Arc<AtomicBool>,
}

mod parking_lot_or_std {
    pub use std::sync::{Mutex, RwLock};
}

impl TabCoordinator {
    pub fn new(com_manager: Arc<ShellComManager>) -> Self {
        Self {
            config: Arc::new(parking_lot_or_std::RwLock::new(TabConfig::default())),
            com_manager,
            history_stack: TabHistoryStack::new(20),
            pending_inherit_path: Arc::new(parking_lot_or_std::Mutex::new(None)),
            bypass_deduplication: Arc::new(AtomicBool::new(false)),
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

    /// 触发“新建标签页”意图（例如用户按下了 Ctrl+T 或点击了“+”新建按钮前）
    /// 预存当前活动目录，为后续新 Tab 毫秒级重定向铺平道路
    pub fn prepare_new_tab_inheritance(&self, explorer_hwnd: HWND) {
        let cfg = self.get_config();
        if !cfg.enabled || !cfg.inherit_current_path {
            return;
        }

        // 获取当前活动 Tab 路径
        if let Some(curr_path) = self.com_manager.get_active_tab_path(Some(explorer_hwnd)) {
            if let Ok(mut pending) = self.pending_inherit_path.lock() {
                *pending = Some((curr_path, Instant::now()));
            }
            // 标记旁路去重
            self.bypass_deduplication.store(true, Ordering::SeqCst);
        }
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

        // 3. 统计当前窗口既有 Tab 数量
        let initial_count = self.com_manager.get_tabs_count_for_hwnd(explorer_hwnd);

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

                // 5. 轮询等待新 Tab 在 COM 中正式挂载注册（最多 20 轮，每轮 25ms，共 500ms）
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(25));
                    if com.get_tabs_count_for_hwnd(h) > initial_count {
                        break;
                    }
                }

                // 6. 稍微平稳等待 60ms 让新 Tab 内部视图框架与文件系统绑定完毕
                std::thread::sleep(Duration::from_millis(60));

                if is_physical_dir_path(&target_path) {
                    let _ = com.navigate_latest(h, &target_path);
                }

                // 维持旁路状态一段时间，杜绝并发去重消除
                std::thread::sleep(Duration::from_millis(600));
                bypass.store(false, Ordering::SeqCst);
            });
        } else {
            self.bypass_deduplication.store(false, Ordering::SeqCst);
        }
    }

    /// 当检测到新 Tab 加载完成，执行重定向并消除闪烁
    pub fn on_tab_created(&self, explorer_hwnd: HWND) {
        let cfg = self.get_config();
        if !cfg.enabled || !cfg.inherit_current_path {
            return;
        }

        let pending_opt = self.pending_inherit_path.lock().ok().and_then(|mut p| p.take());
        if let Some((target_path, created_at)) = pending_opt {
            // 仅在 3 秒内的有效触发周期响应
            if created_at.elapsed() < Duration::from_millis(3000) {
                // 立即重定向到当前目录（仅针对物理磁盘路径）
                if is_physical_dir_path(&target_path) {
                    let _ = self.com_manager.navigate_latest(explorer_hwnd, &target_path);
                }
            }
        }

        // 恢复去重校验
        self.bypass_deduplication.store(false, Ordering::SeqCst);
    }

    /// 处理 Win + E 拦截与自适应分流
    /// 返回 true 表示已被成功拦截并转为在现有窗口中新建 Tab；返回 false 表示放行原生窗口创建
    pub fn handle_wine_hotkey(&self) -> bool {
        let cfg = self.get_config();
        if !cfg.enabled || !cfg.wine_to_tab {
            return false;
        }

        let tabs = self.com_manager.get_all_tabs();
        if tabs.is_empty() {
            // 当前系统没有任何已打开的资源管理器窗口，放行系统原生创建
            return false;
        }

        // 寻找当前聚焦或最近活跃的 Explorer 顶级窗口
        let fg_hwnd = unsafe { GetForegroundWindow() };
        let target_hwnd = if Self::is_explorer_window(fg_hwnd) {
            fg_hwnd
        } else {
            // 否则挑选列表中活跃的窗口
            tabs.iter()
                .find(|t| t.is_active)
                .map(|t| t.hwnd())
                .unwrap_or_else(|| tabs[0].hwnd())
        };

        // 激活目标窗口
        unsafe {
            let _ = SetForegroundWindow(target_hwnd);
        }

        // 如果开启了新建标签页继承当前目录，则在此处预存路径
        if cfg.inherit_current_path {
            self.prepare_new_tab_inheritance(target_hwnd);
        }

        // 触发在目标窗口中新建 Tab
        if trigger_new_tab(target_hwnd) {
            // 如果继承路径开启，延时微任务做防闪烁定向
            let this = self.clone_ref();
            let raw_target = target_hwnd.0 as usize;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(25));
                this.on_tab_created(HWND(raw_target as *mut _));
            });
            return true;
        }

        false
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

    /// 记录 Tab 关闭事件
    pub fn record_tab_closed(&self, path: String) {
        let cfg = self.get_config();
        if cfg.enabled && cfg.reopen_closed {
            self.history_stack.push(path);
        }
    }

    /// 恢复最近关闭的 Tab (Ctrl+Shift+T)
    pub fn reopen_last_closed_tab(&self) -> bool {
        let cfg = self.get_config();
        if !cfg.enabled || !cfg.reopen_closed {
            return false;
        }

        if let Some(reopen_path) = self.history_stack.pop() {
            let tabs = self.com_manager.get_all_tabs();
            if let Some(host) = tabs.first() {
                let host_hwnd = host.hwnd();
                // 在现有窗口新建 Tab 并按需导航
                if trigger_new_tab(host_hwnd) {
                    if is_physical_dir_path(&reopen_path) {
                        let com = self.com_manager.clone();
                        let raw_host = host_hwnd.0 as usize;
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(30));
                            let _ = com.navigate_latest(HWND(raw_host as *mut _), &reopen_path);
                        });
                    }
                    return true;
                }
            } else {
                // 无存活窗口，调用系统命令打开
                let _ = std::process::Command::new("explorer.exe")
                    .arg(&reopen_path)
                    .spawn();
                return true;
            }
        }

        false
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

        // 1. 查找除 new_hwnd 之外的真实存活且可见的宿主窗口
        let tabs = self.com_manager.get_all_tabs();
        let host_tab = tabs.into_iter().find(|t| {
            let h = t.hwnd();
            h != new_hwnd
                && !h.0.is_null()
                && unsafe { IsWindow(Some(h)).as_bool() && IsWindowVisible(h).as_bool() }
        });

        // 关键原则：若当前系统没有其它存活且可见的宿主窗口，坚决不隐藏、不干涉，完全原生放行
        let host = match host_tab {
            Some(h) => h,
            None => return,
        };

        // 2. 确认存在真实有效宿主，移出可视视区防闪烁（替代暴力的 SW_HIDE，保证 COM 与系统渲染不休眠）
        unsafe {
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

        let raw_host = host.hwnd_raw;
        let raw_new = new_hwnd.0 as usize;
        let com = self.com_manager.clone();

        std::thread::spawn(move || {
            let new_h = HWND(raw_new as *mut _);
            let host_h = HWND(raw_host as *mut _);

            let mut target_info: Option<(String, Option<String>)> = None;

            // 1. 优先尝试从新窗口对应的进程命令行提取目标路径（极速识别 /select）
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

            // 2. 若命令行未能命中，通过 COM 穿透定向探测（最多 10 轮，每轮 30ms，共 300ms 窗口期）
            if target_info.is_none() {
                for _ in 0..10 {
                    std::thread::sleep(Duration::from_millis(30));
                    if !unsafe { IsWindow(Some(new_h)) }.as_bool() {
                        return; // 窗口已被外部自行关闭
                    }
                    if let Some(info) = com.get_window_target_info(new_h) {
                        target_info = Some(info);
                        break;
                    }
                }
            }

            let mut merged_success = false;

            if let Some((ref folder_path, _)) = target_info {
                // 校验宿主窗口依然存活
                if unsafe { IsWindow(Some(host_h)).as_bool() && IsWindowVisible(host_h).as_bool() } {
                    unsafe {
                        let _ = SetForegroundWindow(host_h);
                    }

                    // 检查宿主窗口是否已经存在该路径的 Tab
                    let mut already_opened = false;
                    let current_tabs = com.get_all_tabs();
                    let norm_target = normalize_path(folder_path);
                    for tab in &current_tabs {
                        if tab.hwnd() == host_h && normalize_path(&tab.path) == norm_target {
                            already_opened = true;
                            break;
                        }
                    }

                    if already_opened {
                        // 宿主已有该 Tab：提取目录名并通过 UIA 切换激活对应的 TabItem
                        let p = std::path::Path::new(folder_path);
                        let folder_name = p.file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| folder_path.clone());
                        let _ = try_select_tab_item_by_title(host_h, &folder_name);
                        merged_success = true;
                    } else {
                        // 宿主没有该 Tab，在宿主中新建 Tab
                        if trigger_new_tab(host_h) {
                            // 关键防护：仅当目标是具体的本地物理路径时才执行导航重定向；
                            // 若为虚拟 Shell 主页（如主文件夹/此电脑等），原生新 Tab 默认即为主页，绝不调用 navigate
                            if is_physical_dir_path(folder_path) {
                                std::thread::sleep(Duration::from_millis(50));
                                let _ = com.navigate_latest(host_h, folder_path);
                            }
                            merged_success = true;
                        }
                    }
                }
            }

            if merged_success {
                // 如果存在待高亮的文件（外部 /select 唤起），异步在宿主列表中恢复聚焦选中
                if let Some((_, Some(file_name))) = target_info {
                    async_select_file_item(host_h, file_name);
                }

                // 只有在彻底确认合并成功的情况下，才安全销毁源窗口
                unsafe {
                    let _ = PostMessageW(
                        Some(new_h),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
                // 二次核查源窗口是否彻底销毁，若仍存活则补发 WM_CLOSE，彻底杜绝孤立外部窗口残留
                std::thread::sleep(Duration::from_millis(100));
                unsafe {
                    if IsWindow(Some(new_h)).as_bool() {
                        let _ = PostMessageW(
                            Some(new_h),
                            WM_CLOSE,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            } else {
                // 【核心 Fail-Safe 兜底】：未能成功合并或超时未就绪，绝对不关窗，立即恢复显示源窗口并激活！
                unsafe {
                    let _ = SetWindowPos(
                        new_h,
                        None,
                        100,
                        100,
                        0,
                        0,
                        SWP_NOSIZE | SWP_SHOWWINDOW,
                    );
                    let _ = SetForegroundWindow(new_h);
                }
            }
        });
    }

    fn clone_ref(&self) -> Arc<Self> {
        Arc::new(Self {
            config: self.config.clone(),
            com_manager: self.com_manager.clone(),
            history_stack: self.history_stack.clone(),
            pending_inherit_path: self.pending_inherit_path.clone(),
            bypass_deduplication: self.bypass_deduplication.clone(),
        })
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

    unsafe {
        let h_process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let ntdll = windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("ntdll.dll")).ok()?;
        let proc_addr = windows::Win32::System::LibraryLoader::GetProcAddress(ntdll, windows::core::s!("NtQueryInformationProcess"))?;
        let nt_query: NtQueryInformationProcessFn = core::mem::transmute(proc_addr);

        let mut buf = vec![0u8; 2048];
        let mut ret_len = 0u32;
        // ProcessCommandLineInformation = 60
        let status = nt_query(
            h_process,
            60,
            buf.as_mut_ptr() as *mut _,
            buf.len() as u32,
            &mut ret_len,
        );

        let _ = CloseHandle(h_process);

        if status == 0 && ret_len >= size_of::<UNICODE_STRING>() as u32 {
            let us = &*(buf.as_ptr() as *const UNICODE_STRING);
            if us.length > 0 && !us.buffer.is_null() {
                let slice = core::slice::from_raw_parts(us.buffer, (us.length / 2) as usize);
                return Some(String::from_utf16_lossy(slice));
            }
        }

        None
    }
}

/// 从外部 Explorer 命令行提取 `/select,"<path>"` 的目标文件与父目录
pub fn parse_select_target_from_cmdline(cmd: &str) -> Option<(String, Option<String>)> {
    let lower = cmd.to_lowercase();
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

        if target_str.len() >= 2 {
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
    }
    None
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
        assert_eq!(res3, None);
    }
}

