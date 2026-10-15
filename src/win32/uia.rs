#![allow(dead_code)]

use crate::dialog::detector::{log_debug, redact_path_structure};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    TreeScope_Descendants, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, VK_CONTROL, VK_RETURN, keybd_event,
    mouse_event,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId,
    IsWindow, SetCursorPos, SetForegroundWindow,
};
use windows::core::{BSTR, Interface};

/// 调试日志用的「文本指纹」：只暴露**结构信息**，绝不记录原文（审计报告 S-LOG）。
///
/// `old_text` / `cur_val` / `saved_name` / 编辑框当前值都可能包含用户目录、客户名、
/// 项目名或文档名，而故障排查时日志最可能被打包转发。因此这些位置一律改写为
/// 「长度 + 类型类别 + 会话内关联 ID」：
/// - `len` / 类别足以判断“是空还是目录还是文件名”，并解释“为何判定需要恢复”；
/// - `session_id` 是**带进程内随机盐**的哈希，同一次故障的多条日志可据此串联，
///   但无法反推原文，也无法跨进程/跨会话做字典比對。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TextFingerprint {
    len: usize,
    kind: &'static str,
    session_id: u64,
}

impl TextFingerprint {
    fn of(text: &str) -> Self {
        Self {
            len: text.chars().count(),
            kind: classify_text(text),
            session_id: session_hash(text),
        }
    }
}

impl std::fmt::Display for TextFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "len={} kind={} id={:08x}",
            self.len, self.kind, self.session_id
        )
    }
}

/// 允许原样输出的 UIA 类名白名单：**通用控件类名**，不含任何用户数据。
///
/// 该白名单与下面 `SafeUiaMetadata` 的判定共同构成审计报告 S-LOG-A 的“固定标签”语义：
/// 只有精确命中白名单、且长度与字符集受限的值才允许原样出现。
const SAFE_UIA_CLASS_NAMES: &[&str] = &[
    "QLineEdit",
    "kd::KDTextField",
    "Edit",
    "ComboBox",
    "ComboBoxEx32",
    "Button",
    "Static",
    "Splitter",
    "ScrollBar",
    "Slider",
    "ProgressBar",
    "DirectUIHWND",
    "SysListView32",
];

/// 允许原样输出的自动化 ID 白名单：**本文件评分规则自己使用的语义标签**，
/// 它们是固定常量而非用户数据。其余一律降级为指纹。
const SAFE_UIA_AUTO_IDS: &[&str] = &[
    "fileName",
    "FileName",
    "file_name",
    "filename",
    "search",
    "Search",
];

/// UIA 诊断字段的**统一安全格式化器**（审计报告 S-LOG-A）。
///
/// `name` 来自第三方控件的 `CurrentName()`，可能直接暴露文件名 / 路径 / 业务内容，
/// 因此**永远只输出指纹**；`class_name` / `auto_id` 仅在精确命中白名单（且长度、
/// 字符集受限）时输出固定标签，否则同样输出指纹。
///
/// 所有 UIA 日志调用点**必须**使用本格式化器，禁止自行拼接原文。
struct SafeUiaMetadata {
    name: TextFingerprint,
    class_label: String,
    id_label: String,
}

impl SafeUiaMetadata {
    fn of(name: &str, class_name: &str, auto_id: &str) -> Self {
        Self {
            name: TextFingerprint::of(name),
            class_label: safe_uia_label(class_name, SAFE_UIA_CLASS_NAMES),
            id_label: safe_uia_label(auto_id, SAFE_UIA_AUTO_IDS),
        }
    }
}

impl std::fmt::Display for SafeUiaMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "name={} cls={} id={}",
            self.name, self.class_label, self.id_label
        )
    }
}

/// 白名单精确匹配 + 长度/字符集受限时才原样输出，否则回退到指纹。
fn safe_uia_label(value: &str, whitelist: &[&str]) -> String {
    if value.len() <= 32 && value.is_ascii() && whitelist.contains(&value) {
        value.to_string()
    } else {
        TextFingerprint::of(value).to_string()
    }
}

/// 文本类别（不含任何主体内容）：空 / 目录形式 / 带扩展名的文件名 / 其他。
fn classify_text(text: &str) -> &'static str {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "empty";
    }
    if trimmed.ends_with('\\') || trimmed.ends_with('/') {
        return "dir";
    }
    let name = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            "file"
        }
        _ => "text",
    }
}

/// 会话内随机盐：每个进程一次，保证同一文本在本次会话内哈希稳定、跨会话不可关联。
fn session_hash_salt() -> &'static std::collections::hash_map::RandomState {
    static SALT: std::sync::OnceLock<std::collections::hash_map::RandomState> =
        std::sync::OnceLock::new();
    SALT.get_or_init(std::collections::hash_map::RandomState::new)
}

fn session_hash(text: &str) -> u64 {
    use std::hash::BuildHasher;
    session_hash_salt().hash_one(text)
}

/// 目标窗口身份指纹：句柄 + 进程 ID + 进程创建时间 + 窗口类名。
///
/// 审计报告 P0-02：后台注入任务必须携带身份并在每个动作前复核，
/// 否则用户切换应用/关闭窗口/HWND 被复用后，全局鼠标与键盘会被送到错误窗口。
#[derive(Debug, Clone)]
struct WindowIdentity {
    hwnd_raw: usize,
    pid: u32,
    /// 进程创建时间（无法查询时为 None，例如目标进程权限更高）
    create_time: Option<u64>,
    class_name: String,
}

impl WindowIdentity {
    fn capture(hwnd: HWND) -> Option<Self> {
        unsafe {
            if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() {
                return None;
            }
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return None;
            }

            let mut buf = [0u16; 128];
            let len = GetClassNameW(hwnd, &mut buf);
            let class_name = if len > 0 {
                String::from_utf16_lossy(&buf[..len as usize])
            } else {
                String::new()
            };

            Some(Self {
                hwnd_raw: hwnd.0 as usize,
                pid,
                create_time: process_creation_time(pid),
                class_name,
            })
        }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd_raw as *mut _)
    }

    /// 复核窗口身份：句柄存活 + 进程 ID 一致 + 创建时间一致 + 窗口类名一致。
    /// 任何一项不符即视为“窗口已被替换/复用”，调用方必须立即取消后续全局输入。
    fn verify(&self) -> bool {
        unsafe {
            let hwnd = self.hwnd();
            if !IsWindow(Some(hwnd)).as_bool() {
                return false;
            }
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != self.pid {
                return false;
            }
            if let (Some(expected), Some(current)) = (self.create_time, process_creation_time(pid))
                && expected != current
            {
                return false;
            }

            let mut buf = [0u16; 128];
            let len = GetClassNameW(hwnd, &mut buf);
            let class_name = if len > 0 {
                String::from_utf16_lossy(&buf[..len as usize])
            } else {
                String::new()
            };
            class_name == self.class_name
        }
    }

    fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd() }
    }

    /// 对话框 generation 指纹:任一要素变化(HWND 复用 / 进程重启 / 类名变化)
    /// 即视为**不同**对话框,从而不影响新对话框的去重判定。
    fn fingerprint(&self) -> String {
        format!(
            "{:x}:{}:{:?}:{}",
            self.hwnd_raw, self.pid, self.create_time, self.class_name
        )
    }

    /// 将目标窗口带到前台并核验 `GetForegroundWindow()` 确实等于它。
    ///
    /// 绝不信任 `BringWindowToTop` / `SetForegroundWindow` 的返回值：
    /// 前台锁会让它们静默失败，只有前台窗口比对才是可靠证据。
    fn bring_to_front(&self) -> bool {
        unsafe {
            for attempt in 0..4 {
                if attempt > 0 {
                    thread::sleep(Duration::from_millis(40));
                }
                if !self.verify() {
                    return false;
                }
                if self.is_foreground() {
                    return true;
                }
                let _ = BringWindowToTop(self.hwnd());
                let _ = SetForegroundWindow(self.hwnd());
            }
            self.verify() && self.is_foreground()
        }
    }
}

fn filetime_to_u64(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// 查询进程创建时间（用于识别 PID 复用后的“同 PID 新进程”）
fn process_creation_time(pid: u32) -> Option<u64> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let result = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        result.ok()?;
        Some(filetime_to_u64(&creation))
    }
}

/// COM 单元 RAII 守卫：无论从哪个分支提前返回都成对释放
struct ComApartment {
    initialized: bool,
}

impl ComApartment {
    fn new() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Self {
            initialized: hr.is_ok(),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

struct ScoredCandidate {
    elem: IUIAutomationElement,
    val_pattern: IUIAutomationValuePattern,
    score: i32,
    name: String,
    class_name: String,
    auto_id: String,
    val: String,
    rect: RECT,
}

// ══════════════════════════════════════════════════════════════════════
// WPS 注入的**操作级**去重与 generation 核验(审计报告 P0-07 风险 6)
//
// 剪贴板 lease 只能串行化「使用剪贴板的短区间」,挡不住两个线程都走 UIA
// `SetValue` 后继续对同一个对话框操作(重复 Enter / 导航到错误目录),
// 也挡不住延迟 650ms 的文件名恢复干扰后一次操作 —— 那个区间里没有剪贴板事务。
//
// 因此另设两层防护:
// 1 **操作级独占 lease**:同一时刻只允许一次 WPS 注入走完 UIA 查询与全局输入
//   (不排队、不等待,拿不到立即零动作拒绝)。取全局唯一而非逐对话框一份:
//   `bring_to_front` 与 `keybd_event` 操作的是**进程级**前台与键盘,两个不同
//   对话框同时注入同样会互相抢夺前台并把全局按键发给错误窗口。
// 2 **对话框 generation**:每次注入开始就把该身份指纹的代次 +1;延迟的文件名
//   恢复在写入前复核代次,代次已变(有新注入接管)则放弃 —— 旧恢复绝不会覆盖
//   新注入的结果,也不需要把 lease 握过整个 650ms 延迟窗口。
//
// 身份指纹由 HWND + PID + 进程创建时间 + 类名组成:HWND 被复用 / 进程重启后
// 指纹自动不同,不会被旧代次误伤。
// ══════════════════════════════════════════════════════════════════════

#[derive(Default)]
struct WpsInjectState {
    /// 当前持有 lease 的对话框指纹(空闲为 `None`)
    holder: Option<String>,
    /// 各对话框最近一次注入的代次(让延迟的旧恢复自动失效)
    generations: std::collections::HashMap<String, u64>,
}

fn wps_inject_state() -> &'static std::sync::Mutex<WpsInjectState> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<WpsInjectState>> =
        std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(WpsInjectState::default()))
}

/// WPS 对话框操作级独占 lease。持有期间不得再有第二次注入。
#[derive(Debug)]
struct WpsInjectLease {
    fingerprint: String,
}

impl WpsInjectLease {
    /// 非阻塞获取:已有注入在进行时返回 `None`,**不排队也不等待**。
    fn try_acquire(identity: &WindowIdentity) -> Option<Self> {
        Self::try_acquire_fingerprint(&identity.fingerprint())
    }

    /// 去重与代次的**唯一实现**:生产路径与测试共用,避免测试测到复制品
    /// (否则把去重改坏也测不出红)。
    fn try_acquire_fingerprint(fingerprint: &str) -> Option<Self> {
        let mut state = wps_inject_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.holder.is_some() {
            return None;
        }
        state.holder = Some(fingerprint.to_string());
        // 代次表只需保留最近活跃的少数对话框,避免随进程寿命无界增长
        if state.generations.len() > 16 {
            state.generations.clear();
        }
        let generation = state
            .generations
            .entry(fingerprint.to_string())
            .or_insert(0);
        *generation = generation.wrapping_add(1);
        Some(Self {
            fingerprint: fingerprint.to_string(),
        })
    }

    /// 该对话框当前的注入代次(供延迟恢复复核)。
    fn generation(&self) -> u64 {
        current_generation(&self.fingerprint)
    }

    /// 测试专用:按指纹获取 lease(不涉及真实窗口句柄)。
    ///
    /// **复用** [`Self::try_acquire_fingerprint`],因此行为用例钉住的就是生产实现。
    #[cfg(test)]
    fn begin_for_test(fingerprint: &str) -> Option<Self> {
        Self::try_acquire_fingerprint(fingerprint)
    }
}

impl Drop for WpsInjectLease {
    fn drop(&mut self) {
        let mut state = wps_inject_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 只清除自己登记的那一份,避免身份变化后的新持有者被旧 guard 抹掉
        if state.holder.as_deref() == Some(self.fingerprint.as_str()) {
            state.holder = None;
        }
    }
}

/// 按身份指纹读取当前注入代次;从未注入过返回 0。
fn current_generation(fingerprint: &str) -> u64 {
    let state = wps_inject_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.generations.get(fingerprint).copied().unwrap_or(0)
}

/// 模拟快捷键 Ctrl + A 全选（仅在确认目标窗口为前台后调用）
fn simulate_ctrl_a() {
    unsafe {
        keybd_event(VK_CONTROL.0 as u8, 0, Default::default(), 0);
        keybd_event(b'A', 0, Default::default(), 0);
        thread::sleep(Duration::from_millis(20));
        keybd_event(b'A', 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// 模拟快捷键 Ctrl + V 粘贴（仅在确认目标窗口为前台后调用）
fn simulate_ctrl_v() {
    unsafe {
        keybd_event(VK_CONTROL.0 as u8, 0, Default::default(), 0);
        keybd_event(b'V', 0, Default::default(), 0);
        thread::sleep(Duration::from_millis(20));
        keybd_event(b'V', 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// 模拟按下回车键 Enter（仅在路径写入并回读一致后调用）
fn simulate_enter() {
    unsafe {
        keybd_event(VK_RETURN.0 as u8, 0, Default::default(), 0);
        thread::sleep(Duration::from_millis(20));
        keybd_event(VK_RETURN.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// 全局输入放行的纯判定：三个条件必须同时成立。
///
/// 抽成纯函数是为了单测能直接审视这项 AND 语义，而不是“看代码觉得对”。
fn guarded_input_allowed(identity_ok: bool, foreground_ok: bool, focus_ok: bool) -> bool {
    identity_ok && foreground_ok && focus_ok
}

/// 全局输入动作的校验上下文（审计报告 P0-02）。
///
/// WPS Qt 自绘对话框会丢弃 UIA `SetValue`，因此必须保留物理键鼠；
/// 代价是每一个按键/点击之前都要重新满足：
/// ① 窗口身份未变（HWND + PID + 创建时间 + 类名）；
/// ② 目标窗口仍是前台窗口；
/// ③ 目标编辑框确实持有键盘焦点。
struct InputContext<'a> {
    identity: &'a WindowIdentity,
    elem: &'a IUIAutomationElement,
}

impl InputContext<'_> {
    fn ready(&self, action: &str) -> bool {
        let identity_ok = self.identity.verify();
        let foreground_ok = identity_ok && self.identity.is_foreground();
        let focus_ok = unsafe {
            self.elem
                .CurrentHasKeyboardFocus()
                .map(|focused| focused.as_bool())
                .unwrap_or(false)
        };

        if !guarded_input_allowed(identity_ok, foreground_ok, focus_ok) {
            log_debug(&format!(
                "UIA_GUARD: {} 前置校验未通过 (identity={} foreground={} focus={})，取消全局输入",
                action, identity_ok, foreground_ok, focus_ok
            ));
            return false;
        }
        true
    }
}

/// 受保护的 Ctrl+A：先复核身份/前台/焦点，再发送全局按键
fn simulate_ctrl_a_checked(ctx: &InputContext<'_>) -> bool {
    if !ctx.ready("Ctrl+A") {
        return false;
    }
    simulate_ctrl_a();
    true
}

/// 受保护的 Ctrl+V：同上
fn simulate_ctrl_v_checked(ctx: &InputContext<'_>) -> bool {
    if !ctx.ready("Ctrl+V") {
        return false;
    }
    simulate_ctrl_v();
    true
}

/// 受保护的 Enter（导航终点，发送前必须先做一次完整终审）
fn simulate_enter_checked(ctx: &InputContext<'_>) -> bool {
    if !ctx.ready("Enter") {
        return false;
    }
    simulate_enter();
    true
}

/// 智能寻找 WPS 文件对话框中的主“文件名”编辑框元素与对应 ValuePattern
fn find_wps_edit_element(
    uia: &IUIAutomation,
    dialog_hwnd: HWND,
) -> Option<(IUIAutomationElement, IUIAutomationValuePattern)> {
    unsafe {
        let mut dlg_rect = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(dialog_hwnd, &mut dlg_rect);
        let dlg_height = dlg_rect.bottom - dlg_rect.top;

        let root = uia.ElementFromHandle(dialog_hwnd).ok()?;
        let true_cond = uia.CreateTrueCondition().ok()?;
        let elements = root.FindAll(TreeScope_Descendants, &true_cond).ok()?;

        let count = elements.Length().ok()?;
        let mut candidates: Vec<ScoredCandidate> = Vec::new();

        for i in 0..count {
            if let Ok(elem) = elements.GetElement(i) {
                // 1. 必须支持 ValuePattern
                let pattern_unk = match elem.GetCurrentPattern(UIA_ValuePatternId) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let val_pattern = match pattern_unk.cast::<IUIAutomationValuePattern>() {
                    Ok(vp) => vp,
                    Err(_) => continue,
                };

                let name = elem
                    .CurrentName()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                let class_name = elem
                    .CurrentClassName()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                let auto_id = elem
                    .CurrentAutomationId()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                let val = val_pattern
                    .CurrentValue()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                let ctrl_type = elem.CurrentControlType().map(|c| c.0).unwrap_or(0);
                let elem_rect = elem.CurrentBoundingRectangle().unwrap_or_default();

                let mut score = 0i32;

                // 严重负分项（排除 ComboBox 容器、顶部搜索框、Filter 框、按钮、标签等干扰项）
                let lower_name = name.to_lowercase();
                let lower_id = auto_id.to_lowercase();
                let lower_class = class_name.to_lowercase();

                // 核心关键：ComboBox 是组合框外壳，绝不能作为输入目标（必须选其内层的 QLineEdit）！
                if lower_class.contains("combobox") || ctrl_type == 50003 {
                    score -= 800;
                }
                if lower_class.contains("button")
                    || lower_class.contains("label")
                    || lower_class.contains("splitter")
                    || lower_class.contains("scrollbar")
                    || lower_class.contains("slider")
                    || lower_class.contains("progressbar")
                {
                    score -= 800;
                }
                if lower_name.contains("搜索")
                    || lower_name.contains("search")
                    || lower_id.contains("search")
                {
                    score -= 800;
                }

                // 绝对最高优先级：真正的文本编辑控件 QLineEdit
                if class_name == "QLineEdit" || class_name == "kd::KDTextField" {
                    score += 500;
                } else if ctrl_type == 50004 {
                    // UIA_EditControlTypeId
                    score += 200;
                }

                // 几何位置加分：另存为对话框中，真正的文件名输入框必定在界面垂直中下半部
                if dlg_height > 100 {
                    let rel_top = elem_rect.top - dlg_rect.top;
                    if rel_top > dlg_height * 4 / 10 {
                        score += 200;
                    }
                    if rel_top < 100 {
                        score -= 100; // 紧贴顶部的通常是搜索栏或路径栏
                    }
                }

                // 内容特征加分（已有默认文件名或扩展名）
                if !val.trim().is_empty() {
                    if val.contains('.') {
                        score += 150;
                    } else {
                        score += 30;
                    }
                }

                // 名称语义辅助加分
                if name.contains("文件名")
                    || name.contains("File name")
                    || auto_id.contains("fileName")
                    || auto_id.contains("FileName")
                {
                    score += 100;
                }

                log_debug(&format!(
                    "UIA_EVAL[{}]: score={} {} val={} top={}",
                    i,
                    score,
                    SafeUiaMetadata::of(&name, &class_name, &auto_id),
                    TextFingerprint::of(&val),
                    elem_rect.top
                ));

                candidates.push(ScoredCandidate {
                    elem,
                    val_pattern,
                    score,
                    name,
                    class_name,
                    auto_id,
                    val,
                    rect: elem_rect,
                });
            }
        }

        if candidates.is_empty() {
            log_debug("UIA_EVAL: No ValuePattern element found");
            return None;
        }

        // 选取评分最高的候选元素
        candidates.sort_by(|a, b| b.score.cmp(&a.score));
        let best = candidates.remove(0);

        // 防御性安全检查：真正的文件名编辑框（QLineEdit / kd::KDTextField / Edit）得分至少在 200 分以上
        // 若最高分依然过低（例如 <= 100），说明窗口内根本不存在合法的文件名编辑框，绝不盲目注入
        if best.score <= 100 {
            log_debug(&format!(
                "UIA_REJECT: Best candidate score too low ({}) {}",
                best.score,
                SafeUiaMetadata::of(&best.name, &best.class_name, &best.auto_id)
            ));
            return None;
        }

        log_debug(&format!(
            "UIA_CHOSEN: {} val={} score={} rect={:?}",
            SafeUiaMetadata::of(&best.name, &best.class_name, &best.auto_id),
            TextFingerprint::of(&best.val),
            best.score,
            best.rect
        ));

        Some((best.elem, best.val_pattern))
    }
}

/// 使编辑框获得键盘焦点。
///
/// 优先使用 UIA 语义聚焦，并用 `CurrentHasKeyboardFocus()` 确认；
/// 只有在语义聚焦未被确认时才补一次物理点击，且点击前必须复核：
/// ① 窗口身份未变 ② 目标窗口仍是前台 ③ 控件矩形有效（现场重新读取，避免陈旧坐标误点）。
unsafe fn focus_edit_element(identity: &WindowIdentity, elem: &IUIAutomationElement) -> bool {
    unsafe {
        // 1) 语义聚焦 + 焦点确认（无任何全局输入）
        if elem.SetFocus().is_ok()
            && elem
                .CurrentHasKeyboardFocus()
                .map(|focused| focused.as_bool())
                .unwrap_or(false)
        {
            return true;
        }

        // 2) 物理点击兜底：任何前置条件不满足都直接放弃，宁可不注入也不误点
        if !identity.verify() || !identity.is_foreground() {
            log_debug("UIA_FOCUS: 身份或前台状态不符，跳过物理点击");
            return false;
        }

        let rect = elem.CurrentBoundingRectangle().unwrap_or_default();
        if rect.right <= rect.left || rect.bottom <= rect.top {
            log_debug("UIA_FOCUS: 控件矩形无效，跳过物理点击");
            return false;
        }

        let mut orig_pt = POINT::default();
        if GetCursorPos(&mut orig_pt).is_err() {
            return false;
        }
        let cx = (rect.left + rect.right) / 2;
        let cy = (rect.top + rect.bottom) / 2;
        if SetCursorPos(cx, cy).is_err() {
            return false;
        }
        thread::sleep(Duration::from_millis(20));

        // 移动光标后前台可能已变化（例如用户切屏），按下鼠标前再核验一次
        if !identity.is_foreground() {
            let _ = SetCursorPos(orig_pt.x, orig_pt.y);
            log_debug("UIA_FOCUS: 点击前前台窗口已变化，放弃物理点击");
            return false;
        }

        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        thread::sleep(Duration::from_millis(20));
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
        let _ = SetCursorPos(orig_pt.x, orig_pt.y);
        thread::sleep(Duration::from_millis(40));

        // 点击兜底后必须重新确认编辑框确实拿到了键盘焦点，
        // 否则不得视为“已聚焦”，绝不据此发送后续全局按键（报告 P0-02）
        if !identity.verify() {
            log_debug("UIA_FOCUS: 物理点击后窗口身份已变化，判定聚焦失败");
            return false;
        }
        if !elem
            .CurrentHasKeyboardFocus()
            .map(|focused| focused.as_bool())
            .unwrap_or(false)
        {
            log_debug("UIA_FOCUS: 物理点击后编辑框仍未获得键盘焦点，判定聚焦失败");
            return false;
        }
        true
    }
}

/// 路径文本比较：忽略大小写、统一分隔符、忽略结尾多余分隔符与首尾空白
fn normalize_path_text(path: &str) -> String {
    let mut s = path.trim().replace('/', "\\").to_lowercase();
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

fn paths_equal(a: &str, b: &str) -> bool {
    normalize_path_text(a) == normalize_path_text(b)
}

/// 将目标目录写入编辑框并**回读校验**。
///
/// 任一环节失败或回读不一致都返回 false —— 调用方据此绝不发送 Enter，
/// 从根上避免“路径没写进去却按了回车”造成的错误导航或误提交。
unsafe fn write_path_and_verify(
    pattern: &IUIAutomationValuePattern,
    target: &str,
    ctx: &InputContext<'_>,
) -> bool {
    unsafe {
        // 0. 进入写入流程前先做一次前置校验（报告 P0-02）
        if !ctx.ready("写入路径") {
            return false;
        }

        // 路径 1（优先）：UIA `ValuePattern::SetValue` —— **完全不经过系统剪贴板**。
        // 审计需求 `9549648` §3.4.4：「UIA SetValue 可用时优先使用，不经过剪贴板」。
        // 必须检查返回值 + 回读校验，绝不以“调用过”当成功。
        match pattern.SetValue(&BSTR::from(target)) {
            Ok(()) => {
                if wait_readback_match(pattern, target) {
                    return true;
                }
                log_debug("UIA_INJECT: SetValue 已调用但回读不一致，降级使用剪贴板注入");
            }
            Err(err) => {
                log_debug(&format!(
                    "UIA_INJECT: SetValue 返回失败({:?})，降级使用剪贴板注入",
                    err
                ));
            }
        }

        // 路径 2（兜底）：受保护的剪贴板临时注入（统一事务 API + 严格 sequence 归属）。
        // 每次 Ctrl+A / Ctrl+V 之前都重新核验身份、前台与控件焦点；
        // 任一环节不满足即立即中止，绝不把全局按键发给错误窗口。
        // 归属无法证明（外部改写 / 序列号不可判定 / 事件史不可证明）时，临时写入直接失败，
        // 闭包不会执行 → **不发送 Ctrl+V**，也不发送后续 Enter。
        if !ctx.ready("剪贴板兜底注入") {
            return false;
        }
        let mut sequence_ok = false;
        let clipboard_result =
            crate::win32::clipboard::with_temporary_clipboard_text(target, || {
                if !simulate_ctrl_a_checked(ctx) {
                    return;
                }
                thread::sleep(Duration::from_millis(30));
                if !simulate_ctrl_v_checked(ctx) {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
                sequence_ok = true;
            });

        let clipboard_injected = match clipboard_result {
            Ok(()) if sequence_ok => true,
            Ok(()) => {
                log_debug("UIA_INJECT: 物理按键前置校验未通过，本次剪贴板注入已中止");
                false
            }
            Err(err) => {
                log_debug(&format!(
                    "UIA_INJECT: 剪贴板安全守卫拒绝或恢复失败({:?})，本次注入已放弃",
                    err
                ));
                false
            }
        };

        clipboard_injected && wait_readback_match(pattern, target)
    }
}

/// 回读编辑框当前值并与目标路径比对。
///
/// 最多探测 3 次（容忍 Qt 控件值更新略有延迟），但始终以“确实读到一致”
/// 作为发送 Enter 的唯一前提——回读失败或始终不一致一律返回 false。
unsafe fn wait_readback_match(pattern: &IUIAutomationValuePattern, target: &str) -> bool {
    unsafe {
        for attempt in 0..3 {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(60));
            }
            match pattern.CurrentValue() {
                Ok(value) => {
                    let current = value.to_string();
                    if paths_equal(&current, target) {
                        return true;
                    }
                    log_debug(&format!(
                        "UIA_VERIFY[{}]: 回读不一致 current={} target={}",
                        attempt,
                        TextFingerprint::of(&current),
                        redact_path_structure(target)
                    ));
                }
                Err(err) => {
                    log_debug(&format!("UIA_VERIFY[{}]: 回读失败 {:?}", attempt, err));
                }
            }
        }
        false
    }
}

/// 执行一次完整的 WPS 注入（在工作线程内运行）。
///
/// 返回**真实完成结果**：仅当
/// ① 全程窗口身份复核通过、② 目标窗口确实处于前台、③ 路径写入后回读一致，
/// 才发送 Enter 并返回 true；否则零全局输入地放弃。
unsafe fn run_wps_injection(dialog_hwnd: HWND, target_folder: &str) -> bool {
    unsafe {
        let _com = ComApartment::new();

        let Some(identity) = WindowIdentity::capture(dialog_hwnd) else {
            log_debug("UIA_REJECT: 无法确立目标窗口身份(窗口已失效)");
            return false;
        };

        // 审计报告 P0-07 风险 6:**操作级去重**。必须在任何 UIA 查询 / 全局输入之前取得,
        // 否则同一对话框上的两次注入会各自 SetValue 并各发一次 Enter(导航到错误目录)。
        // 拿不到表示同一对话框已有注入在进行:立即返回,**零 UIA 查询、零全局输入**。
        let Some(lease) = WpsInjectLease::try_acquire(&identity) else {
            log_debug("UIA_REJECT: 已有一次 WPS 注入在进行,本次请求被拒绝(零全局输入)");
            return false;
        };

        let uia = match CoCreateInstance::<_, IUIAutomation>(
            &CUIAutomation,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            Ok(uia) => uia,
            Err(err) => {
                log_debug(&format!("UIA_ERR: CoCreateInstance failed: {:?}", err));
                return false;
            }
        };

        // 1. 前台化并以 GetForegroundWindow 为准核验；失败即零输入放弃
        if !identity.bring_to_front() {
            log_debug("UIA_REJECT: 未能取得目标窗口前台焦点，放弃注入（零全局输入）");
            return false;
        }

        // 2. 定位文件名编辑框
        let Some((elem, val_pattern)) = find_wps_edit_element(&uia, dialog_hwnd) else {
            log_debug("UIA_ERR: find_wps_edit_element failed");
            return false;
        };

        // 3. 定位期间窗口可能已关闭/复用/失去前台：任何变化立即取消
        if !identity.verify() || !identity.is_foreground() {
            log_debug("UIA_REJECT: 定位后窗口身份或前台状态已变化，放弃注入");
            return false;
        }

        // 4. 判断是否需要恢复原文件名（用户已敲入文件名时才需要）
        let old_text = val_pattern
            .CurrentValue()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let should_restore_name = !old_text.trim().is_empty()
            && !std::path::Path::new(old_text.trim()).is_dir()
            && !old_text.ends_with('\\');

        log_debug(&format!(
            "UIA_INJECT_PRE: old_text={}, should_restore={}",
            TextFingerprint::of(&old_text),
            should_restore_name
        ));

        // 5. 聚焦编辑框（语义聚焦优先，物理点击前复核身份与前台）
        if !focus_edit_element(&identity, &elem) {
            log_debug("UIA_REJECT: 无法确认编辑框焦点，放弃注入（零全局输入）");
            return false;
        }

        let ctx = InputContext {
            identity: &identity,
            elem: &elem,
        };

        // 6. 写入目标目录并回读校验；不一致绝不发送 Enter
        if !write_path_and_verify(&val_pattern, target_folder, &ctx) {
            log_debug("UIA_REJECT: 路径写入后回读不一致，绝不发送 Enter");
            return false;
        }

        // 7. Enter 是导航终点：发送前再做一次完整终审（身份 + 前台 + 控件焦点），
        //    任一不满足即取消，绝不把全局 Enter 发给其他应用（报告 P0-02）
        if !simulate_enter_checked(&ctx) {
            log_debug("UIA_REJECT: Enter 终审未通过，未发送任何全局输入");
            return false;
        }
        log_debug("UIA_INJECT: 路径已写入并回读一致，Enter 终审通过，触发目录导航");

        // 8. 优雅恢复原文件名(尽力而为,不影响本次注入结果)
        if should_restore_name {
            spawn_restore_original_name(identity, old_text, lease.generation());
        }

        true
    }
}

/// 目录导航完成后把原文件名恢复回编辑框(尽力而为)。
///
/// 审计报告 P0-07 风险 6:恢复在 650ms 后执行,该窗口期内用户可能已再次触发注入
/// (新注入会把该对话框的 generation +1)。因此写入前必须复核代次:
/// 代次变化即放弃 —— 旧恢复绝不覆盖新注入的结果。
fn spawn_restore_original_name(identity: WindowIdentity, saved_name: String, generation: u64) {
    thread::spawn(move || unsafe {
        // 等待 WPS Qt 视图载入新目录并刷新
        thread::sleep(Duration::from_millis(650));

        // generation 核验:已有新注入接管该对话框则放弃(不写编辑框、不发按键)
        if current_generation(&identity.fingerprint()) != generation {
            log_debug("UIA_RESTORE_SKIP: 对话框 generation 已变化,放弃恢复文件名");
            return;
        }

        if !identity.verify() || !identity.is_foreground() {
            log_debug("UIA_RESTORE_SKIP: 窗口身份或前台状态已变化，放弃恢复文件名");
            return;
        }

        let _com = ComApartment::new();
        let Ok(uia) =
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        else {
            return;
        };
        let Some((elem, pattern)) = find_wps_edit_element(&uia, identity.hwnd()) else {
            return;
        };

        let current = pattern
            .CurrentValue()
            .map(|b| b.to_string())
            .unwrap_or_default();
        log_debug(&format!(
            "UIA_RESTORE_CHECK: cur_val={}, saved_name={}",
            TextFingerprint::of(&current),
            TextFingerprint::of(&saved_name)
        ));

        // 智能输入防覆盖保护：用户在等待窗口期内已键入新文件名时严禁覆盖
        if !current.is_empty()
            && current != saved_name
            && !current.ends_with('\\')
            && !current.ends_with('/')
        {
            log_debug("UIA_RESTORE_SKIP: 检测到用户已手动键入新文件名，放弃自动恢复以保护输入");
            return;
        }

        if !focus_edit_element(&identity, &elem) {
            log_debug("UIA_RESTORE_SKIP: 无法确认编辑框焦点，放弃恢复文件名");
            return;
        }

        let ctx = InputContext {
            identity: &identity,
            elem: &elem,
        };
        let mut sequence_ok = false;
        let clipboard_ok =
            crate::win32::clipboard::with_temporary_clipboard_text(&saved_name, || {
                if !simulate_ctrl_a_checked(&ctx) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
                if !simulate_ctrl_v_checked(&ctx) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
                sequence_ok = simulate_ctrl_a_checked(&ctx); // 再次全选原文件名，方便用户直接改名
            })
            .is_ok();
        let restored = clipboard_ok && sequence_ok;

        log_debug(&format!(
            "UIA_RESTORE_DONE: restore_requested={} clipboard_ok={}",
            TextFingerprint::of(&saved_name),
            restored
        ));
    });
}

/// 安全将目标目录注入到 WPS Office 专属对话框中。
///
/// 返回 true 表示注入任务已受理（工作线程已启动）；**真实完成结果**通过
/// `on_done(bool)` 回传——启动线程不等于注入成功（审计报告 P0-02 要求）。
pub fn inject_path_to_wps_dialog(
    dialog_hwnd: HWND,
    target_path: &str,
    on_done: Option<crate::dialog::injector::InjectDone>,
) -> bool {
    let clean_path = target_path.trim().trim_end_matches('\\');
    if !std::path::Path::new(clean_path).is_dir() {
        if let Some(done) = on_done {
            done(false);
        }
        return false;
    }

    let folder_with_slash = format!("{}\\", clean_path);
    let raw_hwnd = dialog_hwnd.0 as usize;

    log_debug(&format!(
        "UIA_INJECT_START: target_folder={} hwnd=0x{:X}",
        redact_path_structure(&folder_with_slash),
        raw_hwnd
    ));

    // 将注入逻辑放入单独线程，避免任何 UIA 查询等待阻塞调用线程（可能是宿主消息线程）
    thread::spawn(move || {
        let injected = unsafe { run_wps_injection(HWND(raw_hwnd as *mut _), &folder_with_slash) };
        if let Some(done) = on_done {
            done(injected);
        }
    });

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path_text_ignores_case_and_trailing_separator() {
        assert_eq!(normalize_path_text(r"C:\Users\Test\"), r"c:\users\test");
        assert_eq!(normalize_path_text("  C:/Users/Test  "), r"c:\users\test");
        assert_eq!(normalize_path_text(r"C:\"), r"c:\");
        assert_eq!(normalize_path_text(r"D:\a\b\"), r"d:\a\b");
    }

    #[test]
    fn test_paths_equal_accepts_wps_style_value_variants() {
        // WPS 回读通常保留尾部反斜杠，我们按“忽略尾部分隔符/大小写/正反斜杠”比对
        assert!(paths_equal(r"C:\Users\Test\", r"C:\Users\Test"));
        assert!(paths_equal(r"c:/users/test", r"C:\Users\Test\"));
        assert!(!paths_equal(r"C:\Users\Test", r"C:\Users\Other"));
        // 前缀相同但并非同一目录：必须判为不一致（避免误按 Enter）
        assert!(!paths_equal(r"C:\Users\Test2", r"C:\Users\Test"));
        assert!(!paths_equal("", r"C:\Users\Test"));
    }

    #[test]
    fn test_guarded_input_requires_all_three_conditions() {
        // 身份 + 前台 + 控件焦点三项必须全部成立才允许发出全局输入（报告 P0-02）
        assert!(guarded_input_allowed(true, true, true));
        assert!(!guarded_input_allowed(false, true, true));
        assert!(!guarded_input_allowed(true, false, true));
        assert!(!guarded_input_allowed(true, true, false));
        assert!(!guarded_input_allowed(false, false, false));
    }

    /// 审计需求 `9549648` §3.4.4 / §3.5 用例 6：WPS 写入路径必须优先走 UIA `SetValue`
    /// （不经过系统剪贴板），且**写入未校验通过时绝不发送后续 Enter**。
    #[test]
    fn test_wps_write_path_prefers_setvalue_and_never_enters_on_failure() {
        const SOURCE: &str = include_str!("uia.rs");

        // 1) `write_path_and_verify` 内部：SetValue 必须排在剪贴板注入之前
        let start = SOURCE
            .find("unsafe fn write_path_and_verify")
            .expect("write_path_and_verify 必须存在");
        let rest = &SOURCE[start..];
        let end = rest
            .find("unsafe fn wait_readback_match")
            .expect("函数体必须能被切分");
        let body = &rest[..end];
        let set_value = body.find("SetValue(").expect("必须保留 UIA SetValue 路径");
        let clipboard = body
            .find("with_temporary_clipboard_text")
            .expect("必须保留剪贴板兜底路径");
        assert!(
            set_value < clipboard,
            "UIA SetValue 必须优先于剪贴板注入（§3.4.4：可用时优先且不经过剪贴板）"
        );

        // 2) `run_wps_injection` 内部：Enter 必须在写入校验之后，且校验失败必须提前 return
        let run_start = SOURCE
            .find("unsafe fn run_wps_injection")
            .expect("run_wps_injection 必须存在");
        let run_body = &SOURCE[run_start..];
        let verify_call = run_body
            .find("write_path_and_verify(&val_pattern")
            .expect("必须调用写入校验");
        let enter = run_body
            .find("simulate_enter_checked(")
            .expect("必须存在 Enter 终审");
        assert!(verify_call < enter, "Enter 绝不能排在写入校验之前");
        assert!(
            run_body[verify_call..enter].contains("return false"),
            "写入校验失败必须提前 return，绝不发送 Enter"
        );
        // 剪贴板注入的兜底不允许静默降级成 Enter
        assert!(
            body.contains("clipboard_injected && wait_readback_match(pattern, target)"),
            "剪贴板注入必须经回读校验后才算成功"
        );
    }

    /// 审计报告 S-LOG：UIA 侧的正文指纹与源码守卫。
    ///
    /// 1. 指纹只暴露长度/类别/会话内关联 ID，绝不包含正文；
    /// 2. 会话内同一文本哈希稳定、不同文本可区分（能把一次故障的多条日志串起来）；
    /// 3. 生产代码不得再把正文原样拼进日志（否则守卫变红）。
    #[test]
    fn test_sensitive_uia_logs_never_emit_raw_text() {
        // --- 指纹本身的语义 ---
        let dir = TextFingerprint::of(r"D:\客户项目\甲方合同\");
        let file = TextFingerprint::of(r"D:\客户项目\甲方合同\报价单.docx");
        let empty = TextFingerprint::of("   ");
        assert_eq!(dir.kind, "dir");
        assert_eq!(file.kind, "file");
        assert_eq!(empty.kind, "empty");
        assert_eq!(empty.len, 3, "长度按字符计（含空白），用于区分空与未填");
        assert_eq!(
            TextFingerprint::of("同一个名字.docx").session_id,
            TextFingerprint::of("同一个名字.docx").session_id,
            "同一会话内同一文本的关联 ID 必须稳定"
        );
        assert_ne!(
            TextFingerprint::of("a.docx").session_id,
            TextFingerprint::of("b.docx").session_id,
            "不同文本必须可区分"
        );

        let rendered = format!("{} {} {}", dir, file, empty);
        for secret in ["客户项目", "甲方合同", "报价单"] {
            assert!(
                !rendered.contains(secret),
                "指纹不得泄露正文（命中 `{}`）：{}",
                secret,
                rendered
            );
        }

        // --- 源码守卫：生产代码不得再把正文原样写进日志 ---
        const SOURCE: &str = include_str!("uia.rs");
        let production = &SOURCE[..SOURCE.find("\nmod tests {").expect("必须存在测试模块")];
        for key in [
            "old_text",
            "cur_val",
            "saved_name",
            "val",
            "current",
            "target",
            "target_folder",
            // S-LOG-A：UIA 外部元数据（第三方控件可能把文件名/路径放进这些字段）
            "name",
            "cls",
            "id",
            "class_name",
            "auto_id",
        ] {
            let raw = format!("{}='{{}}'", key);
            assert!(
                !production.contains(&raw),
                "UIA 调试日志禁止记录正文原文（命中 `{}`）",
                raw
            );
        }
        assert!(
            production.contains("TextFingerprint::of(")
                && production.contains("redact_path_structure("),
            "敏感位置必须经过共享脱敏入口（指纹 / 路径结构骨架）"
        );
        // S-LOG-A：三条 UIA 分支（评估 / 拒绝 / 选中）都必须走统一安全格式化器
        assert_eq!(
            production.matches("SafeUiaMetadata::of(").count(),
            3,
            "UIA_EVAL / UIA_REJECT / UIA_CHOSEN 三条日志分支都必须使用 SafeUiaMetadata"
        );
        assert!(
            production.contains("SAFE_UIA_CLASS_NAMES") && production.contains("SAFE_UIA_AUTO_IDS"),
            "class_name / auto_id 必须经内置白名单过滤后才允许原样输出"
        );
    }

    /// 审计报告 S-LOG-A：UIA 外部元数据不得泄露业务文本。
    ///
    /// `name` 恒为指纹；非白名单（或超长 / 非 ASCII）的 `class_name`、`auto_id` 也必须
    /// 降级为指纹；白名单内的通用类名 / 语义标签仍原样保留，排障信息不丢失。
    #[test]
    fn test_uia_metadata_formatter_never_leaks_business_text() {
        let sensitive = SafeUiaMetadata::of(
            r"D:\客户项目\甲方合同\报价单.docx",
            r"D:\客户项目\甲方合同",
            r"客户编号-8848",
        );
        let rendered = sensitive.to_string();
        for secret in ["客户项目", "甲方合同", "报价单", "客户编号", "8848"] {
            assert!(
                !rendered.contains(secret),
                "UIA 元数据格式化器不得泄露业务文本（命中 `{}`）：{}",
                secret,
                rendered
            );
        }
        assert!(
            rendered.contains("name=len="),
            "name 必须输出指纹：{rendered}"
        );

        // 白名单内的通用控件标签必须原样保留（排障信息不丢）
        let generic = SafeUiaMetadata::of("文件名(N):", "QLineEdit", "fileName");
        let rendered_generic = generic.to_string();
        assert!(rendered_generic.contains("cls=QLineEdit"));
        assert!(rendered_generic.contains("id=fileName"));
        assert!(
            !rendered_generic.contains("文件名"),
            "name 即使看起来是通用标签也必须只输出指纹：{rendered_generic}"
        );

        // 超长 / 非 ASCII 的类名与 ID 必须降级为指纹
        let long_class = "A".repeat(64);
        let non_ascii = "控件类名";
        let degraded = SafeUiaMetadata::of("", &long_class, non_ascii).to_string();
        assert!(!degraded.contains(&long_class));
        assert!(!degraded.contains(non_ascii));
        assert!(degraded.contains("cls=len=64"));
    }

    /// 构造一个**测试专用**身份指纹(不涉及真实窗口,仅验证去重/代次语义)。
    fn test_fingerprint(tag: &str) -> String {
        format!("test:{tag}")
    }

    /// 串行化「碰 WPS 注入 lease 全局状态」的用例。
    ///
    /// 该状态是进程级静态(生产语义如此),并行执行时两个用例会互抢,
    /// 因此必须串行;锁中毒也不掩盖其他用例的真实结果。
    fn wps_inject_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 审计报告 P0-07 风险 6:WPS 注入的**操作级去重**必须严格互斥且非阻塞。
    ///
    /// 这是剪贴板 lease 盖不住的区间(两次注入都走 UIA `SetValue` 时不碰剪贴板),
    /// 两个线程同时通过会造成重复 Enter / 导航到错误目录。
    #[test]
    fn test_wps_inject_lease_is_exclusive_non_blocking_and_independent_of_clipboard() {
        let _guard = wps_inject_test_lock();
        // 同时拿剪贴板测试锁:保证此刻确实没有别的事务在跑(否则下面的断言不稳定)
        let _clip_guard = crate::win32::clipboard::clipboard_test_lock();

        let fp = test_fingerprint("wps_dedup");
        let first = WpsInjectLease::begin_for_test(&fp).expect("空闲时必须能拿到注入 lease");
        assert!(
            WpsInjectLease::begin_for_test(&fp).is_none(),
            "已有注入在跑时第二次获取必须**立即**失败(不排队、不等待)"
        );
        // 与剪贴板 lease 完全独立:注入进行中仍可开启剪贴板事务
        // (注入需要剪贴板兜底时不必自拒绍),反之亦然。
        assert!(
            !crate::win32::clipboard::is_transaction_in_flight(),
            "WPS 注入 lease 不得影响剪贴板事实来源"
        );
        {
            let mut tx = crate::win32::clipboard::ClipboardTransaction::begin()
                .expect("注入期间仍应能开启剪贴板事务");
            assert!(
                WpsInjectLease::begin_for_test(&fp).is_none(),
                "剪贴板事务进行中仍不得再开第二次注入(两把锁互不代偿)"
            );
            let _ = tx.restore_if_still_owned();
        }
        drop(first);
        let second = WpsInjectLease::begin_for_test(&fp).expect("释放后必须能再次获取");
        drop(second);
    }

    /// 审计报告 P0-07 风险 6:延迟恢复必须按 **generation** 复核,旧恢复不得覆盖新结果。
    #[test]
    fn test_delayed_restore_is_invalidated_by_newer_injection_generation() {
        let _guard = wps_inject_test_lock();
        let fp = test_fingerprint("wps_generation");

        // 第一次注入:取得 lease,记下代次后立即释放(模拟注入主体已结束)
        let lease = WpsInjectLease::begin_for_test(&fp).expect("首次注入必须能拿到 lease");
        let generation_of_first = lease.generation();
        drop(lease);
        assert_eq!(
            current_generation(&fp),
            generation_of_first,
            "旧恢复自己的代次在当前代次未变时必须匹配(否则恢复永远不会执行)"
        );

        // 第二次注入接管同一对话框:代次提升,旧恢复随之失效
        let second = WpsInjectLease::begin_for_test(&fp).expect("前次已释放,必须能再次获取");
        let generation_of_second = second.generation();
        drop(second);
        assert_ne!(
            generation_of_first, generation_of_second,
            "新注入必须提升代次,使上一次的延迟恢复不再匹配"
        );
        assert_ne!(
            current_generation(&fp),
            generation_of_first,
            "旧恢复在写入前复核代次时必须判为已失效(放弃恢复)"
        );

        // 身份指纹变化(HWND 复用 / 进程重启) → 属于另一个对话框,不影响本对话框的代次
        let other = test_fingerprint("wps_generation_other");
        assert_eq!(
            current_generation(&other),
            0,
            "从未注入过的对话框代次必须为 0(新对话框不得被旧代次误伤)"
        );
    }

    /// 审计报告 P0-07 风险 6:源码守卫 —— 去重与代次复核必须真的接在 WPS 链路上。
    #[test]
    fn test_wps_injection_wires_lease_and_generation_checks() {
        const SOURCE: &str = include_str!("uia.rs");
        let production = &SOURCE[..SOURCE.find("\nmod tests {").expect("必须存在测试模块")];

        // 1) 注入入口必须在任何 UIA 查询 / 全局输入之前取得操作级 lease
        let run_start = production
            .find("unsafe fn run_wps_injection")
            .expect("run_wps_injection 必须存在");
        let run_body = &production[run_start..];
        let lease_at = run_body
            .find("WpsInjectLease::try_acquire")
            .expect("注入入口必须取得操作级独占 lease");
        let com_at = run_body
            .find("CoCreateInstance::<_, IUIAutomation>")
            .expect("注入入口必须创建 UIA 客户端");
        assert!(
            lease_at < com_at,
            "lease 必须早于任何 UIA 查询(否则两个线程已经各自搜完控件)"
        );
        let rejected = &run_body[lease_at..com_at];
        assert!(
            rejected.contains("return false"),
            "拿不到 lease 时必须零动作退出(不得继续 SetValue / 发 Enter)"
        );

        // 2) 延迟恢复必须复核 generation 且复核失败即 return
        let restore_start = production
            .find("fn spawn_restore_original_name")
            .expect("spawn_restore_original_name 必须存在");
        let restore_body = &production[restore_start..];
        let gen_check = restore_body
            .find("current_generation(")
            .expect("延迟恢复必须按 generation 复核");
        let clipboard_at = restore_body
            .find("with_temporary_clipboard_text")
            .expect("延迟恢复必须走剪贴板事务");
        assert!(
            gen_check < clipboard_at,
            "generation 复核必须早于任何编辑框写入"
        );
        assert!(
            restore_body[gen_check..clipboard_at].contains("return"),
            "generation 已变化时必须放弃恢复(旧恢复绝不覆盖新注入结果)"
        );
        assert!(
            production.contains("WpsInjectLease::try_acquire(&identity)"),
            "去重必须绑定窗口身份指纹(HWND + PID + 创建时间 + 类名)"
        );
    }
}
