#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "zh-CN")]
    ZhCN,
    #[serde(rename = "en-US")]
    EnUS,
}

impl Default for Language {
    fn default() -> Self {
        Language::Auto
    }
}

pub struct I18n;

impl I18n {
    /// 解析实际生效的语言（若为 Auto 则根据系统语言检测）
    pub fn resolve_locale(lang: Language) -> Language {
        match lang {
            Language::Auto => {
                let lcid = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
                // 0x0804: zh-CN (PRC), 0x0404: zh-TW, 0x0C04: zh-HK, 0x1004: zh-SG
                let primary_lang = lcid & 0x03FF;
                if primary_lang == 0x0004 {
                    Language::ZhCN
                } else {
                    Language::EnUS
                }
            }
            Language::ZhCN => Language::ZhCN,
            Language::EnUS => Language::EnUS,
        }
    }

    pub fn floating_title(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "QuickPath 路径跳转 (点击即达 · ↑↓ 键选择，Enter 确认)",
            _ => "QuickPath Jump (Click to Go · ↑↓ Select, Enter Confirm)",
        }
    }

    pub fn tag_explorer(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "资源管理器",
            _ => "Explorer",
        }
    }

    pub fn tag_history(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "最近历史",
            _ => "History",
        }
    }

    pub fn tag_pinned(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "常用固定",
            _ => "Pinned",
        }
    }

    pub fn tag_system(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "系统目录",
            _ => "System",
        }
    }

    pub fn dir_download(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "下载",
            _ => "Downloads",
        }
    }

    pub fn dir_desktop(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "桌面",
            _ => "Desktop",
        }
    }

    pub fn dir_documents(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "文档",
            _ => "Documents",
        }
    }

    pub fn tray_toggle_autoswitch(lang: Language, enabled: bool) -> String {
        let locale = Self::resolve_locale(lang);
        let check = if enabled { "✔ " } else { "    " };
        match locale {
            Language::ZhCN => format!("{}自动秒切 (AutoSwitch)", check),
            _ => format!("{}Auto-Switch (AutoSwitch)", check),
        }
    }

    pub fn tray_toggle_autostart(lang: Language, enabled: bool) -> String {
        let locale = Self::resolve_locale(lang);
        let check = if enabled { "✔ " } else { "    " };
        match locale {
            Language::ZhCN => format!("{}开机自启", check),
            _ => format!("{}Start on Boot", check),
        }
    }

    pub fn tray_show_candidates(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "呼出候选目录 (Ctrl + Q)",
            _ => "Show Candidates (Ctrl + Q)",
        }
    }

    pub fn tray_settings(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "设置中心...",
            _ => "Settings...",
        }
    }

    pub fn tray_exit(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "退出 QuickPath",
            _ => "Exit QuickPath",
        }
    }

    pub fn settings_title(lang: Language) -> &'static str {
        match Self::resolve_locale(lang) {
            Language::ZhCN => "QuickPath 设置中心",
            _ => "QuickPath Settings",
        }
    }
}
