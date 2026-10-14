#![allow(dead_code)]

pub mod com;
pub mod coordinator;
pub mod hook;
pub mod keyboard;
pub mod mouse;
pub mod owner;
pub mod select;
pub mod uia;

pub use com::ShellComManager;
pub use coordinator::{TabConfig, TabCoordinator};
pub use hook::TabHookGuard;
pub use keyboard::TabKeyboardGuard;
pub use mouse::TabMouseGuard;
use std::sync::Arc;
use windows::Win32::Foundation::HWND;

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

    /// 启动多标签增强守护。
    ///
    /// `notify_hwnd` 为宿主消息窗口：低级键盘钩子只向它投递自定义消息，
    /// 自身不做任何 COM/UIA 重活，保证钩子回调在微秒级返回。
    pub fn start(&mut self, notify_hwnd: HWND) -> Result<(), String> {
        // 已全部就绪则幂等返回，不再重复安装
        if self.hook_guard.is_some() && self.mouse_guard.is_some() && self.keyboard_guard.is_some()
        {
            return Ok(());
        }

        // 三个钩子要么全装上、要么一个都不留（报告 P2-01）。
        // 先清掉上一次半途失败可能留下的残留，保证下面新建的三个属于同一批。
        self.stop();

        let hook_guard = TabHookGuard::new(self.coordinator.clone())?;
        let mouse_guard = match TabMouseGuard::new(self.coordinator.clone()) {
            Ok(guard) => guard,
            Err(err) => {
                // 回滚：drop 会归还已安装的 HOOK_SLOT
                drop(hook_guard);
                return Err(err);
            }
        };
        let keyboard_guard = match TabKeyboardGuard::new(self.coordinator.clone(), notify_hwnd) {
            Ok(guard) => guard,
            Err(err) => {
                drop(mouse_guard);
                drop(hook_guard);
                return Err(err);
            }
        };

        self.hook_guard = Some(hook_guard);
        self.mouse_guard = Some(mouse_guard);
        self.keyboard_guard = Some(keyboard_guard);
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
