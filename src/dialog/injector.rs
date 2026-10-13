use crate::dialog::DialogKind;
use crate::win32::ime::ImeGuard;
use std::path::Path;
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, IsChild, IsWindow, PostMessageW, SendMessageTimeoutW,
    SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_COMMAND, WM_GETTEXT, WM_SETTEXT,
};

const EM_SETSEL: u32 = 0x00B1;
const MESSAGE_TIMEOUT_MS: u32 = 200;

/// 安全将目标目录注入到文件对话框中，并促使其平滑导航切换
/// 根据 DialogKind 严格物理隔离：标准 Win32 走纯 Win32 消息，WPS Office 走专用 UIA 引擎
pub fn inject_path_to_dialog(
    dialog_hwnd: HWND,
    edit_hwnd: HWND,
    target_path: &str,
    kind: DialogKind,
) -> bool {
    let clean_path = clean_directory_path(target_path);
    if !unsafe { IsWindow(Some(dialog_hwnd)).as_bool() }
        || !Path::new(clean_path).is_absolute()
        || !Path::new(clean_path).is_dir()
    {
        return false;
    }

    match kind {
        DialogKind::WpsOffice => {
            // 针对 WPS Office 自绘对话框，通过专用的 UI Automation 引擎进行注入
            crate::win32::uia::inject_path_to_wps_dialog(dialog_hwnd, target_path)
        }
        DialogKind::StandardWin32 => {
            // 针对标准 Win32 对话框，绝对只走纯原生 Win32 消息通道，绝不调用 UIA！
            inject_path_to_standard_dialog(dialog_hwnd, edit_hwnd, clean_path)
        }
    }
}

fn inject_path_to_standard_dialog(dialog_hwnd: HWND, edit_hwnd: HWND, clean_path: &str) -> bool {
    unsafe {
        // 对话框和 Edit 均须有效，且 Edit 必须仍属于该对话框。
        if !is_dialog_edit(dialog_hwnd, edit_hwnd) {
            return false;
        }
        let dialog_identity = window_identity(dialog_hwnd);
        let edit_identity = window_identity(edit_hwnd);

        // 根路径保留原有斜杠，其他目录只追加一个导航用斜杠。
        let folder_with_slash = directory_navigation_text(clean_path);

        // 1. 有界读取当前文件名；超时不是空文件名，直接停止注入。
        let Some(old_text) = get_edit_text(edit_hwnd) else {
            return false;
        };
        let should_restore_name = !old_text.trim().is_empty()
            && !Path::new(old_text.trim()).is_dir()
            && !old_text.ends_with(['\\', '/']);

        // 2. 启用输入法保护守卫（离开作用域时自动恢复输入法）
        let _ime_guard = ImeGuard::new(dialog_hwnd);

        // 3. 焦点与前台确保
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(dialog_hwnd);
        let _ = SetFocus(Some(edit_hwnd));

        // 4. 将目录路径通过 WM_SETTEXT 直接设置进编辑控件（无击键延迟，零乱码）
        let wide_folder: Vec<u16> = folder_with_slash.encode_utf16().chain(Some(0)).collect();
        if !is_dialog_edit(dialog_hwnd, edit_hwnd)
            || send_edit_message(
                edit_hwnd,
                WM_SETTEXT,
                WPARAM(0),
                LPARAM(wide_folder.as_ptr() as isize),
            ).is_none_or(|result| result == 0)
        {
            return false;
        }

        // 5. 只投递一次 IDOK 导航，不能再追加 Enter 造成二次确认。
        if !is_dialog_edit(dialog_hwnd, edit_hwnd)
            || PostMessageW(Some(dialog_hwnd), WM_COMMAND, WPARAM(1), LPARAM(0)).is_err()
        {
            return false;
        }

        // 6. 仅在必要时（如另存为），等待对话框完成平滑导航后，再优雅恢复原文件名
        if should_restore_name {
            let dialog_raw = dialog_hwnd.0 as usize;
            let edit_raw = edit_hwnd.0 as usize;
            let name_to_restore = old_text;
            let check_folder = folder_with_slash;
            thread::spawn(move || {
                // 等待对话框完成视图刷新与目录定位。
                thread::sleep(Duration::from_millis(350));
                let h_dialog = HWND(dialog_raw as *mut _);
                let h_edit = HWND(edit_raw as *mut _);
                if !is_dialog_edit(h_dialog, h_edit)
                    || window_identity(h_dialog) != dialog_identity
                    || window_identity(h_edit) != edit_identity
                {
                    return;
                }
                let Some(current) = get_edit_text(h_edit) else {
                    return;
                };
                // 仅接受空值或本次注入的精确目录；用户输入其他目录也不能覆盖。
                if should_restore_file_name(&current, &check_folder)
                    && is_dialog_edit(h_dialog, h_edit)
                    && window_identity(h_dialog) == dialog_identity
                    && window_identity(h_edit) == edit_identity
                    && get_edit_text(h_edit).as_deref() == Some(current.as_str())
                {
                    let wide_name: Vec<u16> = name_to_restore.encode_utf16().chain(Some(0)).collect();
                    if send_edit_message(
                        h_edit,
                        WM_SETTEXT,
                        WPARAM(0),
                        LPARAM(wide_name.as_ptr() as isize),
                    ).is_none_or(|result| result == 0) {
                        return;
                    }
                    // 只有原关系和恢复文本仍未改变，才全选；后台线程不抢焦点。
                    if is_dialog_edit(h_dialog, h_edit)
                        && get_edit_text(h_edit).as_deref() == Some(name_to_restore.as_str())
                    {
                        let _ = send_edit_message(h_edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
                    }
                }
            });
        }

        true
    }
}

fn clean_directory_path(target_path: &str) -> &str {
    let path = target_path.trim();
    let trimmed = path.trim_end_matches(['\\', '/']);
    let bytes = trimmed.as_bytes();
    let drive_prefix = bytes.len() == 2
        || (bytes.len() == 6 && (trimmed.starts_with("\\\\?\\") || trimmed.starts_with("\\\\.\\")));
    if drive_prefix && bytes[bytes.len() - 2].is_ascii_alphabetic()
        && bytes[bytes.len() - 1] == b':' && path.len() > trimmed.len()
    {
        &path[..trimmed.len() + 1]
    } else {
        trimmed
    }
}

fn directory_navigation_text(clean_path: &str) -> String {
    if clean_path.ends_with(['\\', '/']) {
        clean_path.to_string()
    } else {
        format!("{}\\", clean_path)
    }
}

fn should_restore_file_name(current: &str, injected_folder: &str) -> bool {
    current.is_empty() || current == injected_folder
}

fn is_dialog_edit(dialog_hwnd: HWND, edit_hwnd: HWND) -> bool {
    unsafe {
        IsWindow(Some(dialog_hwnd)).as_bool()
            && IsWindow(Some(edit_hwnd)).as_bool()
            && IsChild(dialog_hwnd, edit_hwnd).as_bool()
    }
}

fn window_identity(hwnd: HWND) -> (u32, u32) {
    unsafe {
        let mut pid = 0;
        let thread_id = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (thread_id, pid)
    }
}

fn send_edit_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<usize> {
    unsafe {
        let mut result = 0;
        let delivered = SendMessageTimeoutW(
            hwnd,
            message,
            wparam,
            lparam,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            MESSAGE_TIMEOUT_MS,
            Some(&mut result),
        );
        (delivered.0 != 0).then_some(result)
    }
}

fn get_edit_text(hwnd: HWND) -> Option<String> {
    // 一次有界读取，避免长度查询后用户输入增长造成截断并误判为可恢复文本。
    let mut buf = vec![0u16; 32768];
    let copied = send_edit_message(
        hwnd,
        WM_GETTEXT,
        WPARAM(buf.len()),
        LPARAM(buf.as_mut_ptr() as isize),
    )?;
    if copied >= buf.len() - 1 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..copied]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_directory_roots_preserve_absolute_paths() {
        for path in ["C:\\", "C:\\\\\\", " C:\\ ", "c:/", "C:////"] {
            let clean = clean_directory_path(path);
            assert_eq!(clean.len(), 3);
            assert!(Path::new(clean).is_absolute());
            assert_eq!(directory_navigation_text(clean), clean);
        }
        for path in ["", " ", "\\", "/", "相对目录", " C: "] {
            assert!(!Path::new(clean_directory_path(path)).is_absolute());
        }
        assert_eq!(clean_directory_path("C:"), "C:");
        assert!(!Path::new(clean_directory_path("C:")).is_absolute());
        assert_eq!(clean_directory_path(" C:\\Users\\ "), "C:\\Users");
        assert_eq!(directory_navigation_text("C:\\Users"), "C:\\Users\\");
        assert_eq!(directory_navigation_text(clean_directory_path("\\\\server\\share\\")), "\\\\server\\share\\");
        assert_eq!(directory_navigation_text(clean_directory_path("\\\\?\\C:\\")), "\\\\?\\C:\\");
    }

    #[test]
    fn test_restore_preserves_user_input() {
        let folder = "C:\\target\\";
        assert!(should_restore_file_name("", folder));
        assert!(should_restore_file_name(folder, folder));
        for current in ["D:\\other\\", "C:\\target\\new\\", "new.txt", " ", "C:\\target"] {
            assert!(!should_restore_file_name(current, folder));
        }
    }

    #[test]
    fn test_invalid_dialog_edit_is_rejected() {
        let invalid = HWND(std::ptr::null_mut());
        assert!(!is_dialog_edit(invalid, invalid));
        assert!(!inject_path_to_standard_dialog(invalid, invalid, "C:\\"));
        assert!(get_edit_text(invalid).is_none());
    }

    #[test]
    fn test_hidden_edit_relationship_and_bounded_messages() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_CHILD, WS_POPUP,
        };
        unsafe {
            let dialog = CreateWindowExW(
                WINDOW_EX_STYLE(0), w!("#32770"), w!(""), WS_POPUP,
                0, 0, 100, 100, None, None, None, None,
            ).unwrap();
            let other = CreateWindowExW(
                WINDOW_EX_STYLE(0), w!("#32770"), w!(""), WS_POPUP,
                0, 0, 100, 100, None, None, None, None,
            ).unwrap();
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0), w!("Edit"), w!("原文件.txt"), WS_CHILD,
                0, 0, 100, 20, Some(dialog), None, None, None,
            ).unwrap();
            assert!(is_dialog_edit(dialog, edit));
            assert!(!is_dialog_edit(dialog, dialog));
            assert!(!is_dialog_edit(other, edit));
            assert_eq!(get_edit_text(edit).as_deref(), Some("原文件.txt"));
            let text: Vec<u16> = "C:\\".encode_utf16().chain(Some(0)).collect();
            assert_eq!(send_edit_message(edit, WM_SETTEXT, WPARAM(0), LPARAM(text.as_ptr() as isize)), Some(1));
            assert_eq!(get_edit_text(edit).as_deref(), Some("C:\\"));
            assert!(send_edit_message(edit, EM_SETSEL, WPARAM(0), LPARAM(-1)).is_some());
            let _ = DestroyWindow(dialog);
            assert!(!is_dialog_edit(dialog, edit));
            let _ = DestroyWindow(other);
        }
    }

    #[test]
    fn test_unresponsive_test_window_times_out() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
        };
        let (created_tx, created_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let owner = thread::spawn(move || unsafe {
            let window = CreateWindowExW(
                WINDOW_EX_STYLE(0), w!("Edit"), w!(""), WS_POPUP,
                0, 0, 100, 20, None, None, None, None,
            ).unwrap();
            created_tx.send(window.0 as usize).unwrap();
            // 自建窗口线程故意不处理消息，模拟无响应目标，不操作真实进程。
            let _ = release_rx.recv();
            let _ = DestroyWindow(window);
        });
        let window = HWND(created_rx.recv().unwrap() as *mut _);
        let started = std::time::Instant::now();
        let result = send_edit_message(window, EM_SETSEL, WPARAM(0), LPARAM(-1));
        let elapsed = started.elapsed();
        release_tx.send(()).unwrap();
        owner.join().unwrap();
        assert!(result.is_none());
        assert!(elapsed < Duration::from_secs(2));
    }
}
