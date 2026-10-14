#![allow(dead_code)]

use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationInvokePattern, TreeScope_Descendants,
    UIA_AutomationIdPropertyId, UIA_InvokePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL, keybd_event};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, IsWindow, PostMessageW, SetForegroundWindow, WM_COMMAND,
};
use windows::core::{BSTR, Interface};

/// 尝试向 ShellTabWindowClass 发送 Windows 11 Explorer 内部原生创建新 Tab 的 0xA21B 魔法指令
fn try_magic_command_add_tab(explorer_hwnd: HWND) -> bool {
    if explorer_hwnd.0.is_null() {
        return false;
    }
    unsafe {
        if !IsWindow(Some(explorer_hwnd)).as_bool() {
            return false;
        }
        if let Ok(tab_wnd) = FindWindowExW(
            Some(explorer_hwnd),
            None,
            windows::core::w!("ShellTabWindowClass"),
            None,
        ) && !tab_wnd.0.is_null()
        {
            // 采用 PostMessageW 异步安全投递，绝不阻塞 UI 消息泵（与 ExplorerTabUtility 架构对齐）
            let _ = PostMessageW(Some(tab_wnd), WM_COMMAND, WPARAM(0xA21B), LPARAM(0));
            return true;
        }
    }
    false
}

/// 通过 UIA 定位 AutomationId 为 "AddButton" 的新建标签页按钮并触发
fn try_uia_add_button(explorer_hwnd: HWND) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let uia: Result<IUIAutomation, _> =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        if let Ok(uia) = uia
            && let Ok(root) = uia.ElementFromHandle(explorer_hwnd)
        {
            let auto_id = BSTR::from("AddButton");
            let var_id = VARIANT::from(auto_id);
            if let Ok(cond) = uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &var_id)
                && let Ok(btn) = root.FindFirst(TreeScope_Descendants, &cond)
                && let Ok(pattern) = btn.GetCurrentPattern(UIA_InvokePatternId)
                && let Ok(inv) = pattern.cast::<IUIAutomationInvokePattern>()
                && inv.Invoke().is_ok()
            {
                return true;
            }
        }
    }
    false
}

/// 降级策略：激活宿主窗口并发送硬件级 Ctrl + T 快捷键
fn simulate_ctrl_t(explorer_hwnd: HWND) -> bool {
    unsafe {
        if explorer_hwnd.0.is_null() || !IsWindow(Some(explorer_hwnd)).as_bool() {
            return false;
        }
        let _ = SetForegroundWindow(explorer_hwnd);
        thread::sleep(Duration::from_millis(30));

        keybd_event(VK_CONTROL.0 as u8, 0, Default::default(), 0);
        keybd_event(b'T', 0, Default::default(), 0);
        thread::sleep(Duration::from_millis(20));
        keybd_event(b'T', 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        true
    }
}

/// 触发 Win11 Explorer 创建新 Tab（魔法指令 0xA21B 优先 + UIA 次之 + 原生 Ctrl+T 兜底）
pub fn trigger_new_tab(explorer_hwnd: HWND) -> bool {
    if try_magic_command_add_tab(explorer_hwnd) {
        return true;
    }
    if try_uia_add_button(explorer_hwnd) {
        return true;
    }
    simulate_ctrl_t(explorer_hwnd)
}

/// 无全局输入的安全新建标签：仅使用窗口消息与 UIA 语义动作，
/// 绝不发送全局键盘事件（Ctrl+T 兜底会把按键送到当时的前台窗口，
/// 属于跨应用输入风险）。调用方在返回 false 时应用原生窗口兜底。
pub fn trigger_new_tab_safe(explorer_hwnd: HWND) -> bool {
    if try_magic_command_add_tab(explorer_hwnd) {
        return true;
    }
    try_uia_add_button(explorer_hwnd)
}

/// 兼容旧命名
pub fn trigger_new_tab_via_message(explorer_hwnd: HWND) -> bool {
    trigger_new_tab(explorer_hwnd)
}
