use crate::win32::ime::ImeGuard;
use std::path::Path;
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextLengthW, GetWindowTextW, PostMessageW, SendMessageW,
    WM_KEYDOWN, WM_KEYUP, WM_SETTEXT,
};

const EM_SETSEL: u32 = 0x00B1;

/// 安全将目标目录注入到文件对话框中，并促使其平滑导航切换
/// 保证：
/// 1. 不丢失用户已输入的文件名
/// 2. 隔离输入法，不触发任何中文拼音候选框
/// 3. 操作后自动恢复原焦点与选区
pub fn inject_path_to_dialog(dialog_hwnd: HWND, edit_hwnd: HWND, target_path: &str) -> bool {
    let clean_path = target_path.trim().trim_end_matches('\\');
    if !Path::new(clean_path).is_dir() {
        return false;
    }

    // 针对 WPS Office 自绘对话框或无独立 Edit 句柄的窗口，通过 UI Automation 引擎进行注入
    let class_name = crate::dialog::detector::get_window_class_name(dialog_hwnd);
    let has_valid_edit = unsafe {
        windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(edit_hwnd)).as_bool()
            && edit_hwnd != dialog_hwnd
    };

    if class_name == "KcfdFileDialog" || class_name == "Qt5QWindowIcon" || !has_valid_edit {
        return crate::win32::uia::inject_path_to_wps_dialog(dialog_hwnd, target_path);
    }

    // 格式化为带有单斜杠结尾的目录字符串（用于告知系统对话框进入该目录而非将其当做文件名保存）
    let folder_with_slash = format!("{}\\", clean_path);

    unsafe {
        // 1. 读取当前已有的文件名文本
        let old_text = get_edit_text(edit_hwnd);
        let should_restore_name = !old_text.trim().is_empty()
            && !Path::new(old_text.trim()).is_dir()
            && !old_text.ends_with('\\');

        // 2. 启用输入法保护守卫（离开作用域时自动恢复输入法）
        let _ime_guard = ImeGuard::new(dialog_hwnd);

        // 3. 焦点与前台确保
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(dialog_hwnd);
        let _ = SetFocus(Some(edit_hwnd));

        // 4. 将目录路径通过 WM_SETTEXT 直接设置进编辑控件（无击键延迟，零乱码）
        let wide_folder: Vec<u16> = folder_with_slash.encode_utf16().chain(Some(0)).collect();
        let set_res = SendMessageW(
            edit_hwnd,
            WM_SETTEXT,
            Some(WPARAM(0)),
            Some(LPARAM(wide_folder.as_ptr() as isize)),
        );

        if set_res.0 == 0 {
            return false;
        }

        // 5. 触发双重跳转命令（标准 IDOK 确认命令 + 键盘回车）
        // 向对话框主窗口发送 IDOK (1) 命令，等同于点击“打开(O)”按钮，促使 Shell 命名空间原生导航
        let _ = PostMessageW(
            Some(dialog_hwnd),
            windows::Win32::UI::WindowsAndMessaging::WM_COMMAND,
            WPARAM(1), // IDOK
            LPARAM(0),
        );

        let _ = PostMessageW(
            Some(edit_hwnd),
            WM_KEYDOWN,
            WPARAM(VK_RETURN.0 as usize),
            LPARAM(0),
        );
        let _ = PostMessageW(
            Some(edit_hwnd),
            WM_KEYUP,
            WPARAM(VK_RETURN.0 as usize),
            LPARAM(0),
        );

        // 6. 仅在必要时（如另存为），等待对话框完成平滑导航后，再优雅恢复原文件名
        if should_restore_name {
            let edit_raw = edit_hwnd.0 as usize;
            let name_to_restore = old_text.clone();
            let check_folder = folder_with_slash.clone();
            thread::spawn(move || {
                // 等待对话框完成视图刷新与目录定位
                thread::sleep(Duration::from_millis(350));
                let h_edit = HWND(edit_raw as *mut _);
                if !windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(h_edit)).as_bool() {
                    return;
                }
                let current = get_edit_text(h_edit);
                // 仅当编辑框当前已被对话框重置/清空，或仍残留旧目录时，才安全恢复文件名
                if current.is_empty() || current == check_folder || current.ends_with('\\') {
                    let wide_name: Vec<u16> = name_to_restore.encode_utf16().chain(Some(0)).collect();
                    let _ = SendMessageW(
                        h_edit,
                        WM_SETTEXT,
                        Some(WPARAM(0)),
                        Some(LPARAM(wide_name.as_ptr() as isize)),
                    );
                    // 全选文件名以便用户直接修改
                    let _ = SendMessageW(
                        h_edit,
                        EM_SETSEL,
                        Some(WPARAM(0)),
                        Some(LPARAM(-1)),
                    );
                    let _ = SetFocus(Some(h_edit));
                }
            });
        }

        true
    }
}

fn get_edit_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied > 0 {
            String::from_utf16_lossy(&buf[..copied as usize])
        } else {
            String::new()
        }
    }
}
