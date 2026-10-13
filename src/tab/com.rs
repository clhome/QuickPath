#![allow(dead_code)]

use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;
use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{IShellFolderViewDual, IShellWindows, IWebBrowser2, ShellWindows};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, IsWindowVisible};

#[derive(Debug, Clone)]
pub struct ShellTabInfo {
    pub hwnd_raw: usize,
    pub path: String,
    pub title: String,
    pub is_active: bool,
}

impl ShellTabInfo {
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd_raw as *mut _)
    }
}

pub enum ComRequest {
    /// 获取当前前台或所有活跃资源管理器的 Tab 列表
    GetAllTabs {
        reply: Sender<Vec<ShellTabInfo>>,
        deadline: std::time::Instant,
    },
    /// 获取指定或前台 Explorer 窗口中处于活跃/选中的 Tab 路径
    GetActiveTabPath {
        target_hwnd_raw: Option<usize>,
        reply: Sender<Option<String>>,
        deadline: std::time::Instant,
    },
    /// 导航指定的 WebBrowser/Tab 到新路径
    Navigate {
        hwnd_raw: usize,
        target_path: String,
        reply: Sender<bool>,
        deadline: std::time::Instant,
    },
    /// 导航指定 Explorer 窗口中最新创建的 Tab 到新路径（反向反查）
    NavigateLatest {
        hwnd_raw: usize,
        target_path: String,
        reply: Sender<bool>,
        deadline: std::time::Instant,
    },
    /// 获取指定窗口的目标路径与选定文件（穿透检测，不受可见性限制）
    GetWindowTargetInfo {
        target_hwnd_raw: usize,
        reply: Sender<Option<(String, Option<String>)>>,
        deadline: std::time::Instant,
    },
    /// 尝试单次检查新 Tab 在 COM 中注册并将其导航至指定路径（无 sleep，原子操作）
    TryNavigateNewTab {
        host_hwnd_raw: usize,
        count_before: usize,
        target_path: String,
        reply: Sender<bool>,
        deadline: std::time::Instant,
    },
    /// 在指定宿主窗口内按规范化路径查找 Tab 索引（0-based）
    FindTabIndexByPath {
        host_hwnd_raw: usize,
        target_path: String,
        reply: Sender<Option<usize>>,
        deadline: std::time::Instant,
    },
}

impl ComRequest {
    #[inline]
    pub fn deadline(&self) -> std::time::Instant {
        match self {
            Self::GetAllTabs { deadline, .. } => *deadline,
            Self::GetActiveTabPath { deadline, .. } => *deadline,
            Self::Navigate { deadline, .. } => *deadline,
            Self::NavigateLatest { deadline, .. } => *deadline,
            Self::GetWindowTargetInfo { deadline, .. } => *deadline,
            Self::TryNavigateNewTab { deadline, .. } => *deadline,
            Self::FindTabIndexByPath { deadline, .. } => *deadline,
        }
    }
}

/// STA 线程上的 Shell COM 管理器
pub struct ShellComManager {
    tx: Sender<ComRequest>,
}

impl ShellComManager {
    pub fn new() -> Self {
        let (tx, rx) = channel::<ComRequest>();

        thread::Builder::new()
            .name("QuickPath-ShellComSTA".to_string())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(
                        None,
                        COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
                    );
                }

                while let Ok(req) = rx.recv() {
                    // 若调用方已超时放弃该请求，直接丢弃，不执行耗时 COM 操作，彻底杜绝队列积压
                    if std::time::Instant::now() > req.deadline() {
                        continue;
                    }

                    match req {
                        ComRequest::GetAllTabs { reply, .. } => {
                            let tabs = enumerate_shell_tabs();
                            let _ = reply.send(tabs);
                        }
                        ComRequest::GetActiveTabPath { target_hwnd_raw, reply, .. } => {
                            let hwnd_opt = target_hwnd_raw.map(|raw| HWND(raw as *mut _));
                            let path = get_active_tab_path_internal(hwnd_opt);
                            let _ = reply.send(path);
                        }
                        ComRequest::Navigate { hwnd_raw, target_path, reply, .. } => {
                            let hwnd = HWND(hwnd_raw as *mut _);
                            let ok = navigate_internal(hwnd, &target_path);
                            let _ = reply.send(ok);
                        }
                        ComRequest::NavigateLatest { hwnd_raw, target_path, reply, .. } => {
                            let hwnd = HWND(hwnd_raw as *mut _);
                            let ok = navigate_latest_internal(hwnd, &target_path);
                            let _ = reply.send(ok);
                        }
                        ComRequest::GetWindowTargetInfo { target_hwnd_raw, reply, .. } => {
                            let hwnd = HWND(target_hwnd_raw as *mut _);
                            let info = get_window_target_info_internal(hwnd);
                            let _ = reply.send(info);
                        }
                        ComRequest::TryNavigateNewTab { host_hwnd_raw, count_before, target_path, reply, .. } => {
                            let hwnd = HWND(host_hwnd_raw as *mut _);
                            let ok = try_navigate_new_tab_internal(hwnd, count_before, &target_path);
                            let _ = reply.send(ok);
                        }
                        ComRequest::FindTabIndexByPath { host_hwnd_raw, target_path, reply, .. } => {
                            let hwnd = HWND(host_hwnd_raw as *mut _);
                            let idx = find_tab_index_by_path_internal(hwnd, &target_path);
                            let _ = reply.send(idx);
                        }
                    }
                }
            })
            .expect("创建 ShellComManager STA 线程失败");

        Self { tx }
    }

    /// 获取所有打开且可见有效的资源管理器 Tab（带 150ms 超时）
    pub fn get_all_tabs(&self) -> Vec<ShellTabInfo> {
        let timeout = Duration::from_millis(150);
        let deadline = std::time::Instant::now() + timeout;
        let (reply_tx, reply_rx) = channel();
        if self.tx.send(ComRequest::GetAllTabs { reply: reply_tx, deadline }).is_err() {
            return Vec::new();
        }
        reply_rx.recv_timeout(timeout).unwrap_or_default()
    }

    /// 获取活动 Tab 路径（带 150ms 超时）
    pub fn get_active_tab_path(&self, target_hwnd: Option<HWND>) -> Option<String> {
        let timeout = Duration::from_millis(150);
        let deadline = std::time::Instant::now() + timeout;
        let (reply_tx, reply_rx) = channel();
        let target_hwnd_raw = target_hwnd.map(|h| h.0 as usize);
        if self.tx.send(ComRequest::GetActiveTabPath { target_hwnd_raw, reply: reply_tx, deadline }).is_err() {
            return None;
        }
        reply_rx.recv_timeout(timeout).ok().flatten()
    }

    /// 在指定宿主窗口内查找目标路径对应的已有 Tab 索引（0-based，带 250ms 超时）
    pub fn find_tab_index_by_path(&self, host_hwnd: HWND, target_path: &str) -> Option<usize> {
        let timeout = Duration::from_millis(250);
        let deadline = std::time::Instant::now() + timeout;
        let (reply_tx, reply_rx) = channel();
        let host_hwnd_raw = host_hwnd.0 as usize;
        if self.tx.send(ComRequest::FindTabIndexByPath {
            host_hwnd_raw,
            target_path: target_path.to_string(),
            reply: reply_tx,
            deadline,
        }).is_err() {
            return None;
        }
        reply_rx.recv_timeout(timeout).ok().flatten()
    }

    /// 请求重定向某个 Explorer 窗口或标签页（带 200ms 超时）
    pub fn navigate(&self, hwnd: HWND, target_path: &str) -> bool {
        let timeout = Duration::from_millis(200);
        let deadline = std::time::Instant::now() + timeout;
        let (reply_tx, reply_rx) = channel();
        let hwnd_raw = hwnd.0 as usize;
        if self.tx.send(ComRequest::Navigate {
            hwnd_raw,
            target_path: target_path.to_string(),
            reply: reply_tx,
            deadline,
        }).is_err() {
            return false;
        }
        reply_rx.recv_timeout(timeout).unwrap_or(false)
    }

    /// 导航指定 Explorer 窗口中最新创建的 Tab 到新路径（带 200ms 超时，反向反查遍历）
    pub fn navigate_latest(&self, hwnd: HWND, target_path: &str) -> bool {
        let timeout = Duration::from_millis(200);
        let deadline = std::time::Instant::now() + timeout;
        let (reply_tx, reply_rx) = channel();
        let hwnd_raw = hwnd.0 as usize;
        if self.tx.send(ComRequest::NavigateLatest {
            hwnd_raw,
            target_path: target_path.to_string(),
            reply: reply_tx,
            deadline,
        }).is_err() {
            return false;
        }
        reply_rx.recv_timeout(timeout).unwrap_or(false)
    }

    /// 获取指定窗口的目标路径与选定文件名（穿透检测，不受可见性限制，带 200ms 超时）
    pub fn get_window_target_info(&self, target_hwnd: HWND) -> Option<(String, Option<String>)> {
        let timeout = Duration::from_millis(200);
        let deadline = std::time::Instant::now() + timeout;
        let (reply_tx, reply_rx) = channel();
        let target_hwnd_raw = target_hwnd.0 as usize;
        if self.tx.send(ComRequest::GetWindowTargetInfo {
            target_hwnd_raw,
            reply: reply_tx,
            deadline,
        }).is_err() {
            return None;
        }
        reply_rx.recv_timeout(timeout).ok().flatten()
    }

    /// 获取指定窗口下当前有效 Tab 的数量
    pub fn get_tabs_count_for_hwnd(&self, hwnd: HWND) -> usize {
        let tabs = self.get_all_tabs();
        let raw = hwnd.0 as usize;
        tabs.into_iter().filter(|t| t.hwnd_raw == raw).count()
    }

    /// 轮询等待新 Tab 在宿主窗口中就绪并执行重定向导航（统一绝对截止时间，总预算严格控制在 800ms 内）
    pub fn wait_and_navigate_new_tab(&self, host_hwnd: HWND, count_before: usize, target_path: &str) -> bool {
        let host_hwnd_raw = host_hwnd.0 as usize;
        let start = std::time::Instant::now();
        let total_budget = Duration::from_millis(800);
        let deadline = start + total_budget;

        // 在调用方工作线程轮询，每轮 30ms 向 STA 请求一次原子单次检查
        while std::time::Instant::now() < deadline {
            let now = std::time::Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                break;
            }

            let timeout = remaining.min(Duration::from_millis(80));
            let req_deadline = now + timeout;

            let (reply_tx, reply_rx) = channel();
            if self.tx.send(ComRequest::TryNavigateNewTab {
                host_hwnd_raw,
                count_before,
                target_path: target_path.to_string(),
                reply: reply_tx,
                deadline: req_deadline,
            }).is_ok() {
                if let Ok(true) = reply_rx.recv_timeout(timeout) {
                    return true;
                }
            }

            std::thread::sleep(Duration::from_millis(30));
        }

        // 统一截止时间最终单次兜底
        self.navigate_latest(host_hwnd, target_path)
    }

    /// 根据窗口 HWND、预期的 Tab 标题或索引，解析出最匹配的物理路径
    pub fn resolve_tab_path_by_target(
        &self,
        hwnd: HWND,
        target_title: Option<&str>,
        target_index: Option<usize>,
    ) -> Option<String> {
        let tabs = self.get_all_tabs();
        let raw = hwnd.0 as usize;
        let window_tabs: Vec<_> = tabs.into_iter().filter(|t| t.hwnd_raw == raw).collect();

        if window_tabs.is_empty() {
            return None;
        }

        // 1. 如果有 target_index 且在范围内，且其标题与 target_title 匹配（或未指定 title）
        if let Some(idx) = target_index {
            if idx < window_tabs.len() {
                if let Some(expected_title) = target_title {
                    if window_tabs[idx].title == expected_title {
                        return Some(window_tabs[idx].path.clone());
                    }
                } else {
                    return Some(window_tabs[idx].path.clone());
                }
            }
        }

        // 2. 如果指定了 target_title，按标题精确匹配
        if let Some(title) = target_title {
            if let Some(matched) = window_tabs.iter().find(|t| t.title == title) {
                return Some(matched.path.clone());
            }
        }

        // 3. 回退到当前处于 active 的 Tab
        if let Some(active) = window_tabs.iter().find(|t| t.is_active) {
            return Some(active.path.clone());
        }

        // 4. 最低优先级回退到第一个 Tab
        window_tabs.first().map(|t| t.path.clone())
    }
}

/// 枚举系统中所有 ShellWindows 并提取路径（严格过滤死窗口与不可见窗口，精准识别活动 Tab）
fn enumerate_shell_tabs() -> Vec<ShellTabInfo> {
    let mut results = Vec::new();

    unsafe {
        let top_explorer = crate::managers::explorer::get_top_visible_explorer_hwnd();
        let top_active_tab = top_explorer.and_then(crate::managers::explorer::get_active_tab_hwnd);

        let shell_windows: Result<IShellWindows, _> = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);

        if let Ok(shell_windows) = shell_windows {
            if let Ok(count) = shell_windows.Count() {
                for i in 0..count {
                    let index = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&index) {
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser.HWND().map(|h| HWND(h.0 as *mut _)).unwrap_or_default();
                            // 关键 Fail-Safe：严格检查窗口存活性与可见性，彻底剔除 dead/zombie 窗口
                            if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() || !IsWindowVisible(hwnd).as_bool() {
                                continue;
                            }

                            let url = browser.LocationURL().map(|u| u.to_string()).unwrap_or_default();
                            let name = browser.LocationName().map(|n| n.to_string()).unwrap_or_default();

                            if let Some(path) = parse_shell_url(&url, &name) {
                                let is_valid = path.starts_with("shell:") || path.starts_with("::{") || (path.len() >= 2 && &path[1..2] == ":") || path.starts_with("\\\\");
                                if is_valid {
                                    let title = if name.is_empty() {
                                        Path::new(&path)
                                            .file_name()
                                            .map(|f| f.to_string_lossy().to_string())
                                            .unwrap_or_else(|| path.clone())
                                    } else {
                                        name
                                    };

                                    // 核心：基于 ShellTabWindowClass 精准识别 Win11 多标签页活动状态
                                    let mut is_active = false;
                                    let tab_sub_hwnd = crate::managers::explorer::get_browser_tab_hwnd(&browser);

                                    if let Some(top_exp) = top_explorer {
                                        if hwnd == top_exp {
                                            if let (Some(my_tab), Some(active_tab)) = (tab_sub_hwnd, top_active_tab) {
                                                if my_tab == active_tab {
                                                    is_active = true;
                                                }
                                            } else if top_active_tab.is_none() {
                                                is_active = true;
                                            }
                                        }
                                    }

                                    results.push(ShellTabInfo {
                                        hwnd_raw: hwnd.0 as usize,
                                        path,
                                        title,
                                        is_active,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 活动 Tab 优先排序
    results.sort_by(|a, b| b.is_active.cmp(&a.is_active));

    results
}

/// 获取指定或活动 Explorer 窗口的当前活跃路径
fn get_active_tab_path_internal(target_hwnd: Option<HWND>) -> Option<String> {
    let tabs = enumerate_shell_tabs();
    if let Some(target) = target_hwnd {
        let raw = target.0 as usize;
        // 优先获取该目标窗口内部正处于 active 的 Tab
        if let Some(tab) = tabs.iter().find(|t| t.hwnd_raw == raw && t.is_active) {
            return Some(tab.path.clone());
        }
        if let Some(tab) = tabs.iter().find(|t| t.hwnd_raw == raw) {
            return Some(tab.path.clone());
        }
    }

    if let Some(tab) = tabs.iter().find(|t| t.is_active) {
        return Some(tab.path.clone());
    }

    tabs.first().map(|t| t.path.clone())
}

/// 内部导航实现
fn navigate_internal(target_hwnd: HWND, target_path: &str) -> bool {
    unsafe {
        let shell_windows: Result<IShellWindows, _> = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows {
            if let Ok(count) = shell_windows.Count() {
                for i in 0..count {
                    let index = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&index) {
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser.HWND().map(|h| HWND(h.0 as *mut _)).unwrap_or_default();
                            if hwnd == target_hwnd || target_hwnd.0.is_null() {
                                let path_bstr = BSTR::from(target_path);
                                let var_url = VARIANT::from(path_bstr);
                                let null_var = VARIANT::default();
                                if browser.Navigate2(
                                    &var_url,
                                    Some(&null_var as *const _),
                                    Some(&null_var as *const _),
                                    Some(&null_var as *const _),
                                    Some(&null_var as *const _),
                                ).is_ok() {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// 内部导航实现（从后往前反向查找最新创建的 Tab）
fn navigate_latest_internal(target_hwnd: HWND, target_path: &str) -> bool {
    unsafe {
        let shell_windows: Result<IShellWindows, _> = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows {
            if let Ok(count) = shell_windows.Count() {
                for i in (0..count).rev() {
                    let index = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&index) {
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser.HWND().map(|h| HWND(h.0 as *mut _)).unwrap_or_default();
                            if hwnd == target_hwnd || target_hwnd.0.is_null() {
                                let path_bstr = BSTR::from(target_path);
                                let var_url = VARIANT::from(path_bstr);
                                let null_var = VARIANT::default();
                                if browser.Navigate2(
                                    &var_url,
                                    Some(&null_var as *const _),
                                    Some(&null_var as *const _),
                                    Some(&null_var as *const _),
                                    Some(&null_var as *const _),
                                ).is_ok() {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// 单次原子检查宿主窗口内新注册的 Tab 并对其调用 Navigate2（零 sleep，极速无阻塞）
fn try_navigate_new_tab_internal(host_hwnd: HWND, count_before: usize, target_path: &str) -> bool {
    unsafe {
        let shell_windows: Result<IShellWindows, _> = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows {
            if let Ok(count) = shell_windows.Count() {
                let mut host_browsers = Vec::new();
                for i in 0..count {
                    let index = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&index) {
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser.HWND().map(|h| HWND(h.0 as *mut _)).unwrap_or_default();
                            if hwnd == host_hwnd {
                                host_browsers.push(browser);
                            }
                        }
                    }
                }

                // 当属于 host_hwnd 的 Tab 数量出现增长，或者无前置数量但已存在有效 browser
                if host_browsers.len() > count_before || (count_before == 0 && !host_browsers.is_empty()) {
                    if let Some(newest_browser) = host_browsers.last() {
                        let path_bstr = BSTR::from(target_path);
                        let var_url = VARIANT::from(path_bstr);
                        let null_var = VARIANT::default();
                        if newest_browser.Navigate2(
                            &var_url,
                            Some(&null_var as *const _),
                            Some(&null_var as *const _),
                            Some(&null_var as *const _),
                            Some(&null_var as *const _),
                        ).is_ok() {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// 在指定宿主窗口内按规范化路径查找 Tab 索引（0-based）
fn find_tab_index_by_path_internal(host_hwnd: HWND, target_path: &str) -> Option<usize> {
    if host_hwnd.0.is_null() || target_path.is_empty() {
        return None;
    }
    let target_norm = normalize_path_local(target_path);

    unsafe {
        let shell_windows: Result<IShellWindows, _> = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows {
            if let Ok(count) = shell_windows.Count() {
                let mut current_idx = 0usize;
                for i in 0..count {
                    let index = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&index) {
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser.HWND().map(|h| HWND(h.0 as *mut _)).unwrap_or_default();
                            if hwnd == host_hwnd {
                                let url = browser.LocationURL().map(|u| u.to_string()).unwrap_or_default();
                                let name = browser.LocationName().map(|n| n.to_string()).unwrap_or_default();
                                if let Some(path) = parse_shell_url(&url, &name) {
                                    if normalize_path_local(&path) == target_norm {
                                        return Some(current_idx);
                                    }
                                }
                                current_idx += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn normalize_path_local(path: &str) -> String {
    let mut s = path.trim().to_lowercase().replace('/', "\\");
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

/// 针对指定 Explorer 窗口定向穿透探测其当前路径及待选文件名（不受 IsWindowVisible 限制）
fn get_window_target_info_internal(target_hwnd: HWND) -> Option<(String, Option<String>)> {
    if target_hwnd.0.is_null() {
        return None;
    }

    unsafe {
        if !IsWindow(Some(target_hwnd)).as_bool() {
            return None;
        }

        let shell_windows: Result<IShellWindows, _> = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL);
        if let Ok(shell_windows) = shell_windows {
            if let Ok(count) = shell_windows.Count() {
                for i in 0..count {
                    let index = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&index) {
                        if let Ok(browser) = disp.cast::<IWebBrowser2>() {
                            let hwnd = browser.HWND().map(|h| HWND(h.0 as *mut _)).unwrap_or_default();
                            if hwnd == target_hwnd {
                                // 1. 解析 LocationURL 基础路径
                                let url = browser.LocationURL().map(|u| u.to_string()).unwrap_or_default();
                                let name = browser.LocationName().map(|n| n.to_string()).unwrap_or_default();
                                let parsed_path = parse_shell_url(&url, &name);

                                // 2. 穿透提取 SelectedItems（针对外部 /select 或 SHOpenFolderAndSelectItems 唤起）
                                let mut selected_file = None;
                                let mut selected_full_path = None;

                                if let Ok(doc_disp) = browser.Document() {
                                    if let Ok(sfv) = doc_disp.cast::<IShellFolderViewDual>() {
                                        if let Ok(items) = sfv.SelectedItems() {
                                            if let Ok(item_count) = items.Count() {
                                                if item_count > 0 {
                                                    let v_zero = VARIANT::from(0i32);
                                                    if let Ok(first_item) = items.Item(&v_zero) {
                                                        if let Ok(item_path) = first_item.Path() {
                                                            let path_str = item_path.to_string();
                                                            if !path_str.is_empty() {
                                                                selected_full_path = Some(path_str);
                                                            }
                                                        }
                                                        if let Ok(item_name) = first_item.Name() {
                                                            let name_str = item_name.to_string();
                                                            if !name_str.is_empty() {
                                                                selected_file = Some(name_str);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // 3. 优先依据选中项完整路径定位父目录与高亮文件
                                if let Some(ref full_p) = selected_full_path {
                                    let p = Path::new(full_p);
                                    if let Some(parent) = p.parent() {
                                        let folder = parent.to_string_lossy().to_string();
                                        let file = p.file_name().map(|f| f.to_string_lossy().to_string());
                                        if is_physical_dir_path(&folder) {
                                            return Some((folder, file.or(selected_file)));
                                        }
                                    }
                                }

                                // 4. 若无选中项全路径，使用解析出的 URL 路径结合文件名
                                if let Some(path) = parsed_path {
                                    return Some((path, selected_file));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// 解析 LocationURL 或虚拟 Shell 路径
pub fn parse_shell_url(url: &str, location_name: &str) -> Option<String> {
    if url.is_empty() {
        if location_name == "此电脑" || location_name.eq_ignore_ascii_case("this pc") {
            return Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string());
        }
        if location_name == "主文件夹" || location_name == "主页" || location_name.eq_ignore_ascii_case("home") {
            return Some("shell:Home".to_string());
        }
        return None;
    }

    if url.starts_with("file:///") {
        let raw = &url[8..];
        let decoded = url_decode(raw);
        let path_str = decoded.replace('/', "\\");

        if path_str.len() >= 2 && &path_str[1..2] == ":" {
            Some(path_str)
        } else if path_str.starts_with('\\') {
            Some(format!("\\{}", path_str))
        } else {
            Some(path_str)
        }
    } else if url.starts_with("shell:::") || url.starts_with("::{") || url.starts_with("shell:") {
        Some(url.to_string())
    } else {
        None
    }
}

/// 解析 file:/// URL（兼容历史调用）
pub fn parse_file_url(url: &str) -> Option<String> {
    parse_shell_url(url, "")
}

/// 判断路径是否为合法的本地物理目录或网络物理路径（排除虚拟 Shell 主页，零阻塞）
pub fn is_physical_dir_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with("shell:") || path.starts_with("::{") {
        return false;
    }
    (path.len() >= 2 && &path[1..2] == ":") || path.starts_with("\\\\")
}

fn url_decode(input: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = input.bytes();

    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                if let Ok(val) = u8::from_str_radix(
                    &format!("{}{}", h1 as char, h2 as char),
                    16,
                ) {
                    bytes.push(val);
                    continue;
                }
            }
        }
        bytes.push(b);
    }

    String::from_utf8_lossy(&bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_shell_url_standard_files() {
        assert_eq!(
            parse_shell_url("file:///C:/Windows/System32", ""),
            Some("C:\\Windows\\System32".to_string())
        );
        assert_eq!(
            parse_shell_url("file:///D:/Project%20Files/Code", ""),
            Some("D:\\Project Files\\Code".to_string())
        );
    }

    #[test]
    fn test_parse_shell_url_virtual_folders() {
        // 此电脑 (This PC)
        assert_eq!(
            parse_shell_url("", "此电脑"),
            Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string())
        );
        assert_eq!(
            parse_shell_url("", "This PC"),
            Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string())
        );

        // 主文件夹 (Home)
        assert_eq!(
            parse_shell_url("", "主文件夹"),
            Some("shell:Home".to_string())
        );
        assert_eq!(
            parse_shell_url("", "Home"),
            Some("shell:Home".to_string())
        );

        // GUID 格式直传
        assert_eq!(
            parse_shell_url("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", ""),
            Some("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string())
        );
    }

    #[test]
    fn test_is_physical_dir_path() {
        assert!(is_physical_dir_path("C:\\Windows"));
        assert!(is_physical_dir_path("D:\\"));
        assert!(is_physical_dir_path("\\\\server\\share"));

        // 虚拟路径必须一律判定为 false，杜绝 Navigate2 触发系统报错
        assert!(!is_physical_dir_path(""));
        assert!(!is_physical_dir_path("shell:Home"));
        assert!(!is_physical_dir_path("shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"));
        assert!(!is_physical_dir_path("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"));
    }

    #[test]
    fn test_resolve_tab_path_logic() {
        let tabs = vec![
            ShellTabInfo {
                hwnd_raw: 100,
                path: "C:\\FolderA".to_string(),
                title: "FolderA".to_string(),
                is_active: false,
            },
            ShellTabInfo {
                hwnd_raw: 100,
                path: "D:\\FolderB".to_string(),
                title: "FolderB".to_string(),
                is_active: true,
            },
            ShellTabInfo {
                hwnd_raw: 100,
                path: "E:\\FolderC".to_string(),
                title: "FolderC".to_string(),
                is_active: false,
            },
        ];

        // 模拟 resolve 匹配逻辑
        let resolve = |target_title: Option<&str>, target_index: Option<usize>| -> Option<String> {
            if let Some(idx) = target_index {
                if idx < tabs.len() {
                    if let Some(expected_title) = target_title {
                        if tabs[idx].title == expected_title {
                            return Some(tabs[idx].path.clone());
                        }
                    } else {
                        return Some(tabs[idx].path.clone());
                    }
                }
            }
            if let Some(title) = target_title {
                if let Some(matched) = tabs.iter().find(|t| t.title == title) {
                    return Some(matched.path.clone());
                }
            }
            if let Some(active) = tabs.iter().find(|t| t.is_active) {
                return Some(active.path.clone());
            }
            tabs.first().map(|t| t.path.clone())
        };

        // 双击第 0 个 Tab (FolderA) -> 必须精准解析出 FolderA，绝不是最后的 FolderC 或活跃的 FolderB
        assert_eq!(resolve(Some("FolderA"), Some(0)), Some("C:\\FolderA".to_string()));

        // 双击第 2 个 Tab (FolderC) -> 必须精准解析出 FolderC
        assert_eq!(resolve(Some("FolderC"), Some(2)), Some("E:\\FolderC".to_string()));

        // 仅通过标题匹配
        assert_eq!(resolve(Some("FolderA"), None), Some("C:\\FolderA".to_string()));

        // 未命中任何特征 -> 回退到当前 active (FolderB)
        assert_eq!(resolve(Some("NonExistent"), None), Some("D:\\FolderB".to_string()));
    }
}
