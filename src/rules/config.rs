use crate::rules::i18n::Language;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

fn default_schema_version() -> u32 {
    1
}

// 测试专用：将配置读写重定向到临时路径，避免单元测试污染用户真实配置。
#[cfg(test)]
thread_local! {
    static TEST_CONFIG_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_config_path(path: Option<PathBuf>) {
    TEST_CONFIG_PATH.with(|p| *p.borrow_mut() = path);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// 配置文件结构 Schema 版本
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 已执行的应用内单次迁移标记列表
    #[serde(default)]
    pub applied_migrations: Vec<String>,
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
    /// Total Commander 自动探测开关。
    ///
    /// **默认关闭**（审计需求 `9549648` §2.4 方案 B / §5.1 安全降级条件）：
    /// TC 的路径回传只能依赖“让管理器把路径写进系统剪贴板”，而系统剪贴板无法提供
    /// 可靠的更新来源保证。必须在明确理解“探测期间会短暂占用剪贴板”的前提下显式开启。
    #[serde(default)]
    pub enable_totalcmd: bool,
    /// XYplorer 自动探测开关。默认关闭，理由同上。
    #[serde(default)]
    pub enable_xyplorer: bool,
    /// 外观主题：auto（跟随系统）, dark（深色模式）, light（浅色模式）
    pub theme: String,
    /// 任务栏硬件状态监控模块配置
    pub monitor: MonitorConfig,
    /// 资源管理器多标签页增强模块配置
    #[serde(default)]
    pub tab_module: TabModuleConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabModuleConfig {
    /// 资源管理器多标签页增强总开关
    pub enabled: bool,
    /// Win + E 自动转为新建标签页
    pub wine_to_tab: bool,
    /// 标签页智能去重
    pub deduplication: bool,
    /// 双击标签页自动在右侧复制当前路径标签页 (Duplicate Tab)
    pub double_click_to_duplicate: bool,
}

impl Default for TabModuleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wine_to_tab: true,
            deduplication: true,
            double_click_to_duplicate: true,
        }
    }
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
#[serde(default)]
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
            schema_version: 1,
            applied_migrations: Vec::new(),
            auto_switch_enabled: true,
            auto_switch_delay_ms: 100,
            hotkey: "Ctrl+Q".to_string(),
            floating_bar_enabled: true,
            floating_bar_auto_show: true,
            floating_bar_opacity: 88,
            language: Language::Auto,
            autostart_enabled: false,
            autostart_task_scheduler: false,
            blacklist_processes: Vec::new(),
            whitelist_processes: Vec::new(),
            pinned_folders: Vec::new(),
            history_folders: Vec::new(),
            max_history_count: 15,
            enable_win11_tabs: true,
            enable_dopus: true,
            // 剪贴板回传通道默认关闭：须用户显式开启（见字段文档）
            enable_totalcmd: false,
            enable_xyplorer: false,
            theme: "auto".to_string(),
            monitor: MonitorConfig::default(),
            tab_module: TabModuleConfig::default(),
        }
    }
}

/// Windows 原生原子文件替换封装
#[cfg(windows)]
fn atomic_replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            lpExistingFileName: *const u16,
            lpNewFileName: *const u16,
            dwFlags: u32,
        ) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x00000001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x00000008;

    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();

    let res = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if res != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn atomic_replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

/// 将损坏的配置文件隔离备份为 .bad.<timestamp>
fn isolate_corrupted_config(path: &Path) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bad_path = path.with_extension(format!("bad.{}", timestamp));
    let _ = fs::rename(path, bad_path);
}

impl AppConfig {
    /// 确定配置文件路径（优先当前目录便携版，否则存入 %APPDATA%\QuickPath\config.json）
    pub fn get_config_path() -> PathBuf {
        #[cfg(test)]
        if let Some(p) = TEST_CONFIG_PATH.with(|p| p.borrow().clone()) {
            return p;
        }

        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let portable_config = exe_dir.join("quickpath.json");

        if portable_config.exists() {
            return portable_config;
        }

        // 标准模式：%APPDATA%\QuickPath\config.json
        let standard_dir = directories::BaseDirs::new()
            .map(|b| b.config_dir().join("QuickPath"))
            .unwrap_or_else(|| PathBuf::from("config"));
        let standard_path = standard_dir.join("config.json");

        // 平滑兼容历史版本可能遗留的旧双层路径：%APPDATA%\QuickPath\QuickPath\config\config.json
        if !standard_path.exists()
            && let Some(old_proj) = directories::ProjectDirs::from("com", "QuickPath", "QuickPath")
        {
            let old_path = old_proj.config_dir().join("config.json");
            if old_path.exists() {
                let _ = fs::create_dir_all(&standard_dir);
                let _ = fs::copy(&old_path, &standard_path);
            }
        }

        let _ = fs::create_dir_all(&standard_dir);
        standard_path
    }

    /// 加载配置（具备三级容灾能力与一次性受控迁移）：
    /// 1. 正常解析 config.json；
    /// 2. 失败时尝试从最近一次成功保存的 config.json.bak 恢复；
    /// 3. 若均失败，将损坏文件隔离重命名为 config.json.bad.<timestamp>，绝不静默覆盖！
    pub fn load() -> Self {
        let path = Self::get_config_path();
        let bak_path = path.with_extension("bak");

        // 1. 尝试读取并解析主配置
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path)
                && let Ok(mut config) = serde_json::from_str::<AppConfig>(&content)
            {
                config.apply_migrations_and_sanitize();
                return config;
            }
            eprintln!(
                "警告: 主配置文件 {:?} 格式异常，尝试从 .bak 备份回退...",
                path
            );
        }

        // 2. 尝试从备份配置恢复
        if bak_path.exists()
            && let Ok(bak_content) = fs::read_to_string(&bak_path)
            && let Ok(mut config) = serde_json::from_str::<AppConfig>(&bak_content)
        {
            eprintln!("成功从备份配置文件 {:?} 恢复用户配置！", bak_path);
            isolate_corrupted_config(&path);
            config.apply_migrations_and_sanitize();
            let _ = config.save();
            return config;
        }

        // 3. 容灾隔离并启用默认配置
        if path.exists() {
            isolate_corrupted_config(&path);
        }

        let default_config = Self::default();
        let _ = default_config.save();
        default_config
    }

    /// 受控规整与一次性版本迁移：绝对尊重用户主动设定的配置与黑名单
    fn apply_migrations_and_sanitize(&mut self) {
        let mut changed = false;

        // 一次性迁移示例：仅记录已执行标记，不再无差别强行改写用户的开关与黑名单
        let migration_v1 = "migrated_to_v1_1_3".to_string();
        if !self.applied_migrations.contains(&migration_v1) {
            self.applied_migrations.push(migration_v1);
            changed = true;
        }

        // 一次性安全迁移（审计需求 `9549648` §2.4 方案 B）：
        // 历史版本的 TC / XYplorer 自动探测默认开启，而该探测只能让管理器把路径写进
        // 系统剪贴板 —— 无法证明请求窗口内没有第三方更新。因此把所有存量配置**一次性
        // 置为关闭**，以后必须用户显式开启才生效（迁移标记保证只执行一次，不会每次
        // 启动都覆盖用户的选择）。
        let migration_clipboard_probe = "clipboard_probe_opt_in_1_2_3".to_string();
        if !self.applied_migrations.contains(&migration_clipboard_probe) {
            self.applied_migrations
                .push(migration_clipboard_probe.clone());
            if self.enable_totalcmd {
                self.enable_totalcmd = false;
            }
            if self.enable_xyplorer {
                self.enable_xyplorer = false;
            }
            changed = true;
        }

        // 仅对非法越界数值进行合理防御性校准（防止手动修改配置输入越界）
        if self.floating_bar_opacity < 40 || self.floating_bar_opacity > 100 {
            self.floating_bar_opacity = 88;
            changed = true;
        }
        if self.monitor.opacity < 50 || self.monitor.opacity > 100 {
            self.monitor.opacity = 85;
            changed = true;
        }
        if self.monitor.refresh_interval_ms < 500 || self.monitor.refresh_interval_ms > 5000 {
            self.monitor.refresh_interval_ms = 3000;
            changed = true;
        }

        if changed {
            let _ = self.save();
        }
    }

    /// 保存配置到本地文件（临时文件 + sync_all + 原子替换 + 自动维护 .bak 备份）
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::get_config_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let json = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_extension("tmp");

        // 1. 写入临时文件并执行物理落盘冲刷
        {
            use std::io::Write;
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }

        // 2. 若原文件存在且完好，保留 .bak 备份
        let bak_path = path.with_extension("bak");
        if path.exists() {
            let _ = fs::copy(&path, &bak_path);
        }

        // 3. 原子替换主文件（若原子替换失败，坚决不删除原有主配置文件，保留原配置并向调用方返回错误）
        if let Err(e) = atomic_replace_file(&tmp_path, &path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(e);
        }

        Ok(())
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
    fn test_app_config_missing_fields_defaults() {
        // 模拟缺失大量字段（甚至是新版本新增字段）时的向后兼容性
        let minimal_json = r#"{
            "hotkey": "Alt+Q",
            "floating_bar_auto_show": false
        }"#;

        let parsed: AppConfig =
            serde_json::from_str(minimal_json).expect("必须成功解析缺失字段的旧配置");
        assert_eq!(parsed.hotkey, "Alt+Q");
        assert!(!parsed.floating_bar_auto_show);
        assert!(parsed.auto_switch_enabled); // 自动补齐默认值
        assert_eq!(parsed.schema_version, 1);
    }

    #[test]
    fn test_user_settings_respect() {
        // 关键：测试必须写入隔离的临时路径，绝不允许触碰用户真实配置
        let temp_dir = std::env::temp_dir().join(format!("qp_cfg_respect_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        set_test_config_path(Some(temp_dir.join("config.json")));

        // 确保用户的设置(关闭悬浮吸附栏、自定义浏览器黑名单)不会被自愈逻辑无理改写
        let mut config = AppConfig {
            floating_bar_auto_show: false,
            ..AppConfig::default()
        };
        config.blacklist_processes.push("chrome.exe".to_string());

        config.apply_migrations_and_sanitize();

        assert!(
            !config.floating_bar_auto_show,
            "用户关闭的悬浮栏绝不能被强制开启"
        );
        assert!(
            config
                .blacklist_processes
                .contains(&"chrome.exe".to_string()),
            "用户手动添加的黑名单进程绝不能被删除"
        );

        set_test_config_path(None);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// 审计需求 `9549648` §2.4 方案 B：剪贴板回传探测器（TC / XYplorer）必须**默认关闭**，
    /// 且存量配置必须被一次性置为关闭；执行后不得再反复覆盖用户后续的显式开启。
    #[test]
    fn test_clipboard_probe_defaults_off_and_migrates_once() {
        let default_cfg = AppConfig::default();
        assert!(
            !default_cfg.enable_totalcmd,
            "Total Commander 自动探测必须默认关闭（只能靠系统剪贴板回传）"
        );
        assert!(
            !default_cfg.enable_xyplorer,
            "XYplorer 自动探测必须默认关闭（只能靠系统剪贴板回传）"
        );

        // 存量配置（旧版本默认 true）必须被一次性关掉，并留下迁移标记
        let mut legacy = AppConfig {
            enable_totalcmd: true,
            enable_xyplorer: true,
            ..AppConfig::default()
        };
        legacy.applied_migrations.clear();
        legacy.apply_migrations_and_sanitize();
        assert!(
            !legacy.enable_totalcmd && !legacy.enable_xyplorer,
            "存量配置必须被迁移为关闭"
        );
        assert!(
            legacy
                .applied_migrations
                .iter()
                .any(|m| m == "clipboard_probe_opt_in_1_2_3"),
            "必须留下一次性迁移标记"
        );

        // 迁移只执行一次：用户显式开启后再次规整不得被重新关掉
        legacy.enable_totalcmd = true;
        legacy.enable_xyplorer = true;
        legacy.apply_migrations_and_sanitize();
        assert!(
            legacy.enable_totalcmd && legacy.enable_xyplorer,
            "用户显式开启后不得被每次启动的规整逻辑重新关闭"
        );
    }

    /// 审计报告 U-01 验收标准 2：两个授权开关必须可持久化（设置中心点“确定”后落盘）。
    ///
    /// 迁移标记必须同时存在，否则启动时的安全迁移会（按设计）把它们重新置为关闭。
    #[test]
    fn test_authorization_switches_roundtrip() {
        let temp_dir =
            std::env::temp_dir().join(format!("qp_cfg_authorize_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        set_test_config_path(Some(temp_dir.join("config.json")));

        let mut config = AppConfig {
            enable_totalcmd: true,
            enable_xyplorer: true,
            ..AppConfig::default()
        };
        // 模拟已经执行过一次性安全迁移（真实用户配置在首次升级时已写入该标记）
        config.applied_migrations.clear();
        config.apply_migrations_and_sanitize();
        assert!(!config.enable_totalcmd && !config.enable_xyplorer);
        config.enable_totalcmd = true;
        config.enable_xyplorer = true;
        config.save().expect("隔离路径下保存必须成功");

        let loaded = AppConfig::load();
        assert!(
            loaded.enable_totalcmd && loaded.enable_xyplorer,
            "用户显式授权必须在重启后保留（否则开关等于摆设）"
        );

        set_test_config_path(None);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_config_roundtrip_stays_in_isolated_path() {
        let temp_dir =
            std::env::temp_dir().join(format!("qp_cfg_roundtrip_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let isolated = temp_dir.join("config.json");
        set_test_config_path(Some(isolated.clone()));

        let mut config = AppConfig {
            hotkey: "Alt+Shift+Q".to_string(),
            ..AppConfig::default()
        };
        config.history_folders.push("C:\\Windows".to_string());
        config.save().expect("隔离路径下保存必须成功");

        assert!(isolated.exists(), "配置必须写入隔离路径");
        let loaded = AppConfig::load();
        assert_eq!(loaded.hotkey, "Alt+Shift+Q");
        assert_eq!(loaded.history_folders, vec!["C:\\Windows".to_string()]);

        set_test_config_path(None);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_corrupted_json_isolation_concept() {
        let temp_dir = std::env::temp_dir().join(format!("qp_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let config_path = temp_dir.join("config.json");
        let bak_path = temp_dir.join("config.bak");

        // 写入初始正常配置并保存备份
        let original_cfg = AppConfig::default();
        let valid_json = serde_json::to_string_pretty(&original_cfg).unwrap();
        fs::write(&config_path, &valid_json).unwrap();
        fs::write(&bak_path, &valid_json).unwrap();

        // 模拟损坏的主配置文件（非法 JSON）
        fs::write(&config_path, "{ broken json content").unwrap();

        // 验证主配置损坏时，尝试从 bak_path 恢复成功
        let bak_content = fs::read_to_string(&bak_path).unwrap();
        let recovered: Result<AppConfig, _> = serde_json::from_str(&bak_content);
        assert!(recovered.is_ok(), "必须能从备份配置中恢复正常数据");

        // 清理测试临时目录
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_tab_module_config_defaults() {
        let default_cfg = TabModuleConfig::default();
        assert!(default_cfg.enabled);
        assert!(default_cfg.wine_to_tab);
        assert!(default_cfg.deduplication);
        assert!(default_cfg.double_click_to_duplicate);

        let app_cfg = AppConfig::default();
        assert_eq!(app_cfg.tab_module, default_cfg);

        // 验证旧版 JSON 反序列化时自动补齐 tab_module 默认字段
        let old_json = r#"{"hotkey": "Ctrl+Q"}"#;
        let parsed: AppConfig =
            serde_json::from_str(old_json).expect("必须成功解析缺失 tab_module 的旧配置");
        assert!(parsed.tab_module.enabled);

        // 验证多语言语言包包含 tab_module 标题与新增分组
        let bundle_zh = crate::rules::I18n::get_bundle(&Language::ZhCN);
        assert!(!bundle_zh.tab_module.card_title.is_empty());
        assert!(!bundle_zh.tab_module.enable_tab_module.is_empty());

        let bundle_en = crate::rules::I18n::get_bundle(&Language::EnUS);
        assert!(!bundle_en.tab_module.card_title.is_empty());
        assert!(!bundle_en.tab_module.enable_tab_module.is_empty());
    }
}
