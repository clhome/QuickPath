#![allow(dead_code)]

/// QuickPath 官方版本号单一数据源 (Single Source of Truth)
/// 统一存放于此，修改后全局（包含窗口标题、关于页卡片、页脚、多语言配置）自动联动生效！
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 获取当前标准版本号字符串（例如 "1.0.0"）
pub fn get_app_version() -> &'static str {
    APP_VERSION
}

/// 格式化带 v 前缀的完整版本号（例如 "v1.0.0"）
pub fn get_app_version_tag() -> String {
    format!("v{}", APP_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-01 守卫：README 徽章必须跟随单一版本源（`Cargo.toml`），不得再次漂移。
    ///
    /// 背景（审计报告 D-01）：曾出现 `Cargo.toml` 已是 1.2.1 / edition 2024，
    /// 而两份 README 徽章仍停在 v1.2.0 / Rust 2021。这里在编译期把 README 纳入追踪，
    /// 一旦版本或 edition 变更而 README 未同步，测试直接报红。
    #[test]
    fn test_readme_badges_track_single_version_source() {
        const README: &str = include_str!("../../README.md");
        const README_CN: &str = include_str!("../../README_CN.md");
        const CARGO_TOML: &str = include_str!("../../Cargo.toml");

        let edition = CARGO_TOML
            .lines()
            .find_map(|line| line.trim().strip_prefix("edition"))
            .and_then(|rest| rest.split('"').nth(1))
            .expect("Cargo.toml 必须声明 edition");

        let version_badge = format!("Version-{}", get_app_version_tag());
        let language_badge = format!("Language-Rust%20{}", edition);

        for (name, content) in [("README.md", README), ("README_CN.md", README_CN)] {
            assert!(
                content.contains(&version_badge),
                "{} 的版本徽章必须为 {}（单一版本源）",
                name,
                get_app_version_tag()
            );
            assert!(
                content.contains(&language_badge),
                "{} 的 Rust 徽章必须与 Cargo.toml edition={} 一致",
                name,
                edition
            );
        }
    }
}
