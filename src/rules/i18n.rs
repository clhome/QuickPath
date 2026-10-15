#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    #[serde(rename = "auto")]
    #[default]
    Auto,
    #[serde(rename = "zh-CN")]
    ZhCN,
    #[serde(rename = "en-US")]
    EnUS,
    #[serde(untagged)]
    Custom(String),
}

impl Language {
    pub fn as_code(&self) -> &str {
        match self {
            Language::Auto => "auto",
            Language::ZhCN => "zh-CN",
            Language::EnUS => "en-US",
            Language::Custom(s) => s.as_str(),
        }
    }

    pub fn from_code(code: &str) -> Self {
        match code {
            "auto" => Language::Auto,
            "zh-CN" => Language::ZhCN,
            "en-US" => Language::EnUS,
            other => Language::Custom(other.to_string()),
        }
    }

    /// 动态轮转到下一个可用语言（支持用户外部扩展语言）
    pub fn next(&self) -> Language {
        let avail = I18n::get_available_languages();
        let mut codes = vec!["auto".to_string()];
        for (code, _) in avail {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        let current = self.as_code();
        let idx = codes.iter().position(|c| c == current).unwrap_or(0);
        let next_idx = (idx + 1) % codes.len();
        Language::from_code(&codes[next_idx])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaSection {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingBarSection {
    pub title: String,
    #[serde(default)]
    pub producer: String,
    pub tag_explorer: String,
    pub tag_history: String,
    pub tag_pinned: String,
    #[serde(default)]
    pub tag_system: String,
    #[serde(default)]
    pub no_candidate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeySection {
    pub card_title: String,
    pub card_desc: String,
    pub recording_prompt: String,
    #[serde(default)]
    pub click_to_record: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsSection {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    pub auto_switch: String,
    pub auto_switch_desc: String,
    pub autostart: String,
    pub autostart_desc: String,
    pub opacity: String,
    pub opacity_desc: String,
    pub language: String,
    pub language_desc: String,
    #[serde(default)]
    pub ecosystem: String,
    #[serde(default)]
    pub ecosystem_desc: String,
    pub version_info: String,
    pub producer: String,
    pub btn_ok: String,
    pub btn_cancel: String,
    #[serde(default)]
    pub status_enabled: String,
    #[serde(default)]
    pub status_disabled: String,
    #[serde(default)]
    pub status_all_ready: String,
    #[serde(default)]
    pub lang_auto: String,
    #[serde(default)]
    pub debug_mode: String,
    #[serde(default)]
    pub debug_mode_desc: String,
    #[serde(default)]
    pub log_title: String,
    #[serde(default)]
    pub log_clear: String,
    #[serde(default)]
    pub log_cleared: String,
    #[serde(default)]
    pub log_clear_failed: String,
    #[serde(default)]
    pub log_none: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AboutSection {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub copyright: String,
    #[serde(default)]
    pub website: String,
    #[serde(default)]
    pub github: String,
    #[serde(default)]
    pub os_environment: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraySection {
    pub show_candidates: String,
    pub toggle_autoswitch: String,
    pub toggle_autostart: String,
    #[serde(default)]
    pub toggle_monitor: String,
    pub settings: String,
    pub exit: String,
    /// 审计报告 U-01：显式获取第三方管理器当前路径（单次、前台、用户触发）
    #[serde(default)]
    pub probe_totalcmd: String,
    #[serde(default)]
    pub probe_xyplorer: String,
    /// 审计报告 D-01A：探测结束后的**可见反馈**（成功 / 未取得路径）
    #[serde(default)]
    pub probe_result_ok: String,
    #[serde(default)]
    pub probe_result_failed: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorSection {
    #[serde(default)]
    pub tab_general: String,
    #[serde(default)]
    pub tab_monitor: String,
    #[serde(default)]
    pub master_switch: String,
    #[serde(default)]
    pub master_switch_desc: String,
    #[serde(default)]
    pub metrics_title: String,
    #[serde(default)]
    pub metrics_desc: String,
    #[serde(default)]
    pub metric_net_label: String,
    #[serde(default)]
    pub metric_cpu_label: String,
    #[serde(default)]
    pub metric_mem_label: String,
    #[serde(default)]
    pub metric_gpu_label: String,
    #[serde(default)]
    pub metric_disk_label: String,
    #[serde(default)]
    pub visual_title: String,
    #[serde(default)]
    pub visual_desc: String,
    #[serde(default)]
    pub graph_bg_label: String,
    #[serde(default)]
    pub interval_label: String,
    #[serde(default)]
    pub interval_1s: String,
    #[serde(default)]
    pub interval_3s: String,
    #[serde(default)]
    pub interval_5s: String,
    #[serde(default)]
    pub opacity_label: String,
    #[serde(default)]
    pub opacity_desc: String,
    #[serde(default)]
    pub menu_settings: String,
    #[serde(default)]
    pub menu_metrics: String,
    #[serde(default)]
    pub menu_metric_net: String,
    #[serde(default)]
    pub menu_metric_cpu: String,
    #[serde(default)]
    pub menu_metric_mem: String,
    #[serde(default)]
    pub menu_metric_gpu: String,
    #[serde(default)]
    pub menu_metric_disk: String,
    #[serde(default)]
    pub menu_graph_bg: String,
    #[serde(default)]
    pub menu_refresh_net: String,
    #[serde(default)]
    pub menu_hide: String,
    #[serde(default)]
    pub menu_exit: String,
    #[serde(default)]
    pub menu_position: String,
    #[serde(default)]
    pub menu_pos_tray_left: String,
    #[serde(default)]
    pub menu_pos_taskbar_left: String,
    #[serde(default)]
    pub position_label: String,
    #[serde(default)]
    pub tips_title: String,
    #[serde(default)]
    pub tips_line1: String,
    #[serde(default)]
    pub tips_line2: String,
    #[serde(default)]
    pub tooltip_title: String,
    #[serde(default)]
    pub tooltip_net_speed: String,
    #[serde(default)]
    pub tooltip_net_adapter: String,
    #[serde(default)]
    pub tooltip_cpu: String,
    #[serde(default)]
    pub tooltip_memory: String,
    #[serde(default)]
    pub tooltip_gpu: String,
    #[serde(default)]
    pub tooltip_disk: String,
    #[serde(default)]
    pub tooltip_uptime: String,
    #[serde(default)]
    pub tooltip_power: String,
    #[serde(default)]
    pub tooltip_power_ac: String,
    #[serde(default)]
    pub tooltip_power_battery: String,
    #[serde(default)]
    pub tooltip_vram: String,
    #[serde(default)]
    pub tooltip_used: String,
    #[serde(default)]
    pub tooltip_total: String,
    #[serde(default)]
    pub tooltip_read: String,
    #[serde(default)]
    pub tooltip_write: String,
    #[serde(default)]
    pub tooltip_top: String,
    #[serde(default)]
    pub tooltip_session_total: String,
    #[serde(default)]
    pub tooltip_days: String,
    #[serde(default)]
    pub tooltip_hours: String,
    #[serde(default)]
    pub tooltip_minutes: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TabModuleSection {
    #[serde(default)]
    pub card_title: String,
    #[serde(default)]
    pub card_desc: String,
    #[serde(default)]
    pub enable_tab_module: String,
    #[serde(default)]
    pub enable_tab_module_desc: String,
    #[serde(default)]
    pub shortcuts_group_title: String,
    #[serde(default)]
    pub shortcuts_group_desc: String,
    #[serde(default)]
    pub wine_to_tab: String,
    #[serde(default)]
    pub wine_to_tab_desc: String,
    #[serde(default)]
    pub behaviors_group_title: String,
    #[serde(default)]
    pub behaviors_group_desc: String,
    #[serde(default)]
    pub tab_deduplication: String,
    #[serde(default)]
    pub tab_deduplication_desc: String,
    #[serde(default)]
    pub double_click_duplicate: String,
    #[serde(default)]
    pub double_click_duplicate_desc: String,
    #[serde(default)]
    pub tips_title: String,
    #[serde(default)]
    pub tips_line1: String,
    #[serde(default)]
    pub tips_line2: String,
    #[serde(default)]
    pub unsupported_badge: String,
    #[serde(default)]
    pub unsupported_title: String,
    #[serde(default)]
    pub unsupported_desc: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntegrationSection {
    #[serde(default)]
    pub tab_title: String,
    #[serde(default)]
    pub totalcmd_switch: String,
    #[serde(default)]
    pub totalcmd_desc: String,
    #[serde(default)]
    pub xyplorer_switch: String,
    #[serde(default)]
    pub xyplorer_desc: String,
    #[serde(default)]
    pub tips_title: String,
    #[serde(default)]
    pub tips_line1: String,
    #[serde(default)]
    pub tips_line2: String,
    #[serde(default)]
    pub tips_line3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocaleBundle {
    pub meta: MetaSection,
    pub floating_bar: FloatingBarSection,
    pub hotkey: HotkeySection,
    pub settings: SettingsSection,
    #[serde(default)]
    pub about: AboutSection,
    pub tray: TraySection,
    #[serde(default)]
    pub monitor: MonitorSection,
    #[serde(default)]
    pub tab_module: TabModuleSection,
    /// 审计报告 U-01：第三方管理器联动页
    #[serde(default)]
    pub integration: IntegrationSection,
}

// 编译期嵌入官方自带语言文件，确保单文件绿色分发零外部依赖
const EMBEDDED_ZH_CN: &str = include_str!("../../locales/zh-CN.toml");
const EMBEDDED_EN_US: &str = include_str!("../../locales/en-US.toml");

static LOCALES: OnceLock<HashMap<String, LocaleBundle>> = OnceLock::new();

/// 将语言模板文件中的 {version} 动态注入为全局统一版本号
fn parse_bundle_with_version(raw: &str) -> Option<LocaleBundle> {
    let injected = raw.replace("{version}", crate::rules::version::APP_VERSION);
    toml::from_str::<LocaleBundle>(&injected).ok()
}

pub struct I18n;

/// 管理器联动页（审计报告 U-01）的解析后文案：语言包缺失时回退到内置默认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationTexts {
    pub tab_title: String,
    pub totalcmd_switch: String,
    pub totalcmd_desc: String,
    pub xyplorer_switch: String,
    pub xyplorer_desc: String,
    pub tips_title: String,
    pub tips_line1: String,
    pub tips_line2: String,
    pub tips_line3: String,
}

impl I18n {
    /// 获取全局语言包注册表（内置 + 外部动态扩展覆盖，并自动注入全局统一版本号）
    pub fn get_bundles() -> &'static HashMap<String, LocaleBundle> {
        LOCALES.get_or_init(|| {
            let mut map = HashMap::new();

            // 1. 加载编译期内置官方语言（动态注入统一版本号）
            if let Some(zh) = parse_bundle_with_version(EMBEDDED_ZH_CN) {
                map.insert("zh-CN".to_string(), zh);
            }
            if let Some(en) = parse_bundle_with_version(EMBEDDED_EN_US) {
                map.insert("en-US".to_string(), en);
            }

            // 2. 动态扫描外部覆盖与扩展语言文件
            let candidate_dirs = vec![
                PathBuf::from("locales"),
                PathBuf::from("./locales"),
                directories::ProjectDirs::from("com", "yufeng", "QuickPath")
                    .map(|p| p.config_dir().join("locales"))
                    .unwrap_or_else(|| PathBuf::from("locales")),
            ];

            for dir in candidate_dirs {
                if dir.is_dir()
                    && let Ok(entries) = fs::read_dir(dir)
                {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("toml")
                            && let Ok(content) = fs::read_to_string(&path)
                            && let Some(bundle) = parse_bundle_with_version(&content)
                        {
                            map.insert(bundle.meta.code.clone(), bundle);
                        }
                    }
                }
            }

            map
        })
    }

    /// 获取所有可用语言列表 (代码, 显示名称)，支持动态扩展
    pub fn get_available_languages() -> Vec<(String, String)> {
        let bundles = Self::get_bundles();
        let mut list = Vec::new();
        if let Some(zh) = bundles.get("zh-CN") {
            list.push((zh.meta.code.clone(), zh.meta.name.clone()));
        }
        if let Some(en) = bundles.get("en-US") {
            list.push((en.meta.code.clone(), en.meta.name.clone()));
        }
        for (code, bundle) in bundles {
            if code != "zh-CN" && code != "en-US" {
                list.push((code.clone(), bundle.meta.name.clone()));
            }
        }
        list
    }

    /// 解析实际生效的语言代码（若为 Auto 则根据系统语言检测）
    pub fn resolve_locale_code(lang: &Language) -> String {
        match lang {
            Language::Auto => {
                let lcid = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
                let primary_lang = lcid & 0x03FF;
                if primary_lang == 0x0004 {
                    "zh-CN".to_string()
                } else {
                    "en-US".to_string()
                }
            }
            Language::ZhCN => "zh-CN".to_string(),
            Language::EnUS => "en-US".to_string(),
            Language::Custom(code) => code.clone(),
        }
    }

    /// 获取对应语言包，若不存在则回退至内置英文或中文
    pub fn get_bundle(lang: &Language) -> &'static LocaleBundle {
        let bundles = Self::get_bundles();
        let code = Self::resolve_locale_code(lang);
        bundles
            .get(&code)
            .or_else(|| bundles.get("en-US"))
            .or_else(|| bundles.get("zh-CN"))
            .expect("内置语言包不可用")
    }

    // ==========================================
    // 高层兼容 API 保持原样，直接读取 TOML 配置
    // ==========================================

    pub fn floating_title(lang: &Language) -> &str {
        &Self::get_bundle(lang).floating_bar.title
    }

    pub fn floating_producer(lang: &Language) -> &str {
        let b = Self::get_bundle(lang);
        if !b.floating_bar.producer.is_empty() {
            &b.floating_bar.producer
        } else {
            &b.settings.producer
        }
    }

    pub fn tag_explorer(lang: &Language) -> &str {
        &Self::get_bundle(lang).floating_bar.tag_explorer
    }

    pub fn tag_history(lang: &Language) -> &str {
        &Self::get_bundle(lang).floating_bar.tag_history
    }

    pub fn tag_pinned(lang: &Language) -> &str {
        &Self::get_bundle(lang).floating_bar.tag_pinned
    }

    pub fn tag_system(lang: &Language) -> &str {
        let b = Self::get_bundle(lang);
        if b.floating_bar.tag_system.is_empty() {
            "System"
        } else {
            &b.floating_bar.tag_system
        }
    }

    pub fn dir_download(lang: &Language) -> &str {
        match Self::resolve_locale_code(lang).as_str() {
            "zh-CN" => "下载",
            _ => "Downloads",
        }
    }

    pub fn dir_desktop(lang: &Language) -> &str {
        match Self::resolve_locale_code(lang).as_str() {
            "zh-CN" => "桌面",
            _ => "Desktop",
        }
    }

    pub fn dir_documents(lang: &Language) -> &str {
        match Self::resolve_locale_code(lang).as_str() {
            "zh-CN" => "文档",
            _ => "Documents",
        }
    }

    pub fn tray_toggle_autoswitch(lang: &Language, enabled: bool) -> String {
        let check = if enabled { "✔ " } else { "    " };
        format!("{}{}", check, Self::get_bundle(lang).tray.toggle_autoswitch)
    }

    pub fn tray_toggle_autostart(lang: &Language, enabled: bool) -> String {
        let check = if enabled { "✔ " } else { "    " };
        format!("{}{}", check, Self::get_bundle(lang).tray.toggle_autostart)
    }

    pub fn tray_toggle_monitor(lang: &Language, enabled: bool) -> String {
        let check = if enabled { "✔ " } else { "    " };
        let text = &Self::get_bundle(lang).tray.toggle_monitor;
        let title = if text.is_empty() {
            "任务栏状态监控"
        } else {
            text.as_str()
        };
        format!("{}{}", check, title)
    }

    pub fn tray_show_candidates(lang: &Language, hotkey_display: &str) -> String {
        format!(
            "{}({})",
            Self::get_bundle(lang).tray.show_candidates,
            hotkey_display
        )
    }

    pub fn tray_settings(lang: &Language) -> &str {
        &Self::get_bundle(lang).tray.settings
    }

    /// 托盘菜单：显式获取第三方管理器当前路径的条目文案。
    /// `index = 0` 为 Total Commander，其余为 XYplorer（审计报告 U-01）。
    pub fn tray_probe(lang: &Language, index: usize) -> &str {
        let tray = &Self::get_bundle(lang).tray;
        let (value, fallback) = if index == 0 {
            (&tray.probe_totalcmd, "从 TC 获取当前路径")
        } else {
            (&tray.probe_xyplorer, "从 XYplorer 获取当前路径")
        };
        if value.is_empty() { fallback } else { value }
    }

    pub fn tray_exit(lang: &Language) -> &str {
        &Self::get_bundle(lang).tray.exit
    }

    /// 托盘气泡：显式探测结束后的可见反馈文案（审计报告 D-01A）。
    ///
    /// `success = true` 为「已取得路径」，否则为「未取得路径（已取消 / 超时）」。
    /// 文案刻意简短，且**不包含**任何剪贴板正文。
    pub fn tray_probe_result(lang: &Language, success: bool) -> &str {
        let tray = &Self::get_bundle(lang).tray;
        let (value, fallback) = if success {
            (&tray.probe_result_ok, "已获取管理器当前路径")
        } else {
            (&tray.probe_result_failed, "未取得路径（已取消或超时）")
        };
        if value.is_empty() { fallback } else { value }
    }

    /// 管理器联动页的文案（审计报告 U-01），缺失时回退到内置默认。
    pub fn integration_texts(lang: &Language) -> IntegrationTexts {
        let section = &Self::get_bundle(lang).integration;
        let zh = Self::resolve_locale_code(lang) == "zh-CN";
        let pick = |value: &str, zh_default: &'static str, en_default: &'static str| -> String {
            if !value.is_empty() {
                value.to_string()
            } else if zh {
                zh_default.to_string()
            } else {
                en_default.to_string()
            }
        };
        IntegrationTexts {
            tab_title: pick(&section.tab_title, "管理器联动", "Managers"),
            totalcmd_switch: pick(
                &section.totalcmd_switch,
                "实验性：Total Commander 路径探测",
                "Experimental: Total Commander probe",
            ),
            totalcmd_desc: pick(
                &section.totalcmd_desc,
                "手动触发一次；来源无法证明时取消，不覆盖当前内容",
                "Manual, one-shot; cancels if the source can't be proven",
            ),
            xyplorer_switch: pick(
                &section.xyplorer_switch,
                "实验性：XYplorer 路径探测",
                "Experimental: XYplorer probe",
            ),
            xyplorer_desc: pick(
                &section.xyplorer_desc,
                "手动触发一次；来源无法证明时取消，不覆盖当前内容",
                "Manual, one-shot; cancels if the source can't be proven",
            ),
            tips_title: pick(
                &section.tips_title,
                "💡 使用前请先了解",
                "💡 Before enabling",
            ),
            tips_line1: pick(
                &section.tips_line1,
                "• 开关只表示“已授权”，不会自行探测",
                "• The switch only grants permission; it never probes",
            ),
            tips_line2: pick(
                &section.tips_line2,
                "• 每次都要手动点托盘/吸附条上的「TC」或「XY」",
                "• Click “TC” / “XY” in the tray or floating bar to fetch once",
            ),
            tips_line3: pick(
                &section.tips_line3,
                "• 探测会短暂占用剪贴板；来源不可证明时不覆盖当前内容，但管理器可能回写路径",
                "• A probe briefly uses the clipboard; on doubt no old snapshot is restored, but the manager may still write its path",
            ),
        }
    }

    pub fn settings_title(lang: &Language) -> &str {
        &Self::get_bundle(lang).settings.title
    }

    pub fn producer(lang: &Language) -> &str {
        &Self::get_bundle(lang).settings.producer
    }

    pub fn btn_ok(lang: &Language) -> &str {
        &Self::get_bundle(lang).settings.btn_ok
    }

    pub fn btn_cancel(lang: &Language) -> &str {
        &Self::get_bundle(lang).settings.btn_cancel
    }

    pub fn hotkey_card_title(lang: &Language) -> &str {
        &Self::get_bundle(lang).hotkey.card_title
    }

    pub fn hotkey_card_desc(lang: &Language) -> &str {
        &Self::get_bundle(lang).hotkey.card_desc
    }

    pub fn hotkey_recording_prompt(lang: &Language) -> &str {
        &Self::get_bundle(lang).hotkey.recording_prompt
    }

    pub fn about_website(lang: &Language) -> &str {
        let b = Self::get_bundle(lang);
        if !b.about.website.is_empty() {
            &b.about.website
        } else {
            "https://qp.yftec.top"
        }
    }

    pub fn about_github(lang: &Language) -> &str {
        let b = Self::get_bundle(lang);
        if !b.about.github.is_empty() {
            &b.about.github
        } else {
            "https://github.com/clhome/QuickPath"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_floating_producer_i18n() {
        let zh_prod = I18n::floating_producer(&Language::ZhCN);
        assert_eq!(zh_prod, "衢州御风科技有限公司出品");

        let en_prod = I18n::floating_producer(&Language::EnUS);
        assert_eq!(en_prod, "Produced by Quzhou Yufeng Technology Co., Ltd.");
    }

    #[test]
    fn test_dynamic_version_injection() {
        let bundle_zh = I18n::get_bundle(&Language::ZhCN);
        assert!(
            bundle_zh
                .about
                .version
                .contains(crate::rules::version::APP_VERSION)
        );
        assert!(
            bundle_zh
                .settings
                .version_info
                .contains(crate::rules::version::APP_VERSION)
        );

        let bundle_en = I18n::get_bundle(&Language::EnUS);
        assert!(
            bundle_en
                .about
                .version
                .contains(crate::rules::version::APP_VERSION)
        );
        assert!(
            bundle_en
                .settings
                .version_info
                .contains(crate::rules::version::APP_VERSION)
        );
    }

    #[test]
    fn test_monitor_tips_and_abbrev_labels_i18n() {
        let zh = I18n::get_bundle(&Language::ZhCN);
        assert_eq!(zh.monitor.tips_title, "💡 高效交互与智能特性指南");
        assert!(zh.monitor.tips_line1.contains("双击直接调出系统任务管理器"));
        assert_eq!(zh.monitor.interval_3s, "3.0秒 (推荐)");
        assert_eq!(zh.monitor.menu_pos_taskbar_left, "左上方 (独立悬浮)");
        assert_eq!(zh.monitor.menu_pos_tray_left, "任务栏右侧 (默认)");

        let en = I18n::get_bundle(&Language::EnUS);
        assert_eq!(en.monitor.tips_title, "💡 Tips & Smart Features");
        assert!(
            en.monitor
                .tips_line1
                .contains("Double-click to open Task Manager")
        );
        assert_eq!(en.monitor.interval_3s, "3.0s (Rec.)");
        assert_eq!(en.monitor.menu_pos_taskbar_left, "Top-Left (Float)");
        assert_eq!(en.monitor.menu_pos_tray_left, "Tray Right (Default)");
    }
}
