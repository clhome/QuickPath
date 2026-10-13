#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;

use super::ipc::{self, Manager};

/// dopus.lister 窗口必须属于以下进程镜像之一，否则视为同类名伪装窗口，不发送协议消息
const EXPECTED_PROCESS_NAMES: [&str; 1] = ["dopus.exe"];

#[derive(Debug, Clone)]
pub struct DopusFolder {
    pub path: String,
    pub title: String,
    pub is_active: bool,
    pub hwnd: HWND,
}

/// 扫描系统中所有运行的 Directory Opus 窗口及其打开的路径
pub fn get_dopus_folders() -> Vec<DopusFolder> {
    let mut results = Vec::new();

    // 冷却窗口内不重试（上次超时/失败后避免每次前台切换都白等一个超时）
    if !ipc::try_enter(Manager::Dopus) {
        return results;
    }

    let class_name: Vec<u16> = "dopus.lister".encode_utf16().chain(Some(0)).collect();
    let class_pcwstr = PCWSTR(class_name.as_ptr());

    unsafe {
        let hwnd = FindWindowExW(None, None, class_pcwstr, PCWSTR::null()).unwrap_or_default();
        if hwnd.0.is_null() {
            return results;
        }

        // 进程镜像核对：只对真正的 dopus.exe 窗口发送协议消息
        if !ipc::window_process_matches(hwnd, &EXPECTED_PROCESS_NAMES) {
            crate::dialog::detector::log_debug(
                "IPC: 命中 dopus.lister 类名但进程镜像不符，已跳过（疑似伪装窗口）",
            );
            ipc::report_failure(Manager::Dopus);
            return results;
        }

        let temp_dir = std::env::temp_dir();
        let info_file = temp_dir.join("quickpath_dopus.xml");

        // 尝试找到 dopusrt.exe（可信绝对路径）
        let Some(dopusrt) = find_dopusrt() else {
            ipc::report_failure(Manager::Dopus);
            return results;
        };

        let info_arg = format!("\"{}\",paths", info_file.display());
        // 有界执行：dopusrt 卡住时直接 kill，绝不无限等待
        if !ipc::run_bounded(&dopusrt, &["/info".to_string(), info_arg]) {
            crate::dialog::detector::log_debug("IPC: dopusrt.exe 超时或被终止，进入冷却");
            ipc::report_failure(Manager::Dopus);
            let _ = fs::remove_file(&info_file);
            return results;
        }

        let mut parsed = false;
        if info_file.exists() {
            if let Ok(content) = fs::read_to_string(&info_file) {
                parse_dopus_xml(&content, hwnd, &mut results);
                parsed = true;
            }
            let _ = fs::remove_file(&info_file);
        }

        if parsed {
            ipc::report_success(Manager::Dopus);
        } else {
            ipc::report_failure(Manager::Dopus);
        }
    }

    results
}

/// 定位可信的 dopusrt.exe。
///
/// 审计报告 S-01：绝不再搜索用户 `PATH` 或当前工作目录——在“最高权限自启”模式下，
/// 同名可执行文件会被高权限进程拉起。只接受：
/// 1. 注册表登记的官方安装目录（且必须位于受信安装根目录内）；
/// 2. 受信 Program Files 目录下的标准安装路径。
fn find_dopusrt() -> Option<PathBuf> {
    use crate::win32::trusted::{is_trusted_install_dir, trusted_roots};

    // 1. 注册表安装目录优先（官方安装信息，非用户可写的 PATH）
    if let Some(dir) = registry_install_dir() {
        let candidate = dir.join("dopusrt.exe");
        if candidate.is_file() && is_trusted_install_dir(&candidate) {
            return Some(candidate);
        }
    }

    // 2. 受信 Program Files 目录下的标准安装路径
    for root in trusted_roots() {
        let candidate = root
            .join("GPSoftware")
            .join("Directory Opus")
            .join("dopusrt.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// 读取注册表中的 Directory Opus 安装目录（HKLM 优先，HKCU 兜底；兼容 32 位视图）
fn registry_install_dir() -> Option<PathBuf> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    const SUB_KEYS: [&str; 3] = [
        r"SOFTWARE\GPSoftware\Directory Opus",
        r"SOFTWARE\WOW6432Node\GPSoftware\Directory Opus",
        r"SOFTWARE\GPSoftware\Directory Opus\Install",
    ];
    let value_name = HSTRING::from("InstallDir");

    unsafe {
        for sub_key in SUB_KEYS {
            let sub_key = HSTRING::from(sub_key);
            for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
                let mut buf = [0u16; 1024];
                let mut size = (buf.len() * 2) as u32;
                let status = RegGetValueW(
                    root,
                    &sub_key,
                    &value_name,
                    RRF_RT_REG_SZ,
                    None,
                    Some(buf.as_mut_ptr() as *mut _),
                    Some(&mut size),
                );
                if status == ERROR_SUCCESS && size > 2 {
                    // size 含结尾 NUL
                    let chars = (size as usize / 2).saturating_sub(1);
                    let raw = String::from_utf16_lossy(&buf[..chars]);
                    let trimmed = raw.trim();
                    if !trimmed.is_empty() {
                        return Some(PathBuf::from(trimmed));
                    }
                }
            }
        }
    }

    None
}

/// 解析 dopusinfo.xml 中的路径和 tab 状态
fn parse_dopus_xml(xml: &str, hwnd: HWND, results: &mut Vec<DopusFolder>) {
    // 简易安全解析，匹配 <path tab_state="1"> 或 <path tab_state="2">
    for line in xml.lines() {
        if line.contains("<path") && line.contains("</path>") {
            let is_active = line.contains("tab_state=\"1\"");
            if let Some(start) = line.find('>') {
                if let Some(end) = line.rfind("</path>") {
                    if start + 1 < end {
                        let path_str = line[start + 1..end].trim();
                        if !path_str.is_empty() && std::path::Path::new(path_str).is_dir() {
                            results.push(DopusFolder {
                                path: path_str.to_string(),
                                title: if is_active {
                                    "Directory Opus (活动标签)".to_string()
                                } else {
                                    "Directory Opus".to_string()
                                },
                                is_active,
                                hwnd,
                            });
                        }
                    }
                }
            }
        }
    }
}
