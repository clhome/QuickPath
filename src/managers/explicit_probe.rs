#![allow(dead_code)]

//! 用户**显式触发**的一次性第三方管理器路径探测（审计报告 U-01 / U-01A）。
//!
//! 报告 §2.3 的推荐实现把「允许使用」与「发起一次探测」拆开，本模块是整个拆分的落点：
//!
//! 1. `AppConfig::enable_totalcmd` / `enable_xyplorer` **只表示用户已授权**——打开开关
//!    不会自行修改剪贴板，后台 scanner 也不再探测这两个管理器；
//! 2. 每次探测都必须可追溯到一次用户点击（托盘菜单 / 悬浮吸附条按钮），且目标窗口
//!    只能来自**触发事件那一刻冻结的不可变快照**（`TriggerSnapshot`），不再有
//!    「600 秒内曾在前台」这类墙钟回退（审计报告 U-01A）；
//! 3. 探测的**整个生命周期**（快照 → 清空 → 请求管理器 → 读回 → 恢复）由进程级
//!    独占 lease 保护（审计报告 P0-06），拿不到 lease 立即返回 `AlreadyRunning`
//!    且零剪贴板触碰；
//! 4. 剪贴板事务的 fail-closed 检查（唯一事件 / sequence / owner / 事件环完整性 /
//!    失败后不恢复）全部保持不变，仍由 `ClipboardTransaction` 承载。
//!
//! ## 触发口径（两类入口刻意不同，文档必须如实标注）
//!
//! | 入口 | 快照生成时机 | 口径 |
//! |---|---|---|
//! | 托盘菜单 | `WM_RBUTTONUP`（弹出菜单前） | **严格前台**：那一刻前台必须是该管理器，否则菜单项置灰 |
//! | 悬浮吸附条 | 对话框打开事件 | **事件绑定**：冻结「最近一个前台管理器」；吸附条是 `WS_EX_NOACTIVATE`，点击时前台永远是对话框，故**不是**严格前台触发 |
//!
//! 两类快照都会在投递协议消息前逐项复核 HWND / PID / 类名 / 进程镜像 / 可见性 / 顶层属性，
//! 窗口被关闭、HWND 被复用或进程重启都会在写下任何剪贴板内容前取消。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, IsWindow, IsWindowVisible};

use super::ipc::{self, Manager};
use super::probe_lease::ProbeLease;

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

    /// UI 数组下标（`[TotalCmd, XYplorer]`，与托盘 / 吸附条一致）
    pub const ALL: [Self; 2] = [Self::TotalCmd, Self::XYplorer];
}

/// 一次显式探测的结局（结构化，不含任何剪贴板正文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplicitProbeOutcome {
    /// 取到来源可证明、且已校验为真实目录的路径
    Path(String),
    /// 用户尚未授权该能力（配置开关关闭）
    NotAuthorized,
    /// 快照指向的窗口已不可归因（关闭 / 易主 / 进程重启 / 不可见）
    NoForegroundTarget,
    /// 处于上次失败后的冷却窗口
    CoolingDown,
    /// **已有一次显式探测正在进行**（进程级独占 lease 拒绝，P0-06）
    AlreadyRunning,
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

/// 一次触发事件冻结下来的**不可变目标快照**（审计报告 U-01A）。
///
/// 快照只在「用户发起触发」（托盘右键）或「对话框打开」两个 UI 事件里生成，
/// 之后探测只消费这份快照，绝不重新去猜目标窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerSnapshot {
    pub hwnd_raw: usize,
    pub pid: u32,
    pub manager: ScriptedManager,
    /// 生成时刻（仅用于诊断日志；**不作为可用性判据**，U-01A 已删除墙钟时效）
    pub captured_at: Instant,
    /// 单次消费标识：同一 `request_id` 只允许触发一次探测
    pub request_id: u64,
}

impl TriggerSnapshot {
    fn target(&self) -> TargetWindow {
        TargetWindow {
            hwnd_raw: self.hwnd_raw,
            pid: self.pid,
            manager: self.manager,
        }
    }
}

/// 「最近一个前台管理器」记录（**不含墙钟时效**）。
///
/// 只服务于吸附条：吸附条是 `WS_EX_NOACTIVATE`，点击时前台必然是对话框，因此它
/// 只能在**对话框打开事件**那一刻把这份记录冻结成快照。托盘菜单不使用本表。
#[derive(Debug, Clone, Copy)]
struct LastForeground {
    hwnd_raw: usize,
    pid: u32,
}

fn last_foreground_table() -> &'static Mutex<HashMap<Manager, LastForeground>> {
    static TABLE: OnceLock<Mutex<HashMap<Manager, LastForeground>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_table() -> std::sync::MutexGuard<'static, HashMap<Manager, LastForeground>> {
    last_foreground_table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn next_request_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// 宿主前台事件回调：记录「某个管理器窗口刚刚成为前台」。
///
/// 仅做一次窗口类名比对（廉价）即可排除绝大多数窗口，命中后才查进程镜像。
/// 本函数**不做任何探测、不碰剪贴板**。
pub fn note_foreground_window(hwnd: HWND) {
    for manager in ScriptedManager::ALL {
        if let Some(target) = identify(hwnd, manager) {
            let mut table = lock_table();
            table.insert(
                manager.ipc(),
                LastForeground {
                    hwnd_raw: target.hwnd_raw,
                    pid: target.pid,
                },
            );
        }
    }
}

/// 托盘菜单：在**右键那一刻**严格取当前前台窗口（审计报告 U-01A 托盘方案）。
///
/// 前台不是目标管理器时返回 `None`，对应菜单项必须置灰。这里不做任何墙钟回退，
/// 因此「十分钟前的旧记录」不可能让菜单项重新可用。
pub fn capture_foreground_snapshot(manager: ScriptedManager) -> Option<TriggerSnapshot> {
    let foreground = crate::win32::events::get_active_foreground_window();
    let target = identify(foreground, manager)?;
    Some(TriggerSnapshot {
        hwnd_raw: target.hwnd_raw,
        pid: target.pid,
        manager,
        captured_at: Instant::now(),
        request_id: next_request_id(),
    })
}

/// 吸附条：在对话框打开事件那一刻，把「最近一个前台管理器」冻结为不可变快照。
///
/// 同一对话框（generation = 对话框句柄 + 授权组合）复用同一份快照，对话框变化即重新冻结。
/// 该语义**不是严格前台触发**（报告 §3「吸附条推荐方案」明确允许的替代方案），
/// 因此文档必须如实标注。
pub fn bar_probe_targets(
    dialog_hwnd_raw: usize,
    authorized: [bool; 2],
) -> [Option<TriggerSnapshot>; 2] {
    let requested = (dialog_hwnd_raw, authorized[0], authorized[1]);
    let mut generation = bar_generation()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if should_refreeze(generation.key, requested) {
        generation.key = Some(requested);
        generation.targets = [
            freeze_last_foreground(ScriptedManager::TotalCmd, authorized[0]),
            freeze_last_foreground(ScriptedManager::XYplorer, authorized[1]),
        ];
    }
    generation.targets
}

/// 是否需要重新冻结（纯函数，便于单测）：generation 未变则复用既有快照。
///
/// `current == None` 表示已失效（`invalidate_bar_targets` 或首次展示）。
fn should_refreeze(current: Option<(usize, bool, bool)>, requested: (usize, bool, bool)) -> bool {
    current != Some(requested)
}

/// 对话框关闭 / 吸附条隐藏时调用：使当前 generation 失效。
pub fn invalidate_bar_targets() {
    let mut generation = bar_generation()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    generation.key = None;
    generation.targets = [None, None];
}

struct BarGeneration {
    key: Option<(usize, bool, bool)>,
    targets: [Option<TriggerSnapshot>; 2],
}

fn bar_generation() -> &'static Mutex<BarGeneration> {
    static GENERATION: OnceLock<Mutex<BarGeneration>> = OnceLock::new();
    GENERATION.get_or_init(|| {
        Mutex::new(BarGeneration {
            key: None,
            targets: [None, None],
        })
    })
}

fn freeze_last_foreground(manager: ScriptedManager, authorized: bool) -> Option<TriggerSnapshot> {
    if !authorized {
        return None;
    }
    let recorded = {
        let table = lock_table();
        table.get(&manager.ipc()).copied()
    }?;
    // 冻结时先复核一次身份：窗口早已关闭 / 易主 / 重启则视为无目标，按钮不出现。
    if !verify_target(manager, recorded.hwnd_raw, recorded.pid) {
        return None;
    }
    Some(TriggerSnapshot {
        hwnd_raw: recorded.hwnd_raw,
        pid: recorded.pid,
        manager,
        captured_at: Instant::now(),
        request_id: next_request_id(),
    })
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

/// 用户显式触发一次探测（唯一入口）：消费一份**不可变触发快照**。
///
/// `authorized` 来自配置开关（只表示用户已授权）；**授权本身绝不触发任何探测**。
///
/// 顺序（每一步都不得越前）：
/// 1. 授权闸门 → 2. 失败冷却 → 3. **进程级独占 lease（P0-06）** → 4. 快照身份核验 →
/// 5. 管理器事务（`ClipboardTransaction::begin()` 及之后的全部剪贴板操作）。
pub fn probe_with_snapshot(snapshot: &TriggerSnapshot, authorized: bool) -> ExplicitProbeOutcome {
    if !authorized {
        return ExplicitProbeOutcome::NotAuthorized;
    }
    if !ipc::try_enter(snapshot.manager.ipc()) {
        return ExplicitProbeOutcome::CoolingDown;
    }
    // P0-06：lease 覆盖「快照 → 清空 → 请求 → 读回 → 恢复」整个生命周期。
    // 拿不到就立即返回，**绝不读取 / 清空 / 写入剪贴板**。
    let Some(_lease) = ProbeLease::try_acquire() else {
        crate::dialog::detector::log_debug(&format!(
            "EXPLICIT_PROBE_ALREADY_RUNNING({}): 已有一次显式探测在进行，本次请求被拒绝（零剪贴板触碰）",
            snapshot.manager.label()
        ));
        return ExplicitProbeOutcome::AlreadyRunning;
    };
    if !verify_target(snapshot.manager, snapshot.hwnd_raw, snapshot.pid) {
        return ExplicitProbeOutcome::NoForegroundTarget;
    }
    let target = snapshot.target();
    match snapshot.manager {
        ScriptedManager::TotalCmd => super::totalcmd::probe_explicit_target(&target),
        ScriptedManager::XYplorer => super::xyplorer::probe_explicit_target(&target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用：在「最近前台管理器」表里临时塞一条记录，执行 `f` 后恢复原状。
    ///
    /// 用于验证**严格前台捕获**（托盘）与**事件绑定冻结**（吸附条）的差异：
    /// 前者必须完全无视该表，后者才允许使用它。
    fn test_with_seeded_last_foreground<R>(manager: ScriptedManager, f: impl FnOnce() -> R) -> R {
        lock_table().insert(
            manager.ipc(),
            LastForeground {
                hwnd_raw: 0xDEAD_BEEF,
                pid: 4242,
            },
        );
        let result = f();
        lock_table().remove(&manager.ipc());
        result
    }

    /// U-01A 核心不变式：**不存在**任何墙钟回退。目标只能来自显式传入的快照。
    #[test]
    fn test_no_wall_clock_fallback_remains() {
        const SOURCE: &str = include_str!("explicit_probe.rs");
        let production = &SOURCE[..SOURCE.find("\nmod tests {").expect("必须存在测试模块")];
        for forbidden in [
            "FOREGROUND_TARGET_RECENCY",
            "fresh_target",
            "Duration::from_secs(600)",
            "resolve_target",
        ] {
            assert!(
                !production.contains(forbidden),
                "审计报告 U-01A 要求删除 600 秒旧前台回退（仍命中 `{}`）",
                forbidden
            );
        }
        assert!(
            production.contains("TriggerSnapshot"),
            "目标必须只来自不可变的触发快照"
        );
    }

    /// 审计报告 U-01 验收标准 3：**仅开启授权不得自行修改剪贴板**。
    #[test]
    fn test_unauthorized_never_probes_or_touches_clipboard() {
        let snapshot = TriggerSnapshot {
            hwnd_raw: 0,
            pid: 0,
            manager: ScriptedManager::TotalCmd,
            captured_at: Instant::now(),
            request_id: 1,
        };
        assert_eq!(
            probe_with_snapshot(&snapshot, false),
            ExplicitProbeOutcome::NotAuthorized
        );

        const SOURCE: &str = include_str!("explicit_probe.rs");
        let production = &SOURCE[..SOURCE.find("\nmod tests {").expect("必须存在测试模块")];
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

    /// 快照的 `request_id` 必须逐次唯一（同一份快照只允许被消费一次，
    /// 由宿主侧 `take()` 语义保证；这里先保证生成端不会复用 id）。
    #[test]
    fn test_snapshots_get_unique_request_ids() {
        let a = next_request_id();
        let b = next_request_id();
        assert_ne!(a, b, "每次触发必须拿到新的 request_id");
    }

    /// 严格前台捕获：没有前台管理器时不得凭空造出快照。
    ///
    /// 该用例在无头/无管理器环境下同样成立：前台窗口不可能是 TC / XYplorer 的
    /// 可归因顶层窗口，因此必须返回 `None`（菜单项置灰）。
    #[test]
    fn test_strict_capture_returns_none_when_foreground_is_not_a_manager() {
        let foreground = crate::win32::events::get_active_foreground_window();
        let is_manager = ScriptedManager::ALL
            .iter()
            .any(|m| identify(foreground, *m).is_some());
        if is_manager {
            return; // 真机恰好停在管理器窗口上时跳过（不影响被测语义）
        }
        assert!(
            capture_foreground_snapshot(ScriptedManager::TotalCmd).is_none(),
            "前台不是 Total Commander 时托盘菜单项必须置灰"
        );
        assert!(
            capture_foreground_snapshot(ScriptedManager::XYplorer).is_none(),
            "前台不是 XYplorer 时托盘菜单项必须置灰"
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

    /// 严格前台捕获（托盘）：**完全无视**「最近前台管理器」表。
    ///
    /// 表里存在记录也不能让菜单项变可用，否则就是审计报告 U-01A 指出的
    /// 「旧前台记录被误当成本次触发目标」。
    #[test]
    fn test_strict_capture_ignores_the_last_foreground_table() {
        let foreground = crate::win32::events::get_active_foreground_window();
        let is_manager = ScriptedManager::ALL
            .iter()
            .any(|m| identify(foreground, *m).is_some());
        if is_manager {
            return; // 真机恰好停在管理器窗口上时跳过（不影响被测语义）
        }
        let captured = test_with_seeded_last_foreground(ScriptedManager::TotalCmd, || {
            capture_foreground_snapshot(ScriptedManager::TotalCmd)
        });
        assert!(
            captured.is_none(),
            "托盘严格前台捕获不得回退到「最近前台」记录（U-01A）"
        );

        // 行为断言在测试环境无法提供“身份可验证的假窗口”，因此再补一条源码守卫：
        // `capture_foreground_snapshot` 函数体不得引用任何历史表。
        const SOURCE: &str = include_str!("explicit_probe.rs");
        let production = &SOURCE[..SOURCE.find("\nmod tests {").expect("必须存在测试模块")];
        let start = production
            .find("pub fn capture_foreground_snapshot")
            .expect("必须存在严格前台捕获入口");
        let end = start
            + production[start..]
                .find("\n}\n")
                .expect("函数必须以独立花括号结尾");
        let body = &production[start..end];
        for forbidden in [
            "freeze_last_foreground",
            "lock_table",
            "last_foreground_table",
        ] {
            assert!(
                !body.contains(forbidden),
                "托盘严格前台捕获不得回退到历史记录（命中 `{}`）：U-01A",
                forbidden
            );
        }
    }

    /// 吸附条 generation：对话框变化必须重新冻结，对话框关闭必须失效。
    #[test]
    fn test_bar_targets_are_generation_bound() {
        // 纯函数层面的 generation 语义
        assert!(should_refreeze(None, (1, true, true)), "首次展示必须冻结");
        assert!(
            !should_refreeze(Some((1, true, true)), (1, true, true)),
            "同一对话框必须复用同一份快照"
        );
        assert!(
            should_refreeze(Some((1, true, true)), (2, true, true)),
            "对话框变化必须重新冻结"
        );
        assert!(
            should_refreeze(Some((1, true, true)), (1, false, true)),
            "授权组合变化必须重新冻结（按钮可见性随授权变化）"
        );

        invalidate_bar_targets();
        assert!(
            should_refreeze(
                {
                    // 失效后 key 必须回到 None
                    bar_generation()
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .key
                },
                (0x1111, true, true)
            ),
            "对话框关闭后必须失效，不得继续沿用旧快照"
        );

        // 未授权 → 不产生任何快照（按钮不出现）
        let unauthorized = bar_probe_targets(0x1111, [false, false]);
        assert!(unauthorized[0].is_none() && unauthorized[1].is_none());

        // 授权但从未记录过前台管理器 → 仍然没有目标（不回退到任何墙钟记录）
        let no_record = bar_probe_targets(0x1111, [true, true]);
        assert!(
            no_record[0].is_none() && no_record[1].is_none(),
            "没有任何前台管理器记录时不得凭空生成吸附条目标"
        );
        assert_eq!(bar_probe_targets(0x1111, [true, true]), no_record);

        invalidate_bar_targets();
        assert_eq!(bar_probe_targets(0x1111, [true, true]), [None, None]);
    }
}
