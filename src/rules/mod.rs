pub mod config;
pub mod i18n;
pub mod version;

pub use config::{AppConfig, MonitorConfig, MonitorPosition};
pub use i18n::{I18n, Language, LocaleBundle};
#[allow(unused_imports)]
pub use version::{get_app_version, get_app_version_tag, APP_VERSION};
