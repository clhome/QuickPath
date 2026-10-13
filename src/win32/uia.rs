#![allow(dead_code)]

use crate::dialog::detector::log_debug;
use std::thread;
use std::time::Duration;
use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    TreeScope_Descendants, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, mouse_event, KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    VK_CONTROL, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId,
    IsWindow, SetCursorPos, SetForegroundWindow,
};

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

                let name = elem.CurrentName().map(|b| b.to_string()).unwrap_or_default();
                let class_name = elem.CurrentClassName().map(|b| b.to_string()).unwrap_or_default();
                let auto_id = elem.CurrentAutomationId().map(|b| b.to_string()).unwrap_or_default();
                let val = val_pattern.CurrentValue().map(|b| b.to_string()).unwrap_or_default();
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
                if lower_name.contains("搜索") || lower_name.contains("search") || lower_id.contains("search") {
                    score -= 800;
                }

                // 绝对最高优先级：真正的文本编辑控件 QLineEdit
                if class_name == "QLineEdit" || class_name == "kd::KDTextField" {
                    score += 500;
                } else if ctrl_type == 50004 { // UIA_EditControlTypeId
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
                if name.contains("文件名") || name.contains("File name") || auto_id.contains("fileName") || auto_id.contains("FileName") {
                    score += 100;
                }

                log_debug(&format!(
                    "UIA_EVAL[{}]: score={} name='{}' cls='{}' id='{}' val='{}' top={}",
                    i, score, name, class_name, auto_id, val, elem_rect.top
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
                "UIA_REJECT: Best candidate score too low ({}) name='{}' cls='{}' id='{}'",
                best.score, best.name, best.class_name, best.auto_id
            ));
            return None;
        }

        log_debug(&format!(
            "UIA_CHOSEN: name='{}' cls='{}' id='{}' val='{}' score={} rect={:?}",
            best.name, best.class_name, best.auto_id, best.val, best.score, best.rect
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

        // 路径 1：受保护的剪贴板临时注入（快照 + 事务化恢复用户原剪贴板）。
        // 每次 Ctrl+A / Ctrl+V 之前都重新核验身份、前台与控件焦点；
        // 任一环节不满足即立即中止，绝不把全局按键发给错误窗口。
        let mut sequence_ok = false;
        let clipboard_result = crate::win32::clipboard::with_temporary_clipboard_text(
            target,
            || {
                if !simulate_ctrl_a_checked(ctx) {
                    return;
                }
                thread::sleep(Duration::from_millis(30));
                if !simulate_ctrl_v_checked(ctx) {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
                sequence_ok = true;
            },
        );

        let clipboard_injected = match clipboard_result {
            Ok(()) if sequence_ok => true,
            Ok(()) => {
                log_debug("UIA_INJECT: 物理按键前置校验未通过，本次剪贴板注入已中止");
                false
            }
            Err(err) => {
                log_debug(&format!(
                    "UIA_INJECT: 剪贴板安全守卫拒绝或恢复失败({:?})，降级使用 UIA SetValue",
                    err
                ));
                false
            }
        };

        if clipboard_injected && wait_readback_match(pattern, target) {
            return true;
        }

        // 路径 2：UIA ValuePattern::SetValue（必须检查返回值，绝不以“调用过”当成功）
        if pattern.SetValue(&BSTR::from(target)).is_err() {
            log_debug("UIA_INJECT: SetValue 返回失败");
            return false;
        }
        wait_readback_match(pattern, target)
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
                        "UIA_VERIFY[{}]: 回读不一致 current='{}' target='{}'",
                        attempt, current, target
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
            log_debug("UIA_REJECT: 无法确立目标窗口身份（窗口已失效）");
            return false;
        };

        let uia =
            match CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            {
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
            "UIA_INJECT_PRE: old_text='{}', should_restore={}",
            old_text, should_restore_name
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

        // 8. 优雅恢复原文件名（尽力而为，不影响本次注入结果）
        if should_restore_name {
            spawn_restore_original_name(identity, old_text);
        }

        true
    }
}

/// 目录导航完成后把原文件名恢复回编辑框（尽力而为）。
///
/// 恢复前复核窗口身份与前台状态；若检测到用户已输入新内容则绝不覆盖。
fn spawn_restore_original_name(identity: WindowIdentity, saved_name: String) {
    thread::spawn(move || unsafe {
        // 等待 WPS Qt 视图载入新目录并刷新
        thread::sleep(Duration::from_millis(650));

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
            "UIA_RESTORE_CHECK: cur_val='{}', saved_name='{}'",
            current, saved_name
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
        let clipboard_ok = crate::win32::clipboard::with_temporary_clipboard_text(&saved_name, || {
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
            "UIA_RESTORE_DONE: restore_requested='{}' clipboard_ok={}",
            saved_name, restored
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
        "UIA_INJECT_START: target_folder='{}' hwnd=0x{:X}",
        folder_with_slash, raw_hwnd
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
}
