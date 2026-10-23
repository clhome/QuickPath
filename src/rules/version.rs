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

    /// D-01 守卫：README 必须挂载官方动态 GitHub Release 自动抓取徽章与一致的 edition。
    ///
    /// 背景：采用 Shields.io 官方动态 GitHub Release 抓取徽章，GitHub 发版即秒级自动联动，
    /// 彻底消除 markdown 硬编码死版本与手动修改成本。
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

        const DYNAMIC_RELEASE_BADGE: &str =
            "https://img.shields.io/github/v/release/clhome/QuickPath?color=success&label=Version";
        const RELEASES_TARGET_URL: &str = "https://github.com/clhome/QuickPath/releases/latest";
        let language_badge = format!("Language-Rust%20{}", edition);

        for (name, content) in [("README.md", README), ("README_CN.md", README_CN)] {
            assert!(
                content.contains(DYNAMIC_RELEASE_BADGE),
                "{} 必须挂载官方动态 GitHub Release 徽章: {}",
                name,
                DYNAMIC_RELEASE_BADGE
            );
            assert!(
                content.contains(RELEASES_TARGET_URL),
                "{} 的版本徽章点击必须直达 GitHub 最新发布页: {}",
                name,
                RELEASES_TARGET_URL
            );
            assert!(
                content.contains(&language_badge),
                "{} 的 Rust 徽章必须与 Cargo.toml edition={} 一致",
                name,
                edition
            );
        }
    }

    /// 审计报告 P1-3 守卫：CI 与发布通道必须把 `cargo-audit` 固定到同一版本。
    ///
    /// 背景：`cargo install cargo-audit --locked` 只锁定该工具**自身的依赖图**，
    /// 不锁定工具本体；不写 `--version` 会在不同日期装到不同版本的 cargo-audit，
    /// 使同一提交的门禁结果不可复现。这里在编译期把「必须写 `--version`」
    /// 与「ci.yml / release.yml 两处一致」钉死。
    #[test]
    fn test_cargo_audit_version_is_pinned_and_consistent_across_workflows() {
        const CI: &str = include_str!("../../.github/workflows/ci.yml");
        const RELEASE: &str = include_str!("../../.github/workflows/release.yml");

        fn pinned_version(name: &str, content: &str) -> String {
            let line = content
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("cargo install cargo-audit"))
                .unwrap_or_else(|| panic!("{name} 必须安装 cargo-audit"));
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let idx = tokens
                .iter()
                .position(|token| *token == "--version")
                .unwrap_or_else(|| panic!("{name} 必须用 --version 固定 cargo-audit 版本"));
            tokens
                .get(idx + 1)
                .unwrap_or_else(|| panic!("{name} 的 --version 后必须有具体版本"))
                .to_string()
        }

        let ci_version = pinned_version("ci.yml", CI);
        let release_version = pinned_version("release.yml", RELEASE);
        assert_eq!(
            ci_version, release_version,
            "ci.yml 与 release.yml 的 cargo-audit 版本必须一致，否则会出现「PR 绿、tag 红」"
        );
    }

    /// M-01 守卫（单一版本源自动化）：验证最终编译进二进制的 `OUT_DIR/app.manifest` 清单，
    /// 其 assemblyIdentity 版本号由 `build.rs` 自动根据 `Cargo.toml` 注入四段式版本号。
    ///
    /// 开发者升级版本时只需修改 `Cargo.toml` 一处，源码根目录的 `app.manifest` 作为模板存在，
    /// 无论根目录模板写什么，本测试都保证编译注入后的真实清单严格等于单一版本源。
    #[test]
    fn test_app_manifest_version_tracks_single_version_source() {
        const GENERATED_MANIFEST: &str = include_str!(concat!(env!("OUT_DIR"), "/app.manifest"));
        const TEMPLATE_MANIFEST: &str = include_str!("../../app.manifest");

        // 1. 验证源码根目录模板结构健全，包含 version= 替换锚点
        assert!(
            TEMPLATE_MANIFEST
                .lines()
                .any(|l| l.trim_start().starts_with("version=")),
            "源码根目录 app.manifest 模板必须包含以 version= 开头的替换锚点行"
        );

        // 2. 验证 build.rs 输出到 OUT_DIR 的真实清单版本严格等于 Cargo.toml 的四段式规范化版本
        let mut parts: Vec<&str> = APP_VERSION.split('.').collect();
        while parts.len() < 4 {
            parts.push("0");
        }
        let expected = format!("version=\"{}\"", parts[..4].join("."));

        let actual = GENERATED_MANIFEST
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("version="))
            .unwrap_or("<未找到 version= 行>");

        assert_eq!(
            actual, expected,
            "OUT_DIR 生成的清单版本必须为四段式 {}（跟随 Cargo.toml 的 {}），实际为 {}",
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
