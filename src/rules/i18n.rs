#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "zh-CN")]
    ZhCN,
    #[serde(rename = "en-US")]
    EnUS,
    #[serde(untagged)]
    Custom(String),
}

impl Default for Language {
    fn default() -> Self {
        Language::Auto
    }
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraySection {
    pub show_candidates: String,
    pub toggle_autoswitch: String,
    pub toggle_autostart: String,
    pub settings: String,
    pub exit: String,
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
                if dir.is_dir() {
                    if let Ok(entries) = fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                                if let Ok(content) = fs::read_to_string(&path) {
                                    if let Some(bundle) = parse_bundle_with_version(&content) {
                                        map.insert(bundle.meta.code.clone(), bundle);
                                    }
                                }
                            }
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

    pub fn tray_show_candidates(lang: &Language, hotkey_display: &str) -> String {
        format!("{}({})", Self::get_bundle(lang).tray.show_candidates, hotkey_display)
    }

    pub fn tray_settings(lang: &Language) -> &str {
        &Self::get_bundle(lang).tray.settings
    }

    pub fn tray_exit(lang: &Language) -> &str {
        &Self::get_bundle(lang).tray.exit
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
        assert!(bundle_zh.about.version.contains(crate::rules::version::APP_VERSION));
        assert!(bundle_zh.settings.version_info.contains(crate::rules::version::APP_VERSION));

        let bundle_en = I18n::get_bundle(&Language::EnUS);
        assert!(bundle_en.about.version.contains(crate::rules::version::APP_VERSION));
        assert!(bundle_en.settings.version_info.contains(crate::rules::version::APP_VERSION));
    }
}
