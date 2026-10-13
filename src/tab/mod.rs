#![allow(dead_code)]

pub mod com;
pub mod coordinator;
pub mod hook;
pub mod keyboard;
pub mod mouse;
pub mod select;
pub mod uia;

use std::sync::Arc;
pub use com::ShellComManager;
pub use coordinator::{TabConfig, TabCoordinator};
pub use hook::TabHookGuard;
pub use keyboard::TabKeyboardGuard;
pub use mouse::TabMouseGuard;

/// QuickPath 资源管理器多标签页增强模块核心服务封装
pub struct TabModule {
    com_manager: Arc<ShellComManager>,
    coordinator: Arc<TabCoordinator>,
    hook_guard: Option<TabHookGuard>,
    mouse_guard: Option<TabMouseGuard>,
    keyboard_guard: Option<TabKeyboardGuard>,
}

impl TabModule {
    pub fn new() -> Self {
        let com_manager = Arc::new(ShellComManager::new());
        let coordinator = Arc::new(TabCoordinator::new(com_manager.clone()));

        Self {
            com_manager,
            coordinator,
            hook_guard: None,
            mouse_guard: None,
            keyboard_guard: None,
        }
    }

    /// 启动多标签增强守护
    pub fn start(&mut self) -> Result<(), String> {
        if self.hook_guard.is_none() {
            let guard = TabHookGuard::new(self.coordinator.clone())?;
            self.hook_guard = Some(guard);
        }
        if self.mouse_guard.is_none() {
            let m_guard = TabMouseGuard::new(self.coordinator.clone())?;
            self.mouse_guard = Some(m_guard);
        }
        if self.keyboard_guard.is_none() {
            let k_guard = TabKeyboardGuard::new(self.coordinator.clone())?;
            self.keyboard_guard = Some(k_guard);
        }
        Ok(())
    }

    /// 停止多标签增强守护
    pub fn stop(&mut self) {
        self.hook_guard = None;
        self.mouse_guard = None;
        self.keyboard_guard = None;
    }

    /// 获取协调器引用
    pub fn coordinator(&self) -> Arc<TabCoordinator> {
        self.coordinator.clone()
    }

    /// 更新配置
    pub fn update_config(&self, cfg: TabConfig) {
        self.coordinator.update_config(cfg);
    }
}

impl Default for TabModule {
    fn default() -> Self {
        Self::new()
    }
}
