#![allow(dead_code)]

use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, WM_COMMAND};

use super::ipc::{self, Manager};

const CM_COPY_SRC_PATH_TO_CLIP: usize = 2029;
const CM_COPY_TRG_PATH_TO_CLIP: usize = 2030;

/// TTOTAL_CMD 窗口必须属于以下进程镜像之一，否则视为同类名伪装窗口，不发送协议消息
const EXPECTED_PROCESS_NAMES: [&str; 3] = ["totalcmd.exe", "totalcmd64.exe", "totalcmd_unicode.exe"];

#[derive(Debug, Clone)]
pub struct TotalCmdFolder {
    pub path: String,
    pub title: String,
    pub is_active: bool,
    pub hwnd: HWND,
}

/// 扫描系统中所有运行的 Total Commander 窗口及其打开的左右路径
pub fn get_totalcmd_folders() -> Vec<TotalCmdFolder> {
    let mut results = Vec::new();

    // 冷却窗口内不重试（上次超时后避免每次前台切换都白等一个超时）
    if !ipc::try_enter(Manager::TotalCmd) {
        return results;
    }

    let class_name: Vec<u16> = "TTOTAL_CMD".encode_utf16().chain(Some(0)).collect();
    let class_pcwstr = PCWSTR(class_name.as_ptr());

    unsafe {
        let mut hwnd = FindWindowExW(None, None, class_pcwstr, PCWSTR::null()).unwrap_or_default();
        if hwnd.0.is_null() {
            return results;
        }

        // 进程镜像核对：只对真正的 Total Commander 窗口发送 WM_COMMAND
        if !ipc::window_process_matches(hwnd, &EXPECTED_PROCESS_NAMES) {
            crate::dialog::detector::log_debug(
                "IPC: 命中 TTOTAL_CMD 类名但进程镜像不符，已跳过（疑似伪装窗口）",
            );
            ipc::report_failure(Manager::TotalCmd);
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
            // 1. 获取源/当前活动面板路径
            //    先清空文本：TM 不响应时绝不会把上一次的陈旧路径当成结果读回来
            let _ = crate::win32::clipboard::clear_clipboard_text();
            match ipc::send_message_timeout(
                hwnd,
                WM_COMMAND,
                WPARAM(CM_COPY_SRC_PATH_TO_CLIP),
                LPARAM(0),
            ) {
                Some(_) => {
                    any_delivered = true;
                    if let Some(src_path) = crate::win32::clipboard::get_clipboard_text() {
                        let clean_path = src_path.trim().trim_end_matches('\\').to_string();
                        if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                            results.push(TotalCmdFolder {
                                path: clean_path,
                                title: "Total Commander (活动窗格)".to_string(),
                                is_active: true,
                                hwnd,
                            });
                        }
                    }
                }
                None => any_timeout = true,
            }

            // 2. 获取目标/副面板路径
            let _ = crate::win32::clipboard::clear_clipboard_text();
            match ipc::send_message_timeout(
                hwnd,
                WM_COMMAND,
                WPARAM(CM_COPY_TRG_PATH_TO_CLIP),
                LPARAM(0),
            ) {
                Some(_) => {
                    any_delivered = true;
                    if let Some(trg_path) = crate::win32::clipboard::get_clipboard_text() {
                        let clean_path = trg_path.trim().trim_end_matches('\\').to_string();
                        if !clean_path.is_empty() && Path::new(&clean_path).is_dir() {
                            results.push(TotalCmdFolder {
                                path: clean_path,
                                title: "Total Commander (副窗格)".to_string(),
                                is_active: false,
                                hwnd,
                            });
                        }
                    }
                }
                None => any_timeout = true,
            }

            // 第一次超时即视为该管理器已挂起：立即中止整轮扫描并进入冷却，
            // 不把剩余窗口/面板的等待时间叠成秒级主线程卡顿
            if any_timeout {
                break;
            }

            hwnd = FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null()).unwrap_or_default();
        }

        // 扫描完成，立即恢复用户原始剪贴板（恢复失败仅记录，不影响扫描结果）
        if let Err(err) = guard.restore_now() {
            crate::dialog::detector::log_debug(&format!(
                "CLIPBOARD_RESTORE_FAILED(totalcmd): {:?}",
                err
            ));
        }

        if any_timeout {
            crate::dialog::detector::log_debug("IPC: Total Commander 响应超时，进入冷却");
            ipc::report_failure(Manager::TotalCmd);
        } else if any_delivered {
            ipc::report_success(Manager::TotalCmd);
        }
    }

    results
}
