use crate::rules::i18n::Language;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 是否开启自动秒切 (AutoSwitch)
    pub auto_switch_enabled: bool,
    /// 自动秒切触发延时（毫秒，默认 80ms 确保对话框控件完全初始化）
    pub auto_switch_delay_ms: u64,
    /// 呼出悬浮吸附条的快捷键
    pub hotkey: String,
    /// 是否启用悬浮吸附条
    pub floating_bar_enabled: bool,
    /// 对话框打开时是否自动弹出悬浮吸附条
    pub floating_bar_auto_show: bool,
    /// 弹出层半透明程度（百分比，40 ~ 100，默认 88）
    pub floating_bar_opacity: u8,
    /// 界面语言：auto, zh-CN, en-US
    pub language: Language,
    /// 开机自启
    pub autostart_enabled: bool,
    /// 开机自启是否采用任务计划程序免 UAC 提权模式
    pub autostart_task_scheduler: bool,
    /// 黑名单进程列表（不执行自动秒切）
    pub blacklist_processes: Vec<String>,
    /// 白名单进程列表（强制自动秒切，若为空则除黑名单外全部生效）
    pub whitelist_processes: Vec<String>,
    /// 固定/常用目录列表 (Pinned)
    pub pinned_folders: Vec<String>,
    /// 最近使用的历史目录队列 (LRU)
    pub history_folders: Vec<String>,
    /// 最大历史记录数量
    pub max_history_count: usize,
    /// 适配文件管理器开关
    pub enable_win11_tabs: bool,
    pub enable_dopus: bool,
    pub enable_totalcmd: bool,
    pub enable_xyplorer: bool,
    /// 外观主题：auto（跟随系统）, dark（深色模式）, light（浅色模式）
    pub theme: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            auto_switch_enabled: true,
            auto_switch_delay_ms: 100,
            hotkey: "Ctrl+Q".to_string(),
            floating_bar_enabled: true,
            floating_bar_auto_show: true,
            floating_bar_opacity: 88,
            language: Language::Auto,
            autostart_enabled: false,
            autostart_task_scheduler: true,
            blacklist_processes: Vec::new(),
            whitelist_processes: Vec::new(),
            pinned_folders: Vec::new(),
            history_folders: Vec::new(),
            max_history_count: 15,
            enable_win11_tabs: true,
            enable_dopus: true,
            enable_totalcmd: true,
            enable_xyplorer: true,
            theme: "auto".to_string(),
        }
    }
}

impl AppConfig {
    /// 确定配置文件路径（优先当前目录便携版，否则存入 %APPDATA%\QuickPath\config.json）
    pub fn get_config_path() -> PathBuf {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let portable_config = exe_dir.join("quickpath.json");

        if portable_config.exists() {
            return portable_config;
        }

        if let Some(proj_dirs) = directories::ProjectDirs::from("com", "QuickPath", "QuickPath") {
            let config_dir = proj_dirs.config_dir();
            let _ = fs::create_dir_all(config_dir);
            return config_dir.join("config.json");
        }

        portable_config
    }

    /// 加载配置，若不存在则创建默认配置并保存
    pub fn load() -> Self {
        let path = Self::get_config_path();
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(mut config) = serde_json::from_str::<AppConfig>(&content) {
                    let mut changed = false;

                    // 自愈修复 1：确保悬浮吸附栏默认自动展示
                    if !config.floating_bar_auto_show {
                        config.floating_bar_auto_show = true;
                        changed = true;
                    }

                    // 自愈修复 2：清除浏览器等常用进程的黑名单误伤
                    let original_len = config.blacklist_processes.len();
                    config.blacklist_processes.retain(|p| {
                        let lower = p.to_lowercase();
                        lower != "chrome.exe"
                            && lower != "msedge.exe"
                            && lower != "firefox.exe"
                            && lower != "brave.exe"
                    });
                    if config.blacklist_processes.len() != original_len {
                        changed = true;
                    }

                    // 自愈修复 3：半透明度规范化（40% ~ 100%）
                    if config.floating_bar_opacity < 40 || config.floating_bar_opacity > 100 {
                        config.floating_bar_opacity = 88;
                        changed = true;
                    }

                    if changed {
                        let _ = config.save();
                    }

                    return config;
                }
            }
        }

        let default_config = Self::default();
        let _ = default_config.save();
        default_config
    }

    /// 保存配置到本地文件
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::get_config_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)
    }

    /// 判定目标进程是否在黑名单中
    pub fn is_blacklisted(&self, process_name: &str) -> bool {
        let name_lower = process_name.to_lowercase();
        self.blacklist_processes
            .iter()
            .any(|item| item.to_lowercase() == name_lower)
    }

    /// 记录访问路径至历史记录 (LRU 算法)
    pub fn record_history(&mut self, folder_path: &str) {
        let trimmed = folder_path.trim();
        if trimmed.is_empty() || !Path::new(trimmed).is_dir() {
            return;
        }

        // 移除旧有重复项
        self.history_folders.retain(|p| p.ne(trimmed));
        // 插入至最前
        self.history_folders.insert(0, trimmed.to_string());
        // 限制最大历史条数
        if self.history_folders.len() > self.max_history_count {
            self.history_folders.truncate(self.max_history_count);
        }
        let _ = self.save();
    }
}
