#![allow(dead_code)]

use std::path::Path;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible,
};

#[derive(Debug, Clone)]
pub struct FileDialogInfo {
    pub hwnd: HWND,
    pub process_id: u32,
    pub process_name: String,
    pub window_title: String,
    pub rect: RECT,
    pub file_name_edit_hwnd: Option<HWND>,
}

struct ChildEnumContext {
    has_list_view: bool,
    has_file_name_combo: bool,
    has_toolbar_or_breadcrumb: bool,
    edit_hwnds: Vec<HWND>,
}

/// 探测指定窗口是否为标准或通用的“打开/另存为”文件对话框
pub fn detect_file_dialog(raw_hwnd: HWND) -> Option<FileDialogInfo> {
    unsafe {
        if !IsWindow(Some(raw_hwnd)).as_bool() {
            return None;
        }

        // 尝试解析顶层祖先窗口，防止焦点在子控件时类名不匹配
        let root_hwnd = windows::Win32::UI::WindowsAndMessaging::GetAncestor(
            raw_hwnd,
            windows::Win32::UI::WindowsAndMessaging::GA_ROOT,
        );
        let hwnd = if !root_hwnd.0.is_null() && IsWindow(Some(root_hwnd)).as_bool() {
            root_hwnd
        } else {
            raw_hwnd
        };

        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }

        let class_name = get_window_class_name(hwnd);
        // 大多数标准与通用文件对话框的类名均为 #32770
        if class_name != "#32770" {
            return None;
        }

        // 枚举子控件以确认特征
        let mut ctx = ChildEnumContext {
            has_list_view: false,
            has_file_name_combo: false,
            has_toolbar_or_breadcrumb: false,
            edit_hwnds: Vec::new(),
        };

        let _ = EnumChildWindows(
            Some(hwnd),
            Some(enum_children_proc),
            LPARAM(&mut ctx as *mut _ as isize),
        );

        // 文件对话框必须同时满足两个必要条件：
        // 1. 文件列表视图（DirectUIHWND / SHELLDLL_DefView）
        // 2. 文件名输入组合框（ComboBoxEx32）
        // 属性页/安装向导/工具软件等 #32770 窗口不会同时具备这两者
        if !ctx.has_list_view || !ctx.has_file_name_combo {
            return None;
        }

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);

        let (pid, process_name) = get_window_process_info(hwnd);
        let window_title = get_window_title(hwnd);

        // 识别文件名输入框（通常是第一个可见的单行 Edit 控件）
        let file_name_edit = ctx.edit_hwnds.first().copied();

        Some(FileDialogInfo {
            hwnd,
            process_id: pid,
            process_name,
            window_title,
            rect,
            file_name_edit_hwnd: file_name_edit,
        })
    }
}

unsafe extern "system" fn enum_children_proc(child: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lparam.0 as *mut ChildEnumContext);
        let class = get_window_class_name(child);

        if class == "DirectUIHWND" || class == "SHELLDLL_DefView" {
            ctx.has_list_view = true;
        } else if class == "ComboBoxEx32" {
            ctx.has_file_name_combo = true;
        } else if class.starts_with("ToolbarWindow32") || class.contains("Breadcrumb") {
            ctx.has_toolbar_or_breadcrumb = true;
        } else if class.eq_ignore_ascii_case("Edit") {
            if IsWindowVisible(child).as_bool() {
                ctx.edit_hwnds.push(child);
            }
        }

        BOOL(1)
    }
}

pub fn get_window_class_name(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf);
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

pub fn get_window_title(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

pub fn get_window_process_info(hwnd: HWND) -> (u32, String) {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return (0, String::new());
        }

        let process_handle = OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        );

        if let Ok(handle) = process_handle {
            let mut name_buf = [0u16; 1024];
            let len = GetModuleFileNameExW(Some(handle), None, &mut name_buf);
            let _ = windows::Win32::Foundation::CloseHandle(handle);

            if len > 0 {
                let full_path = String::from_utf16_lossy(&name_buf[..len as usize]);
                let file_name = Path::new(&full_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or(full_path);
                return (pid, file_name);
            }
        }

        (pid, String::new())
    }
}
