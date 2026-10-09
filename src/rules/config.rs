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
    /// 任务栏硬件状态监控模块配置
    #[serde(default)]
    pub monitor: MonitorConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MonitorPosition {
    /// 任务栏右侧（紧靠系统托盘通知区左边缘，默认）
    #[default]
    TrayLeft,
    /// 任务栏左上方（悬浮于任务栏左侧上边缘上方，与底部应用程序标签栏目完全物理隔离，永不重叠）
    TaskbarLeft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorConfig {
    /// 任务栏状态监控总开关（默认 true，开启）
    pub enabled: bool,

    /// 任务栏停靠位置模式（默认 TrayLeft）
    #[serde(default)]
    pub position: MonitorPosition,

    // --- 细粒度常驻指标勾选项 ---
    /// 是否显示网络吞吐速率（上行与下行合并为一个设置项，默认 true）
    pub show_network: bool,
    /// 是否显示处理器利用率 (CPU，默认 true)
    pub show_cpu: bool,
    /// 是否显示物理内存利用率 (RAM，默认 true)
    pub show_memory: bool,
    /// 是否显示显卡利用率 (GPU，默认 true)
    pub show_gpu: bool,
    /// 是否显示磁盘利用率 (Disk，默认 true)
    pub show_disk: bool,

    // --- 外观与采样高级设置 ---
    /// 是否显示历史折线波动背景图 (15~20秒采样，默认 true)
    pub show_graph_bg: bool,
    /// 采样刷新间隔（毫秒，默认 1000ms，可选 1000 / 1500 / 2000ms）
    pub refresh_interval_ms: u64,
    /// 界面半透明度（50 ~ 100，默认 85）
    pub opacity: u8,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position: MonitorPosition::TrayLeft,
            show_network: true,
            show_cpu: true,
            show_memory: true,
            show_gpu: true,
            show_disk: true,
            show_graph_bg: true,
            refresh_interval_ms: 3000,
            opacity: 95,
        }
    }
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
            monitor: MonitorConfig::default(),
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

                    // 自愈修复 4：监控面板半透明度规范化（50% ~ 100%）与刷新率规范化
                    if config.monitor.opacity < 50 || config.monitor.opacity > 100 {
                        config.monitor.opacity = 85;
                        changed = true;
                    }
                    if config.monitor.refresh_interval_ms < 500 || config.monitor.refresh_interval_ms > 5000 {
                        config.monitor.refresh_interval_ms = 3000;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monitor_position_default() {
        let default_cfg = MonitorConfig::default();
        assert_eq!(default_cfg.position, MonitorPosition::TrayLeft);
        assert_eq!(default_cfg.refresh_interval_ms, 3000);
    }

    #[test]
    fn test_monitor_config_backward_compatibility() {
        // 模拟没有 position 字段的旧版本 JSON
        let old_json = r#"{
            "enabled": true,
            "show_network": true,
            "show_cpu": true,
            "show_memory": true,
            "show_gpu": true,
            "show_disk": true,
            "show_graph_bg": true,
            "refresh_interval_ms": 1000,
            "opacity": 85
        }"#;
        let parsed: MonitorConfig = serde_json::from_str(old_json).expect("解析旧配置必须成功");
        assert_eq!(parsed.position, MonitorPosition::TrayLeft);

        // 测试包含 TaskbarLeft 的新配置序列化与反序列化
        let mut new_cfg = MonitorConfig::default();
        new_cfg.position = MonitorPosition::TaskbarLeft;
        let serialized = serde_json::to_string(&new_cfg).expect("序列化成功");
        let deserialized: MonitorConfig = serde_json::from_str(&serialized).expect("反序列化成功");
        assert_eq!(deserialized.position, MonitorPosition::TaskbarLeft);
    }
}
