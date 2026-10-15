#![allow(dead_code)]

//! 用户**显式触发**的一次性第三方管理器路径探测（审计报告 U-01，方案 B 完整落地）。
//!
//! 报告 §2.3 的推荐实现把「允许使用」与「发起一次探测」拆开，本模块是整个拆分的落点：
//!
//! 1. `AppConfig::enable_totalcmd` / `enable_xyplorer` **只表示用户已授权**——打开开关
//!    不会自行修改剪贴板，后台 scanner 也不再探测这两个管理器（报告 §2.4 验收标准 1/3）；
//! 2. 每次探测都必须可追溯到一次用户点击（托盘菜单 / 悬浮吸附条按钮）；
//! 3. 目标窗口必须可归因：当前前台窗口，或「用户打开触发入口那一刻的窗口」（有界时效 +
//!    句柄 / PID / 类名 / 进程镜像逐项复核），任一不一致即取消；
//! 4. 剪贴板事务的 fail-closed 检查（唯一事件 / sequence / owner / 事件环完整性 /
//!    失败后不恢复）全部保持不变，仍由 `ClipboardTransaction` 承载。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, IsWindow, IsWindowVisible};

use super::ipc::{self, Manager};

/// 「用户刚刚以该窗口为目标」的有效期。
///
/// 托盘菜单与悬浮吸附条在弹出/点击时都会短暂取得前台（悬浮条为 `WS_EX_NOACTIVATE`，
/// 但对话框本身仍在前台），因此不能要求「探测那一刻管理器仍是前台」，只能要求
/// 「用户发起这次触发的那一刻，该管理器窗口是前台」。超过这个时限就不再认账。
pub const FOREGROUND_TARGET_RECENCY: Duration = Duration::from_secs(600);

/// 可显式触发的管理器（仅限「只能靠系统剪贴板回传路径」的两个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptedManager {
    TotalCmd,
    XYplorer,
}

impl ScriptedManager {
    /// 供 UI / 日志使用的稳定标识
    pub fn label(self) -> &'static str {
        match self {
            Self::TotalCmd => "totalcmd",
            Self::XYplorer => "xyplorer",
        }
    }

    fn ipc(self) -> Manager {
        match self {
            Self::TotalCmd => Manager::TotalCmd,
            Self::XYplorer => Manager::XYplorer,
        }
    }

    fn window_class(self) -> &'static str {
        match self {
            Self::TotalCmd => super::totalcmd::WINDOW_CLASS,
            Self::XYplorer => super::xyplorer::WINDOW_CLASS,
        }
    }

    fn process_names(self) -> &'static [&'static str] {
        match self {
            Self::TotalCmd => &super::totalcmd::EXPECTED_PROCESS_NAMES,
            Self::XYplorer => &super::xyplorer::EXPECTED_PROCESS_NAMES,
        }
    }
}

/// 一次显式探测的结局（结构化，不含任何剪贴板正文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplicitProbeOutcome {
    /// 取到来源可证明、且已校验为真实目录的路径
    Path(String),
    /// 用户尚未授权该能力（配置开关关闭）
    NotAuthorized,
    /// 找不到可归因的目标窗口（不在前台 / 记录过期 / 身份已变）
    NoForegroundTarget,
    /// 处于上次失败后的冷却窗口
    CoolingDown,
    /// 探测已执行，但未取得可证明的路径（超时 / 来源不可证明 / 归属丢失）
    NoPath,
}

/// 已解析并通过身份核验的目标窗口（以 `usize` 持有句柄，便于跨线程传递）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetWindow {
    pub hwnd_raw: usize,
    pub pid: u32,
    pub manager: ScriptedManager,
}

impl TargetWindow {
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd_raw as *mut _)
    }
}

#[derive(Debug, Clone, Copy)]
struct RecordedTarget {
    hwnd_raw: usize,
    pid: u32,
    at: Instant,
}

fn recency_table() -> &'static Mutex<HashMap<Manager, RecordedTarget>> {
    static TABLE: OnceLock<Mutex<HashMap<Manager, RecordedTarget>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_table() -> std::sync::MutexGuard<'static, HashMap<Manager, RecordedTarget>> {
    recency_table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 宿主前台事件回调：把「某个管理器窗口刚刚成为前台」记入有界时效表。
///
/// 仅做一次窗口类名比对（廉价）即可排除绝大多数窗口，命中后才查进程镜像。
pub fn note_foreground_window(hwnd: HWND) {
    for manager in [ScriptedManager::TotalCmd, ScriptedManager::XYplorer] {
        if let Some(target) = identify(hwnd, manager) {
            let mut table = lock_table();
            table.insert(
                manager.ipc(),
                RecordedTarget {
                    hwnd_raw: target.hwnd_raw,
                    pid: target.pid,
                    at: Instant::now(),
                },
            );
        }
    }
}

/// 解析本次显式触发的目标窗口。
///
/// 1. 当前前台窗口就是目标管理器 → 直接采用（最严格的前台语义）；
/// 2. 否则回退到时效表里「用户发起触发那一刻的前台窗口」，并逐项复核身份。
pub fn resolve_target(manager: ScriptedManager) -> Option<TargetWindow> {
    let foreground = crate::win32::events::get_active_foreground_window();
    if let Some(target) = identify(foreground, manager) {
        return Some(target);
    }

    let recorded = {
        let table = lock_table();
        table.get(&manager.ipc()).copied()
    };
    let (hwnd_raw, pid) = fresh_target(
        recorded.map(|r| (r.hwnd_raw, r.pid, r.at)),
        Instant::now(),
        FOREGROUND_TARGET_RECENCY,
    )?;
    if !verify_target(manager, hwnd_raw, pid) {
        return None;
    }
    Some(TargetWindow {
        hwnd_raw,
        pid,
        manager,
    })
}

/// 时效判定（纯函数，便于单测）：只有「未过期」的记录才允许继续做身份核验。
fn fresh_target(
    recorded: Option<(usize, u32, Instant)>,
    now: Instant,
    recency: Duration,
) -> Option<(usize, u32)> {
    let (hwnd_raw, pid, at) = recorded?;
    if now.duration_since(at) > recency {
        return None;
    }
    Some((hwnd_raw, pid))
}

/// 身份核验（句柄 + PID + 类名 + 进程镜像 + 可见 + 顶层窗口）。
///
/// 与首次识别共用同一套条件，因此「解析后窗口被关闭 / HWND 被复用 / 进程重启」
/// 都会让探测在投递任何协议消息之前取消。
pub fn verify_target(manager: ScriptedManager, hwnd_raw: usize, pid: u32) -> bool {
    if hwnd_raw == 0 {
        return false;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        // 只接受顶层窗口：子控件句柄同样可能属于该进程，但不应接收管理器协议消息
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return false;
        }
    }
    if crate::dialog::detector::get_window_class_name(hwnd) != manager.window_class() {
        return false;
    }
    if !ipc::window_process_matches(hwnd, manager.process_names()) {
        return false;
    }
    ipc::window_process_id(hwnd) == pid
}

/// 识别单个窗口是否就是目标管理器的可归因顶层窗口。
fn identify(hwnd: HWND, manager: ScriptedManager) -> Option<TargetWindow> {
    if hwnd.0.is_null() {
        return None;
    }
    // 先用类名做廉价筛查，避免每次前台变化都去查进程信息
    if crate::dialog::detector::get_window_class_name(hwnd) != manager.window_class() {
        return None;
    }
    let pid = ipc::window_process_id(hwnd);
    if !verify_target(manager, hwnd.0 as usize, pid) {
        return None;
    }
    Some(TargetWindow {
        hwnd_raw: hwnd.0 as usize,
        pid,
        manager,
    })
}

/// 用户显式触发一次探测（唯一入口）。
///
/// `authorized` 来自配置开关（只表示用户已授权）；**授权本身绝不触发任何探测**。
pub fn probe_once(manager: ScriptedManager, authorized: bool) -> ExplicitProbeOutcome {
    if !authorized {
        return ExplicitProbeOutcome::NotAuthorized;
    }
    if !ipc::try_enter(manager.ipc()) {
        return ExplicitProbeOutcome::CoolingDown;
    }
    let Some(target) = resolve_target(manager) else {
        return ExplicitProbeOutcome::NoForegroundTarget;
    };
    match manager {
        ScriptedManager::TotalCmd => super::totalcmd::probe_explicit_target(&target),
        ScriptedManager::XYplorer => super::xyplorer::probe_explicit_target(&target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recency_window_bounds_the_recorded_target() {
        let now = Instant::now();
        let fresh = now - Duration::from_secs(1);
        let stale = now - FOREGROUND_TARGET_RECENCY - Duration::from_secs(1);

        assert_eq!(
            fresh_target(Some((0x1234, 42, fresh)), now, FOREGROUND_TARGET_RECENCY),
            Some((0x1234, 42)),
            "时效窗口内的记录必须可用"
        );
        assert_eq!(
            fresh_target(Some((0x1234, 42, stale)), now, FOREGROUND_TARGET_RECENCY),
            None,
            "过期记录必须被丢弃（否则会向早已易主/重启的窗口投递消息）"
        );
        assert_eq!(
            fresh_target(None, now, FOREGROUND_TARGET_RECENCY),
            None,
            "从未记录过前台管理器时不得探测"
        );
    }

    /// 审计报告 U-01 验收标准 3：**仅开启授权不得自行修改剪贴板**。
    /// 因此未授权时连目标解析都不做（直接返回 `NotAuthorized`），
    /// 且本模块不得直接触碰剪贴板事务（事务只在目标已解析并核验后、由管理器模块开启）。
    #[test]
    fn test_unauthorized_never_resolves_or_probes() {
        assert_eq!(
            probe_once(ScriptedManager::TotalCmd, false),
            ExplicitProbeOutcome::NotAuthorized
        );
        assert_eq!(
            probe_once(ScriptedManager::XYplorer, false),
            ExplicitProbeOutcome::NotAuthorized
        );

        const SOURCE: &str = include_str!("explicit_probe.rs");
        let production = &SOURCE[..SOURCE.find("#[cfg(test)]").expect("必须存在测试模块")];
        // 先剔除整行注释，避免架构说明文字本身触发误报
        let code = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("ClipboardTransaction"),
            "目标解析模块不得直接开启剪贴板事务（否则无目标时也会触碰剪贴板）"
        );
        assert!(
            !code.contains("EmptyClipboard") && !code.contains("SetClipboardData"),
            "剪贴板写入必须全部走 ClipboardTransaction 事务 API"
        );
    }

    #[test]
    fn test_manager_specs_point_at_the_real_managers() {
        assert_eq!(ScriptedManager::TotalCmd.window_class(), "TTOTAL_CMD");
        assert!(
            ScriptedManager::TotalCmd
                .process_names()
                .contains(&"totalcmd64.exe")
        );
        assert_eq!(ScriptedManager::XYplorer.window_class(), "ThunderRT6FormDC");
        assert_eq!(ScriptedManager::XYplorer.process_names(), &["xyplorer.exe"]);
        assert_eq!(ScriptedManager::TotalCmd.label(), "totalcmd");
    }

    /// 架构守卫：后台扫描链路不得再出现 TC / XY 探测器。
    ///
    /// U-01 的核心是「授权 ≠ 自动探测」。若哪天有人把这两个探测器接回后台扫描，
    /// 仅开启开关就会重新持续占用剪贴板 —— 本守卫直接变红。
    #[test]
    fn test_background_scanner_never_probes_scripted_managers() {
        let sources = [
            ("managers/scanner.rs", include_str!("scanner.rs")),
            ("managers/tracker.rs", include_str!("tracker.rs")),
        ];
        for (name, source) in sources {
            let code = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for forbidden in [
                "totalcmd::",
                "xyplorer::",
                "get_totalcmd_folders",
                "get_xyplorer_folders",
                "probe_explicit_target",
                "explicit_probe",
            ] {
                assert!(
                    !code.contains(forbidden),
                    "{} 不得参与 TC / XYplorer 探测（命中 `{}`）：授权开关只授权，不自动探测",
                    name,
                    forbidden
                );
            }
        }
    }
}
