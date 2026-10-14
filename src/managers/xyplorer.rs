#![allow(dead_code)]

use std::path::Path;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, WM_COPYDATA};
use windows::core::PCWSTR;

use super::ipc::{self, Manager};

const XYPLORER_COPYDATA_FLAG: usize = 4194305;

/// ThunderRT6FormDC 窗口必须属于以下进程镜像之一，否则视为同类名伪装窗口，不发送协议消息
const EXPECTED_PROCESS_NAMES: [&str; 1] = ["xyplorer.exe"];

#[derive(Debug, Clone)]
pub struct XYplorerFolder {
    pub path: String,
    pub title: String,
    pub is_active: bool,
    pub hwnd: HWND,
}

/// 扫描系统中所有运行的 XYplorer 窗口及其打开的路径
pub fn get_xyplorer_folders() -> Vec<XYplorerFolder> {
    let mut results = Vec::new();

    // 冷却窗口内不重试（上次超时后避免每次前台切换都白等一个超时）
    if !ipc::try_enter(Manager::XYplorer) {
        return results;
    }

    let class_name: Vec<u16> = "ThunderRT6FormDC".encode_utf16().chain(Some(0)).collect();
    let class_pcwstr = PCWSTR(class_name.as_ptr());

    unsafe {
        let mut hwnd = FindWindowExW(None, None, class_pcwstr, PCWSTR::null()).unwrap_or_default();
        if hwnd.0.is_null() {
            return results;
        }

        // 进程镜像核对：只对真正的 XYplorer 窗口发送 WM_COPYDATA
        if !ipc::window_process_matches(hwnd, &EXPECTED_PROCESS_NAMES) {
            crate::dialog::detector::log_debug(
                "IPC: 命中 ThunderRT6FormDC 类名但进程镜像不符，已跳过（疑似伪装窗口）",
            );
            ipc::report_failure(Manager::XYplorer);
            return results;
        }

        // P0 安全守卫：在枚举前完整快照剪贴板（含文件列表 CF_HDROP、截图位图等）
        // 若剪贴板被占用或关键格式无法保真备份，宁可放弃扫描，绝不破坏用户剪贴板！
        let snapshot = match crate::win32::clipboard::ClipboardSnapshot::capture() {
            Ok(s) => s,
            Err(_) => return results,
        };
        let mut guard = crate::win32::clipboard::ClipboardRestoreGuard::new(snapshot);

        let mut any_delivered = false;
        let mut any_timeout = false;

        while !hwnd.0.is_null() {
            // 1. 获取活动窗格路径
            //    先清空文本：XY 不响应时绝不会把上一次的陈旧路径当成结果读回来
            let _ = crate::win32::clipboard::clear_clipboard_text();
            match send_xyplorer_script(hwnd, "::copytext get('path', a);") {
                Some(_) => {
                    any_delivered = true;
                    if let Some(path) = crate::win32::clipboard::get_clipboard_text() {
                        let clean_path = path.trim().trim_end_matches('\\').to_string();
                        if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                            results.push(XYplorerFolder {
                                path: clean_path,
                                title: "XYplorer (活动窗格)".to_string(),
                                is_active: true,
                                hwnd,
                            });
                        }
                    }
                }
                None => any_timeout = true,
            }

            // 2. 获取非活动窗格路径
            let _ = crate::win32::clipboard::clear_clipboard_text();
            match send_xyplorer_script(hwnd, "::copytext get('path', i);") {
                Some(_) => {
                    any_delivered = true;
                    if let Some(path) = crate::win32::clipboard::get_clipboard_text() {
                        let clean_path = path.trim().trim_end_matches('\\').to_string();
                        if !clean_path.is_empty()
                            && Path::new(&clean_path).is_dir()
                            && !results.iter().any(|r| r.path == clean_path)
                        {
                            results.push(XYplorerFolder {
                                path: clean_path,
                                title: "XYplorer (副窗格)".to_string(),
                                is_active: false,
                                hwnd,
                            });
                        }
                    }
                }
                None => any_timeout = true,
            }

            // 第一次超时即视为该管理器已挂起：立即中止整轮扫描并进入冷却
            if any_timeout {
                break;
            }

            hwnd =
                FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null()).unwrap_or_default();
        }

        // 扫描完成，立即恢复用户原始剪贴板（恢复失败仅记录，不影响扫描结果）
        if let Err(err) = guard.restore_now() {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_FAILED(xyplorer): {:?}",
                err
            ));
        }

        if any_timeout {
            crate::dialog::detector::log_debug("IPC: XYplorer 响应超时，进入冷却");
            ipc::report_failure(Manager::XYplorer);
        } else if any_delivered {
            ipc::report_success(Manager::XYplorer);
        }
    }

    results
}

/// 有界投递 XYplorer 脚本：使用 `SendMessageTimeoutW(SMTO_BLOCK)`，
/// 保证 `COPYDATASTRUCT` 缓冲区在目标读取期间始终有效，同时对端挂起时能及时脱身。
unsafe fn send_xyplorer_script(hwnd: HWND, script: &str) -> Option<usize> {
    let wide_script: Vec<u16> = script.encode_utf16().chain(Some(0)).collect();
    let cb_data = (wide_script.len() * 2) as u32;

    let copy_data = COPYDATASTRUCT {
        dwData: XYPLORER_COPYDATA_FLAG,
        cbData: cb_data,
        lpData: wide_script.as_ptr() as *mut _,
    };

    ipc::send_message_timeout(
        hwnd,
        WM_COPYDATA,
        WPARAM(0),
        LPARAM(&copy_data as *const _ as isize),
    )
}
