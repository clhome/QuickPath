#![allow(dead_code)]

use crate::dialog::detector::log_debug;
use std::thread;
use std::time::Duration;
use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    TreeScope_Descendants, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, mouse_event, KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    VK_CONTROL, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetCursorPos, GetWindowRect, SetCursorPos, SetForegroundWindow,
};

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

/// 安全读取系统剪贴板文本
fn get_clipboard_text() -> Option<String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let h_data = GetClipboardData(13); // CF_UNICODETEXT = 13
        let res = if let Ok(h) = h_data {
            if !h.0.is_null() {
                let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(h.0));
                if !ptr.is_null() {
                    let u16_ptr = ptr as *const u16;
                    let mut len = 0;
                    while *u16_ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(u16_ptr, len);
                    let text = String::from_utf16_lossy(slice);
                    let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(h.0));
                    Some(text)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let _ = CloseClipboard();
        res
    }
}

/// 安全向系统剪贴板设置文本
fn set_clipboard_text(text: &str) -> bool {
    unsafe {
        for _ in 0..5 {
            if OpenClipboard(None).is_ok() {
                let _ = EmptyClipboard();
                let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                let bytes = wide.len() * 2;
                if let Ok(h) = GlobalAlloc(GHND, bytes) {
                    if !h.0.is_null() {
                        let ptr = GlobalLock(h);
                        if !ptr.is_null() {
                            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
                            let _ = GlobalUnlock(h);
                            let _ = SetClipboardData(13, Some(windows::Win32::Foundation::HANDLE(h.0)));
                        }
                    }
                }
                let _ = CloseClipboard();
                return true;
            }
            thread::sleep(Duration::from_millis(15));
        }
        false
    }
}

/// 模拟快捷键 Ctrl + A 全选
fn simulate_ctrl_a() {
    unsafe {
        keybd_event(VK_CONTROL.0 as u8, 0, Default::default(), 0);
        keybd_event(b'A', 0, Default::default(), 0);
        thread::sleep(Duration::from_millis(20));
        keybd_event(b'A', 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// 模拟快捷键 Ctrl + V 粘贴
fn simulate_ctrl_v() {
    unsafe {
        keybd_event(VK_CONTROL.0 as u8, 0, Default::default(), 0);
        keybd_event(b'V', 0, Default::default(), 0);
        thread::sleep(Duration::from_millis(20));
        keybd_event(b'V', 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// 模拟按下回车键 Enter
fn simulate_enter() {
    unsafe {
        keybd_event(VK_RETURN.0 as u8, 0, Default::default(), 0);
        thread::sleep(Duration::from_millis(20));
        keybd_event(VK_RETURN.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// 智能寻找 WPS 文件对话框中的主“文件名”编辑框元素与对应 ValuePattern
fn find_wps_edit_element(
    uia: &IUIAutomation,
    dialog_hwnd: HWND,
) -> Option<(IUIAutomationElement, IUIAutomationValuePattern, RECT)> {
    unsafe {
        let mut dlg_rect = RECT::default();
        let _ = GetWindowRect(dialog_hwnd, &mut dlg_rect);
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
                if lower_class.contains("button") || lower_class.contains("label") || lower_class.contains("splitter") {
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

        log_debug(&format!(
            "UIA_CHOSEN: name='{}' cls='{}' id='{}' val='{}' score={} rect={:?}",
            best.name, best.class_name, best.auto_id, best.val, best.score, best.rect
        ));

        Some((best.elem, best.val_pattern, best.rect))
    }
}

/// 安全将目标目录注入到 WPS Office 专属对话框中
pub fn inject_path_to_wps_dialog(dialog_hwnd: HWND, target_path: &str) -> bool {
    let clean_path = target_path.trim().trim_end_matches('\\');
    if !std::path::Path::new(clean_path).is_dir() {
        return false;
    }

    let folder_with_slash = format!("{}\\", clean_path);
    let target_folder = folder_with_slash.clone();
    let raw_hwnd = dialog_hwnd.0 as usize;

    log_debug(&format!(
        "UIA_INJECT_START: target_folder='{}' hwnd=0x{:X}",
        target_folder, raw_hwnd
    ));

    // 将注入逻辑放入单独线程，避免任何 UIA 查询等待阻塞主线程
    thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

            let hwnd = HWND(raw_hwnd as *mut _);
            let uia = match CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                Ok(u) => u,
                Err(e) => {
                    log_debug(&format!("UIA_ERR: CoCreateInstance failed: {:?}", e));
                    CoUninitialize();
                    return;
                }
            };

            // 1. 确保前台窗口处于激活状态
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            thread::sleep(Duration::from_millis(50));

            // 2. 智能定位真正的文件名编辑框 QLineEdit
            let (elem, val_pattern, elem_rect) = match find_wps_edit_element(&uia, hwnd) {
                Some(res) => res,
                None => {
                    log_debug("UIA_ERR: find_wps_edit_element failed");
                    CoUninitialize();
                    return;
                }
            };

            // 3. 读取原有文本，判断导航后是否需要恢复文件名
            let old_text = val_pattern.CurrentValue().map(|b| b.to_string()).unwrap_or_default();
            let should_restore_name = !old_text.trim().is_empty()
                && !std::path::Path::new(old_text.trim()).is_dir()
                && !old_text.ends_with('\\');

            log_debug(&format!(
                "UIA_INJECT_PRE: old_text='{}', should_restore={}",
                old_text, should_restore_name
            ));

            // 4. 双重确保聚焦：UIA 聚焦 + 鼠标微点中心点并光速复原光标
            let _ = elem.SetFocus();
            if elem_rect.right > elem_rect.left && elem_rect.bottom > elem_rect.top {
                let mut orig_pt = POINT::default();
                let _ = GetCursorPos(&mut orig_pt);
                let cx = (elem_rect.left + elem_rect.right) / 2;
                let cy = (elem_rect.top + elem_rect.bottom) / 2;
                let _ = SetCursorPos(cx, cy);
                thread::sleep(Duration::from_millis(20));
                mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
                thread::sleep(Duration::from_millis(20));
                mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
                // 瞬间将鼠标恢复至原位，用户毫无察觉
                let _ = SetCursorPos(orig_pt.x, orig_pt.y);
            }
            thread::sleep(Duration::from_millis(40));

            // 5. 备份用户原有剪贴板
            let backup_clip = get_clipboard_text();

            // 6. 将目标文件夹路径写入剪贴板，并通过物理快捷键 Ctrl+A -> Ctrl+V 注入（彻底突破 Qt UIA SetValue 限制！）
            if set_clipboard_text(&target_folder) {
                log_debug("UIA_INJECT: Path set to clipboard, simulating Ctrl+A and Ctrl+V");
                simulate_ctrl_a();
                thread::sleep(Duration::from_millis(30));
                simulate_ctrl_v();
                thread::sleep(Duration::from_millis(50));
            } else {
                // 剪贴板失败兜底：调用 UIA SetValue
                let bstr_folder = BSTR::from(target_folder.as_str());
                let _ = val_pattern.SetValue(&bstr_folder);
            }

            // 7. 立即还原用户的原有剪贴板，保护用户数据隐私与一致性
            if let Some(ref old_clip) = backup_clip {
                set_clipboard_text(old_clip);
            } else {
                // 若用户剪贴板原本为空，清空剪贴板
                if OpenClipboard(None).is_ok() {
                    let _ = EmptyClipboard();
                    let _ = CloseClipboard();
                }
            }

            // 8. 触发回车，促使 WPS 另存为窗口导航至目标目录
            log_debug("UIA_INJECT: Simulating Enter key press for directory navigation");
            simulate_enter();

            // 9. 优雅恢复原文件名（给予充足时间让 WPS Qt 视图载入新目录并刷新）
            if should_restore_name {
                let saved_name = old_text.clone();
                let hwnd_raw_sub = hwnd.0 as usize;

                thread::spawn(move || {
                    // 等待 650ms 确保目录切换完成
                    thread::sleep(Duration::from_millis(650));

                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                    if let Ok(uia_sub) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                        let h_sub = HWND(hwnd_raw_sub as *mut _);
                        if let Some((new_elem, new_pattern, new_rect)) = find_wps_edit_element(&uia_sub, h_sub) {
                            let cur_val = new_pattern.CurrentValue().map(|b| b.to_string()).unwrap_or_default();
                            log_debug(&format!("UIA_RESTORE_CHECK: cur_val='{}', saved_name='{}'", cur_val, saved_name));

                            // 通过物理粘贴恢复原文件名并选中
                            let _ = new_elem.SetFocus();
                            if new_rect.right > new_rect.left && new_rect.bottom > new_rect.top {
                                let mut orig_pt = POINT::default();
                                let _ = GetCursorPos(&mut orig_pt);
                                let cx = (new_rect.left + new_rect.right) / 2;
                                let cy = (new_rect.top + new_rect.bottom) / 2;
                                let _ = SetCursorPos(cx, cy);
                                mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
                                mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
                                let _ = SetCursorPos(orig_pt.x, orig_pt.y);
                            }

                            let sub_clip = get_clipboard_text();
                            if set_clipboard_text(&saved_name) {
                                simulate_ctrl_a();
                                thread::sleep(Duration::from_millis(20));
                                simulate_ctrl_v();
                                thread::sleep(Duration::from_millis(20));
                                simulate_ctrl_a(); // 再次全选原文件名，方便用户直接改名
                            }
                            if let Some(old) = sub_clip {
                                set_clipboard_text(&old);
                            }
                            log_debug(&format!("UIA_RESTORE_DONE: Restored filename to '{}'", saved_name));
                        }
                    }
                    CoUninitialize();
                });
            }

            CoUninitialize();
        }
    });

    true
}
