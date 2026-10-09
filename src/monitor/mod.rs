#![allow(unused_imports, dead_code)]

pub mod collector;
pub mod metrics;
pub mod window;
pub mod worker;

pub use metrics::MetricsSnapshot;
pub use window::BarWindow;
pub use worker::MetricsWorker;

use crate::rules::{Language, MonitorConfig};

/// 任务栏状态监控模块统一生命周期管理器 (MonitorManager)
pub struct MonitorManager {
    worker: MetricsWorker,
    bar_window: Option<BarWindow>,
    current_config: MonitorConfig,
    current_language: Language,
}

impl MonitorManager {
    pub fn new(config: &MonitorConfig, language: Language) -> Self {
        let worker = MetricsWorker::new(config.refresh_interval_ms);
        let mut manager = Self {
            worker,
            bar_window: None,
            current_config: config.clone(),
            current_language: language,
        };

        if config.enabled {
            manager.start();
        }

        manager
    }

    /// 启动监控 Worker 与任务栏悬浮窗
    pub fn start(&mut self) {
        if self.bar_window.is_some() {
            return;
        }

        self.worker.start();
        let snap_handle = self.worker.get_snapshot_handle();

        if let Ok(bar) = BarWindow::new(
            snap_handle,
            self.current_config.clone(),
            self.current_language.clone(),
        ) {
            self.bar_window = Some(bar);
        }
    }

    /// 停止监控：销毁窗口并安全退出 Worker 线程（零开销休止）
    pub fn stop(&mut self) {
        if let Some(bar) = self.bar_window.take() {
            bar.destroy();
        }
        self.worker.stop();
    }

    /// 同步并热更新配置
    pub fn sync_config(&mut self, new_config: &MonitorConfig) {
        let was_enabled = self.current_config.enabled;
        let is_enabled = new_config.enabled;
        self.current_config = new_config.clone();

        if was_enabled != is_enabled {
            if is_enabled {
                self.start();
            } else {
                self.stop();
            }
            return;
        }

        if is_enabled {
            self.worker.update_interval(new_config.refresh_interval_ms);
            if let Some(bar) = &self.bar_window {
                bar.update_config(new_config.clone());
            } else {
                self.start();
            }
        }
    }

    /// 更新界面多语言
    pub fn update_language(&mut self, lang: Language) {
        self.current_language = lang.clone();
        if let Some(bar) = &self.bar_window {
            bar.update_language(lang);
        }
    }

    /// 系统休眠唤醒事件通知
    pub fn notify_system_resumed(&self) {
        self.worker.notify_system_resumed();
    }
}

impl Drop for MonitorManager {
    fn drop(&mut self) {
        self.stop();
    }
}
