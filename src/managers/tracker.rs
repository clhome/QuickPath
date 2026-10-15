use crate::rules::AppConfig;
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;
use windows::Win32::Foundation::HWND;

#[derive(Debug, Clone)]
pub struct FolderCandidate {
    pub path: String,
    pub title: String,
    pub source: String,
    pub is_active: bool,
}

#[derive(Default)]
pub struct WindowTracker {
    pub last_manager_path: Option<String>,
    pub last_manager_time: Option<Instant>,
    pub last_manager_hwnd: Option<HWND>,
    pub cached_candidates: Vec<FolderCandidate>,
    pub last_candidates_time: Option<Instant>,
}

impl WindowTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次由后台扫描/极速路径确认的文件管理器目录。
    ///
    /// 审计报告 P1-02：探测本身必须在后台线程完成，宿主消息线程只负责
    /// 把结果写回这里；因此这里只做纯赋值，不含任何跨进程调用。
    pub fn note_manager_folder(&mut self, path: String, hwnd: HWND) {
        self.last_manager_path = Some(path);
        self.last_manager_time = Some(Instant::now());
        self.last_manager_hwnd = Some(hwnd);
    }

    /// 获取最推荐的自动秒切目标目录 (AutoSwitch 路径)
    pub fn get_auto_switch_target(&mut self, config: &AppConfig) -> Option<String> {
        // 1. 核心提速：极速探测当前或最顶层可见资源管理器的活动 Tab 路径（纳秒/微秒级短路命中）
        if config.enable_win11_tabs
            && let Some(fast_folder) = super::explorer::get_active_explorer_folder_fast()
        {
            let norm = normalize_path(&fast_folder);
            self.last_manager_path = Some(norm.clone());
            self.last_manager_time = Some(Instant::now());
            return Some(norm);
        }

        // 2. 检查最近活跃的文件管理器缓存（放宽至 30 分钟内有效，避免用户阅读/编辑较久后缓存失效）
        if let (Some(path), Some(time)) = (&self.last_manager_path, self.last_manager_time)
            && time.elapsed().as_secs() < 1800
            && is_fast_valid_folder(path)
        {
            return Some(path.clone());
        }

        // 3. 资源管理器（含 Win11 标签页）与第三方管理器：读取后台 scanner 的
        //    零预算快照（绝不阻塞）。宿主消息线程不再做任何跨进程同步探测。
        for candidate in super::scanner::scan_within(
            config.enable_win11_tabs,
            config.enable_dopus,
            std::time::Duration::ZERO,
        ) {
            if candidate.is_active && is_fast_valid_folder(&candidate.path) {
                self.last_manager_path = Some(candidate.path.clone());
                self.last_manager_time = Some(Instant::now());
                return Some(candidate.path);
            }
        }

        // 4. 回退策略：使用最后一次访问的历史记录路径 (Last Used Folder)
        if let Some(last_history) = config.history_folders.first()
            && is_fast_valid_folder(last_history)
        {
            return Some(normalize_path(last_history));
        }

        // 5. 回退策略：使用第一个收藏夹路径
        if let Some(first_pinned) = config.pinned_folders.first()
            && is_fast_valid_folder(first_pinned)
        {
            return Some(normalize_path(first_pinned));
        }

        None
    }

    /// 收集所有候选路径（包括资源管理器与第三方管理器的窗口/Tab、历史记录、收藏夹与系统常用）。
    ///
    /// **P1-02 关键约束**：本函数运行在宿主消息线程上，因此**只做零等待读取**——
    /// 资源管理器与第三方管理器候选全部来自后台 `scanner` 的最近一次快照
    /// （`scan_within(..., Duration::ZERO)` 会顺带请求下一轮后台扫描，但绝不等待）。
    pub fn get_all_candidates(&mut self, config: &AppConfig) -> Vec<FolderCandidate> {
        let mut list = Vec::new();
        let mut seen = HashSet::new();

        // 1~4. 资源管理器（含 Win11 标签页）与 Directory Opus：一次性读取后台快照。
        //      缓存可能来自上一次扫描，因此当前配置已关闭的来源一律过滤掉。
        //      （审计报告 U-01：TC / XYplorer 已不在后台扫描范围内，候选不会出现这两个来源）
        for candidate in super::scanner::scan_within(
            config.enable_win11_tabs,
            config.enable_dopus,
            std::time::Duration::ZERO,
        ) {
            let enabled = match candidate.source.as_str() {
                "资源管理器" => config.enable_win11_tabs,
                "Directory Opus" => config.enable_dopus,
                _ => true,
            };
            if enabled && seen.insert(candidate.path.to_lowercase()) {
                list.push(candidate);
            }
        }

        // 5. 最近历史记录 (History)
        for h in &config.history_folders {
            let norm = normalize_path(h);
            if is_fast_valid_folder(&norm) && seen.insert(norm.to_lowercase()) {
                list.push(FolderCandidate {
                    path: norm.clone(),
                    title: get_folder_name(&norm),
                    source: "最近历史".to_string(),
                    is_active: false,
                });
            }
        }

        // 6. 常用固定 (Pinned)
        for p in &config.pinned_folders {
            let norm = normalize_path(p);
            if is_fast_valid_folder(&norm) && seen.insert(norm.to_lowercase()) {
                list.push(FolderCandidate {
                    path: norm.clone(),
                    title: get_folder_name(&norm),
                    source: "常用固定".to_string(),
                    is_active: false,
                });
            }
        }

        // 7. 如果列表仍完全为空，补齐系统常用目录（下载、桌面、文档），确保绝不为空！
        if list.is_empty()
            && let Some(user_dirs) = directories::UserDirs::new()
        {
            if let Some(down) = user_dirs.download_dir() {
                let norm = normalize_path(&down.to_string_lossy());
                if seen.insert(norm.to_lowercase()) {
                    list.push(FolderCandidate {
                        path: norm,
                        title: crate::rules::I18n::dir_download(&config.language).to_string(),
                        source: "系统目录".to_string(),
                        is_active: false,
                    });
                }
            }
            let desk = user_dirs
                .desktop_dir()
                .map(|d| d.to_string_lossy().to_string());
            if let Some(desk) = desk {
                let norm = normalize_path(&desk);
                if seen.insert(norm.to_lowercase()) {
                    list.push(FolderCandidate {
                        path: norm,
                        title: crate::rules::I18n::dir_desktop(&config.language).to_string(),
                        source: "系统目录".to_string(),
                        is_active: false,
                    });
                }
            }
            let doc = user_dirs
                .document_dir()
                .map(|d| d.to_string_lossy().to_string());
            if let Some(doc) = doc {
                let norm = normalize_path(&doc);
                if seen.insert(norm.to_lowercase()) {
                    list.push(FolderCandidate {
                        path: norm,
                        title: crate::rules::I18n::dir_documents(&config.language).to_string(),
                        source: "系统目录".to_string(),
                        is_active: false,
                    });
                }
            }
        }

        // 排序：活动项最前
        list.sort_by(|a, b| b.is_active.cmp(&a.is_active));

        self.cached_candidates = list.clone();
        self.last_candidates_time = Some(Instant::now());

        list
    }
}

/// 判断给定的窗口类是否属于常见文件管理器
pub fn is_file_manager_class(class_name: &str) -> bool {
    matches!(
        class_name,
        "CabinetWClass" | "ExploreWClass" | "dopus.lister" | "TTOTAL_CMD" | "ThunderRT6FormDC"
    )
}

/// 快速校验目录路径有效性（网络 UNC 路径零 I/O 阻塞，本地盘符免除热路径阻塞）
pub fn is_fast_valid_folder(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    // 1. 网络/UNC 路径 (\\server\share):格式校验,坚决不调用 Path::is_dir() 触发 SMB 超时卡死
    if let Some(rest) = trimmed.strip_prefix("\\\\") {
        return rest.len() >= 3 && rest.contains('\\');
    }
    // 2. 本地绝对路径 (如 C:\...):热路径优先信任格式,排除虚拟或非法格式
    if trimmed.len() >= 3 && &trimmed[1..3] == ":\\" {
        return true;
    }
    Path::new(trimmed).is_dir()
}

/// 路径归一化：scanner 模块（同属 managers）也要复用同一套规则，避免两处实现漂移
pub(super) fn normalize_path(path: &str) -> String {
    let mut p = path.trim().replace('/', "\\");
    while p.ends_with('\\') && p.len() > 3 {
        p.pop();
    }
    p
}

fn get_folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// P1-02：即使来源已开启，宿主消息线程侧收集候选也必须零等待返回
    /// （首轮扫描在后台 worker 里进行，绝不阻塞调用线程）。
    ///
    /// 只开启资源管理器来源：第三方来源会触发真实 IPC / 剪贴板操作，
    /// 不宜在并发单测中引入额外副作用。
    #[test]
    fn test_get_all_candidates_is_zero_wait_even_when_source_enabled() {
        let mut tracker = WindowTracker::new();
        let config = AppConfig {
            enable_win11_tabs: true,
            enable_dopus: false,
            enable_totalcmd: false,
            enable_xyplorer: false,
            ..AppConfig::default()
        };

        let start = Instant::now();
        let _ = tracker.get_all_candidates(&config);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "候选收集必须在宿主线程零等待返回，实际 {:?}",
            start.elapsed()
        );
    }

    /// P1-02：自动秒切目标解析同样不得在宿主线程做同步跨进程等待。
    #[test]
    fn test_get_auto_switch_target_is_zero_wait() {
        let mut tracker = WindowTracker::new();
        // 只开启资源管理器：关闭极速直连路径，强制走“读取后台快照”的分支
        let config = AppConfig {
            enable_win11_tabs: true,
            enable_dopus: false,
            enable_totalcmd: false,
            enable_xyplorer: false,
            ..AppConfig::default()
        };

        let start = Instant::now();
        let _ = tracker.get_auto_switch_target(&config);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "目标解析不得阻塞宿主线程，实际 {:?}",
            start.elapsed()
        );
    }
}
