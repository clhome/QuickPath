#![allow(dead_code)]

use std::thread;
use std::time::Duration;
use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationInvokePattern, TreeScope_Descendants,
    UIA_AutomationIdPropertyId, UIA_InvokePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYEVENTF_KEYUP, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SetForegroundWindow};

/// 通过 UIA 定位 AutomationId 为 "AddButton" 的新建标签页按钮并触发
fn try_uia_add_button(explorer_hwnd: HWND) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let uia: Result<IUIAutomation, _> = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        if let Ok(uia) = uia {
            if let Ok(root) = uia.ElementFromHandle(explorer_hwnd) {
                let auto_id = BSTR::from("AddButton");
                let var_id = VARIANT::from(auto_id);
                if let Ok(cond) = uia.CreatePropertyCondition(
                    UIA_AutomationIdPropertyId,
                    &var_id,
                ) {
                    if let Ok(btn) = root.FindFirst(TreeScope_Descendants, &cond) {
                        if let Ok(pattern) = btn.GetCurrentPattern(UIA_InvokePatternId) {
                            if let Ok(inv) = pattern.cast::<IUIAutomationInvokePattern>() {
                                if inv.Invoke().is_ok() {
                                    return true;
                                }
                            }
                        }
                    }
                }
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

/// 触发 Win11 Explorer 创建新 Tab（UIA 优先 + 原生 Ctrl+T 兜底）
pub fn trigger_new_tab(explorer_hwnd: HWND) -> bool {
    if try_uia_add_button(explorer_hwnd) {
        return true;
    }
    simulate_ctrl_t(explorer_hwnd)
}

/// 兼容旧命名
pub fn trigger_new_tab_via_message(explorer_hwnd: HWND) -> bool {
    trigger_new_tab(explorer_hwnd)
}
