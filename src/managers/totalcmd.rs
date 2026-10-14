#![allow(dead_code)]

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, WM_COMMAND};
use windows::core::PCWSTR;

use super::clipboard_probe::{log_probe_abort, log_restore_outcome, probe_two_panels};
use super::ipc::{self, Manager};
use crate::win32::clipboard::ClipboardTransaction;

const CM_COPY_SRC_PATH_TO_CLIP: usize = 2029;
const CM_COPY_TRG_PATH_TO_CLIP: usize = 2030;

/// TTOTAL_CMD 窗口必须属于以下进程镜像之一，否则视为同类名伪装窗口，不发送协议消息
const EXPECTED_PROCESS_NAMES: [&str; 3] =
    ["totalcmd.exe", "totalcmd64.exe", "totalcmd_unicode.exe"];

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

        // 进程镜像核对：只对真正的 Total Commander 窗口发送 WM_COMMAND。
        // 首个命中窗口不可信时整轮放弃并进入冷却；其后的每个枚举窗口在循环内同样单独核对
        // （同类名伪装窗口可能出现在枚举链的**任意位置**）。
        if !ipc::window_process_matches(hwnd, &EXPECTED_PROCESS_NAMES) {
            crate::dialog::detector::log_debug(
                "IPC: 命中 TTOTAL_CMD 类名但进程镜像不符，已跳过（疑似伪装窗口）",
            );
            ipc::report_failure(Manager::TotalCmd);
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
            // 逐窗口核对进程镜像（含首个窗口，代价仅为一次轻量进程信息查询）：
            // 只核对首个窗口会漏掉枚举链中后续的同类名伪装窗口，照样向它投递 WM_COMMAND。
            // 命中的伪装窗口一律跳过，绝不投递任何管理器协议消息。
            if !ipc::window_process_matches(hwnd, &EXPECTED_PROCESS_NAMES) {
                crate::dialog::detector::log_debug(
                    "IPC: 命中 TTOTAL_CMD 类名但进程镜像不符，已跳过（疑似伪装窗口）",
                );
                hwnd = FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null())
                    .unwrap_or_default();
                continue;
            }

            // 目标来源校验（P0-04 竞态 B）：只认「与目标窗口同进程」的剪贴板更新。
            // 该窗口的进程镜像已在上方逐窗核对，因此这里的 pid 可信。
            let target_pid = ipc::window_process_id(hwnd);

            // 1./2. 依次探测源/当前活动面板与目标/副面板。
            //    事务内部会先清空剪贴板：TM 不响应时绝不会把上一次的陈旧路径当成结果读回来。
            //    P-05：首个面板超时即短路，不再付出第二次 500ms。
            match probe_two_panels(
                &mut tx,
                |pid| pid != 0 && pid == target_pid,
                || {
                    ipc::send_message_timeout(
                        hwnd,
                        WM_COMMAND,
                        WPARAM(CM_COPY_SRC_PATH_TO_CLIP),
                        LPARAM(0),
                    )
                },
                || {
                    ipc::send_message_timeout(
                        hwnd,
                        WM_COMMAND,
                        WPARAM(CM_COPY_TRG_PATH_TO_CLIP),
                        LPARAM(0),
                    )
                },
            ) {
                Ok((first_panel, second_panel)) => {
                    if first_panel.delivered {
                        any_delivered = true;
                    } else {
                        any_timeout = true;
                    }
                    if let Some(path) = first_panel.path {
                        results.push(TotalCmdFolder {
                            path,
                            title: "Total Commander (活动窗格)".to_string(),
                            is_active: true,
                            hwnd,
                        });
                    }
                    if let Some(panel) = second_panel {
                        if panel.delivered {
                            any_delivered = true;
                        } else {
                            any_timeout = true;
                        }
                        if let Some(path) = panel.path {
                            results.push(TotalCmdFolder {
                                path,
                                title: "Total Commander (副窗格)".to_string(),
                                is_active: false,
                                hwnd,
                            });
                        }
                    }
                }
                Err(err) => {
                    // 检出外部改写：立即中止整轮扫描，绝不覆盖用户新复制的内容
                    log_probe_abort("totalcmd", &err);
                    break;
                }
            }

            // 任一面板超时即视为该管理器已挂起：立即中止整轮扫描并进入冷却，
            // 不把剩余窗口的等待时间叠成秒级主线程卡顿
            if any_timeout {
                break;
            }

            hwnd =
                FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null()).unwrap_or_default();
        }

        // 扫描完成（含提前中止），按同一套严格归属校验恢复用户原始剪贴板
        let outcome = tx.restore_if_still_owned();
        log_restore_outcome("totalcmd", &outcome);

        if any_timeout {
            crate::dialog::detector::log_debug("IPC: Total Commander 响应超时，进入冷却");
            ipc::report_failure(Manager::TotalCmd);
        } else if any_delivered {
            ipc::report_success(Manager::TotalCmd);
        }
    }

    results
}
