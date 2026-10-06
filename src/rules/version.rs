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
