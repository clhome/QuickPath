use super::dopus::get_dopus_folders;
use super::explorer::get_explorer_folders;
use super::totalcmd::get_totalcmd_folders;
use super::xyplorer::get_xyplorer_folders;
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
}

impl WindowTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当系统前台窗口改变时调用，若为文件管理器则记录该状态
    pub fn on_foreground_change(&mut self, hwnd: HWND, class_name: &str, config: &AppConfig) {
        if is_file_manager_class(class_name) {
            let active_folder = get_current_active_folder(config);
            if let Some(folder) = active_folder {
                self.last_manager_path = Some(folder);
                self.last_manager_time = Some(Instant::now());
                self.last_manager_hwnd = Some(hwnd);
            }
        }
    }

    /// 获取最推荐的自动秒切目标目录 (AutoSwitch 路径)
    pub fn get_auto_switch_target(&self, config: &AppConfig) -> Option<String> {
        // 1. 如果最近（5分钟内）活跃过某个文件管理器，优先使用该路径
        if let (Some(path), Some(time)) = (&self.last_manager_path, self.last_manager_time) {
            if time.elapsed().as_secs() < 300 && Path::new(path).is_dir() {
                return Some(path.clone());
            }
        }

        // 2. 实时扫描当前打开的活动/首选文件管理器文件夹
        if let Some(folder) = get_current_active_folder(config) {
            return Some(folder);
        }

        // 3. 回退策略：使用最后一次访问的历史记录路径 (Last Used Folder)
        if let Some(last_history) = config.history_folders.first() {
            if Path::new(last_history).is_dir() {
                return Some(normalize_path(last_history));
            }
        }

        // 4. 回退策略：使用第一个收藏夹路径
        if let Some(first_pinned) = config.pinned_folders.first() {
            if Path::new(first_pinned).is_dir() {
                return Some(normalize_path(first_pinned));
            }
        }

        None
    }

    /// 收集所有候选路径（包含所有打开的窗口/Tab、历史记录、收藏夹与系统常用）
    pub fn get_all_candidates(&self, config: &AppConfig) -> Vec<FolderCandidate> {
        let mut list = Vec::new();
        let mut seen = HashSet::new();

        // 1. 扫描 Windows 资源管理器 (含 Win11 标签页)
        if config.enable_win11_tabs {
            for f in get_explorer_folders() {
                let norm = normalize_path(&f.path);
                if seen.insert(norm.to_lowercase()) {
                    list.push(FolderCandidate {
                        path: norm,
                        title: f.title,
                        source: "资源管理器".to_string(),
                        is_active: f.is_active,
                    });
                }
            }
        }

        // 2. Directory Opus
        if config.enable_dopus {
            for f in get_dopus_folders() {
                let norm = normalize_path(&f.path);
                if seen.insert(norm.to_lowercase()) {
                    list.push(FolderCandidate {
                        path: norm,
                        title: f.title,
                        source: "Directory Opus".to_string(),
                        is_active: f.is_active,
                    });
                }
            }
        }

        // 3. Total Commander
        if config.enable_totalcmd {
            for f in get_totalcmd_folders() {
                let norm = normalize_path(&f.path);
                if seen.insert(norm.to_lowercase()) {
                    list.push(FolderCandidate {
                        path: norm,
                        title: f.title,
                        source: "Total Commander".to_string(),
                        is_active: f.is_active,
                    });
                }
            }
        }

        // 4. XYplorer
        if config.enable_xyplorer {
            for f in get_xyplorer_folders() {
                let norm = normalize_path(&f.path);
                if seen.insert(norm.to_lowercase()) {
                    list.push(FolderCandidate {
                        path: norm,
                        title: f.title,
                        source: "XYplorer".to_string(),
                        is_active: f.is_active,
                    });
                }
            }
        }

        // 5. 最近历史记录 (History)
        for h in &config.history_folders {
            let norm = normalize_path(h);
            if Path::new(&norm).is_dir() && seen.insert(norm.to_lowercase()) {
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
            if Path::new(&norm).is_dir() && seen.insert(norm.to_lowercase()) {
                list.push(FolderCandidate {
                    path: norm.clone(),
                    title: get_folder_name(&norm),
                    source: "常用固定".to_string(),
                    is_active: false,
                });
            }
        }

        // 7. 如果列表仍完全为空，补齐系统常用目录（下载、桌面、文档），确保绝不为空！
        if list.is_empty() {
            if let Some(user_dirs) = directories::UserDirs::new() {
                if let Some(down) = user_dirs.download_dir() {
                    let norm = normalize_path(&down.to_string_lossy());
                    if seen.insert(norm.to_lowercase()) {
                        list.push(FolderCandidate {
                            path: norm,
                            title: crate::rules::I18n::dir_download(config.language).to_string(),
                            source: "系统目录".to_string(),
                            is_active: false,
                        });
                    }
                }
                let desk = user_dirs.desktop_dir().map(|d| d.to_string_lossy().to_string());
                if let Some(desk) = desk {
                    let norm = normalize_path(&desk);
                    if seen.insert(norm.to_lowercase()) {
                        list.push(FolderCandidate {
                            path: norm,
                            title: crate::rules::I18n::dir_desktop(config.language).to_string(),
                            source: "系统目录".to_string(),
                            is_active: false,
                        });
                    }
                }
                let doc = user_dirs.document_dir().map(|d| d.to_string_lossy().to_string());
                if let Some(doc) = doc {
                    let norm = normalize_path(&doc);
                    if seen.insert(norm.to_lowercase()) {
                        list.push(FolderCandidate {
                            path: norm,
                            title: crate::rules::I18n::dir_documents(config.language).to_string(),
                            source: "系统目录".to_string(),
                            is_active: false,
                        });
                    }
                }
            }
        }

        // 排序：活动项最前
        list.sort_by(|a, b| b.is_active.cmp(&a.is_active));

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

fn get_current_active_folder(config: &AppConfig) -> Option<String> {
    // 遍历顺序：DOpus -> TotalCmd -> XYplorer -> 资源管理器
    if config.enable_dopus {
        for f in get_dopus_folders() {
            if f.is_active && Path::new(&f.path).is_dir() {
                return Some(normalize_path(&f.path));
            }
        }
    }

    if config.enable_totalcmd {
        for f in get_totalcmd_folders() {
            if f.is_active && Path::new(&f.path).is_dir() {
                return Some(normalize_path(&f.path));
            }
        }
    }

    if config.enable_xyplorer {
        for f in get_xyplorer_folders() {
            if f.is_active && Path::new(&f.path).is_dir() {
                return Some(normalize_path(&f.path));
            }
        }
    }

    if config.enable_win11_tabs {
        let expls = get_explorer_folders();
        // 1. 查找明确标记为 active 的
        for f in &expls {
            if f.is_active && Path::new(&f.path).is_dir() {
                return Some(normalize_path(&f.path));
            }
        }
        // 2. 否则直接取第一个打开的资源管理器
        if let Some(first) = expls.first() {
            if Path::new(&first.path).is_dir() {
                return Some(normalize_path(&first.path));
            }
        }
    }

    None
}

fn normalize_path(path: &str) -> String {
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
