#![allow(dead_code)]

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;

const CREATE_NO_WINDOW: u32 = 0x08000000;

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

    let class_name: Vec<u16> = "dopus.lister".encode_utf16().chain(Some(0)).collect();
    let class_pcwstr = PCWSTR(class_name.as_ptr());

    unsafe {
        let hwnd = FindWindowExW(None, None, class_pcwstr, PCWSTR::null()).unwrap_or_default();
        if hwnd.0.is_null() {
            return results;
        }

        let temp_dir = std::env::temp_dir();
        let info_file = temp_dir.join("quickpath_dopus.xml");

        // 尝试找到 dopusrt.exe
        if let Some(dopusrt) = find_dopusrt() {
            let info_arg = format!("\"{}\",paths", info_file.display());
            let _ = Command::new(&dopusrt)
                .args(["/info", &info_arg])
                .creation_flags(CREATE_NO_WINDOW)
                .status();

            if info_file.exists() {
                if let Ok(content) = fs::read_to_string(&info_file) {
                    parse_dopus_xml(&content, hwnd, &mut results);
                }
                let _ = fs::remove_file(&info_file);
            }
        }
    }

    results
}

fn find_dopusrt() -> Option<PathBuf> {
    // 1. 尝试常见的 Program Files 路径
    let paths = [
        r"C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe",
        r"C:\Program Files (x86)\GPSoftware\Directory Opus\dopusrt.exe",
        r"D:\Program Files\GPSoftware\Directory Opus\dopusrt.exe",
    ];

    for p in paths {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }

    // 2. PATH 环境变量搜索
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("dopusrt.exe");
            if candidate.is_file() {
                return Some(candidate);
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
                        if !path_str.is_empty() && Path::new(path_str).is_dir() {
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
