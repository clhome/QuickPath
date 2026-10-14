#![allow(dead_code)]

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, WM_COPYDATA};
use windows::core::PCWSTR;

use super::clipboard_probe::{log_probe_abort, log_restore_outcome, probe_two_panels};
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
///
/// **审计需求 `9549648` §2.4 方案 B**：XYplorer 的路径回传只能依赖脚本 `copytext`
/// （即系统剪贴板），而 `WM_CLIPBOARDUPDATE` 在**处理时**才采样 owner/sequence，
/// 无法回溯“更新发生瞬间”的历史来源。因此：
/// - 本探测器**默认关闭**（`AppConfig::enable_xyplorer` 默认 false，存量配置一次性迁移为
///   关闭），只有用户显式开启才会被后台扫描调用；
/// - 归属判据已收回 fail-closed：请求窗口内必须**恰好一次**可归因更新，否则事务返回
///   `AbortedEventHistoryUnprovable`，既不认领结果也不恢复旧快照；
/// - 不得把“事件环 + 最终 owner”当作“请求期间无第三方更新”的证明。
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

        // 进程镜像核对：只对真正的 XYplorer 窗口发送 WM_COPYDATA。
        // 首个命中窗口不可信时整轮放弃并进入冷却；其后的每个枚举窗口在循环内同样单独核对
        // （同类名伪装窗口可能出现在枚举链的**任意位置**）。
        if !ipc::window_process_matches(hwnd, &EXPECTED_PROCESS_NAMES) {
            crate::dialog::detector::log_debug(
                "IPC: 命中 ThunderRT6FormDC 类名但进程镜像不符，已跳过（疑似伪装窗口）",
            );
            ipc::report_failure(Manager::XYplorer);
            return results;
        }

        // 安全守卫：在枚举前开启剪贴板事务（完整快照，含文件列表 CF_HDROP、截图位图等）。
        // 若剪贴板被占用或关键格式无法保真备份，宁可放弃扫描，绝不破坏用户剪贴板！
        // 注意：临时清空/写入/恢复全部走统一原子原语（持锁期内复核 sequence），
        // 切换归属不可证明时一律 fail-closed，绝不用旧快照覆盖用户当前内容。
        let mut tx = match ClipboardTransaction::begin() {
            Ok(tx) => tx,
            Err(_) => return results,
        };

        let mut any_delivered = false;
        let mut any_timeout = false;

        while !hwnd.0.is_null() {
            // 逐窗口核对进程镜像（含首个窗口，代价仅为一次轻量进程信息查询）：
            // 只核对首个窗口会漏掉枚举链中后续的同类名伪装窗口，照样向它投递 WM_COPYDATA。
            // 命中的伪装窗口一律跳过，绝不投递任何管理器协议消息。
            if !ipc::window_process_matches(hwnd, &EXPECTED_PROCESS_NAMES) {
                crate::dialog::detector::log_debug(
                    "IPC: 命中 ThunderRT6FormDC 类名但进程镜像不符，已跳过（疑似伪装窗口）",
                );
                hwnd = FindWindowExW(None, Some(hwnd), class_pcwstr, PCWSTR::null())
                    .unwrap_or_default();
                continue;
            }

            // 目标窗口进程 id：仅用于**排除**明显的外部来源（非目标进程的更新直接判
            // `NotOwned`）。**不得**把它当成“请求期间无第三方更新”的证明——处理时采样
            // 无法回溯历史 owner；真正的 fail-closed 判据在事务内部（请求窗口恰好一次更新）。
            let target_pid = ipc::window_process_id(hwnd);

            // 1./2. 依次探测活动窗格与非活动窗格。
            //    事务内部会先清空剪贴板：XY 不响应时绝不会把上一次的陈旧路径当成结果读回来。
            //    P-05：首个面板超时即短路，不再付出第二次 500ms。
            match probe_two_panels(
                &mut tx,
                |pid| pid != 0 && pid == target_pid,
                || send_xyplorer_script(hwnd, "::copytext get('path', a);"),
                || send_xyplorer_script(hwnd, "::copytext get('path', i);"),
            ) {
                Ok((first_panel, second_panel)) => {
                    if first_panel.delivered {
                        any_delivered = true;
                    } else {
                        any_timeout = true;
                    }
                    if let Some(path) = first_panel.path {
                        results.push(XYplorerFolder {
                            path,
                            title: "XYplorer (活动窗格)".to_string(),
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
                }
                Err(err) => {
                    // 检出外部改写：立即中止整轮扫描，绝不覆盖用户新复制的内容
                    log_probe_abort("xyplorer", &err);
                    break;
                }
            }

            // 任一面板超时即视为该管理器已挂起：立即中止整轮扫描并进入冷却
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
