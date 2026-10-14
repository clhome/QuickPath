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

    /// M-01 守卫：`app.manifest` 模板的 assemblyIdentity 版本号必须跟随单一版本源，
    /// 且必须是四段式（不足四位补 `0`，与 `build.rs` 写入 OUT_DIR 的规范化结果一致）。
    ///
    /// 背景（审计报告 M-01）：版本升级时 `app.manifest` 模板仍停留在旧值，属同一处漂移。
    /// `build.rs` 只改写 OUT_DIR 里的清单副本、**不改写源码树模板**（审计报告 P2-03），
    /// 所以源码树这份模板必须由守卫测试锁定，否则模板会再次静默落后。
    #[test]
    fn test_app_manifest_version_tracks_single_version_source() {
        const APP_MANIFEST: &str = include_str!("../../app.manifest");

        // 与 build.rs 相同的四段式规范化：不足四段补 "0"，取前四段。
        let mut parts: Vec<&str> = APP_VERSION.split('.').collect();
        while parts.len() < 4 {
            parts.push("0");
        }
        let expected = format!("version=\"{}\"", parts[..4].join("."));

        let actual = APP_MANIFEST
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("version="))
            .unwrap_or("<未找到 version= 行>");

        assert_eq!(
            actual, expected,
            "app.manifest 的版本必须为四段式 {}（跟随 Cargo.toml 的 {}），实际为 {}",
            expected, APP_VERSION, actual
        );
    }

    /// M-01 守卫：`Cargo.lock` 中 `quickpath` 包条目的版本必须等于单一版本源。
    ///
    /// 背景（审计报告 M-01）：`Cargo.toml` 与 `Cargo.lock` 是两处独立落盘的版本号，
    /// 只改 `Cargo.toml` 会让锁文件停留在旧版本。这里把锁文件纳入编译期追踪。
    #[test]
    fn test_cargo_lock_quickpath_version_tracks_single_version_source() {
        const CARGO_LOCK: &str = include_str!("../../Cargo.lock");

        let entry_version = CARGO_LOCK
            .split("[[package]]")
            .find(|block| block.contains("name = \"quickpath\""))
            .and_then(|block| {
                block
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("version = "))
            });

        let expected = format!("\"{}\"", APP_VERSION);

        assert_eq!(
            entry_version,
            Some(expected.as_str()),
            "Cargo.lock 中 quickpath 条目的版本必须是 {}（跟随 Cargo.toml），实际锁文件条目为 {:?}",
            APP_VERSION,
            entry_version
        );
    }
}
