#![allow(dead_code)]

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, WM_COPYDATA};
use windows::core::PCWSTR;

use super::clipboard_probe::{log_probe_abort, log_restore_outcome, probe_panel_path};
use super::ipc::{self, Manager};
use crate::win32::clipboard::ClipboardTransaction;

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

        // P0 安全守卫：在枚举前开启剪贴板事务（完整快照，含文件列表 CF_HDROP、截图位图等）。
        // 若剪贴板被占用或关键格式无法保真备份，宁可放弃扫描，绝不破坏用户剪贴板！
        let mut tx = match ClipboardTransaction::begin() {
            Ok(tx) => tx,
            Err(_) => return results,
        };

        let mut any_delivered = false;
        let mut any_timeout = false;

        while !hwnd.0.is_null() {
            // 1. 获取活动窗格路径
            //    事务内部会先清空剪贴板：XY 不响应时绝不会把上一次的陈旧路径当成结果读回来
            match probe_panel_path(&mut tx, || {
                send_xyplorer_script(hwnd, "::copytext get('path', a);")
            }) {
                Ok(panel) => {
                    if panel.delivered {
                        any_delivered = true;
                    } else {
                        any_timeout = true;
                    }
                    if let Some(path) = panel.path {
                        results.push(XYplorerFolder {
                            path,
                            title: "XYplorer (活动窗格)".to_string(),
                            is_active: true,
                            hwnd,
                        });
                    }
                }
                Err(err) => {
                    // 检出外部改写：立即中止整轮扫描，绝不覆盖用户新复制的内容
                    log_probe_abort("xyplorer", &err);
                    break;
                }
            }

            // 2. 获取非活动窗格路径
            match probe_panel_path(&mut tx, || {
                send_xyplorer_script(hwnd, "::copytext get('path', i);")
            }) {
                Ok(panel) => {
                    if panel.delivered {
                        any_delivered = true;
                    } else {
                        any_timeout = true;
                    }
                    if let Some(path) = panel.path
                        && !results.iter().any(|r| r.path == path)
                    {
                        results.push(XYplorerFolder {
                            path,
                            title: "XYplorer (副窗格)".to_string(),
                            is_active: false,
                            hwnd,
                        });
                    }
                }
                Err(err) => {
                    log_probe_abort("xyplorer", &err);
                    break;
                }
            }

            // 第一次超时即视为该管理器已挂起：立即中止整轮扫描并进入冷却
            if any_timeout {
                break;
            }

            hwnd =
                FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null()).unwrap_or_default();
        }

        // 扫描完成（含提前中止），按同一套严格归属校验恢复用户原始剪贴板
        let outcome = tx.restore_if_still_owned();
        log_restore_outcome("xyplorer", &outcome);

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
