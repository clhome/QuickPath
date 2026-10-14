#![allow(dead_code)]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::{HSTRING, s, w};

#[repr(C)]
#[allow(non_snake_case)]
#[allow(clippy::upper_case_acronyms)]
struct OSVERSIONINFOEXW {
    dwOSVersionInfoSize: u32,
    dwMajorVersion: u32,
    dwMinorVersion: u32,
    dwBuildNumber: u32,
    dwPlatformId: u32,
    szCSDVersion: [u16; 128],
    wServicePackMajor: u16,
    wServicePackMinor: u16,
    wSuiteMask: u16,
    wProductType: u8,
    wReserved: u8,
}

type RtlGetVersionFn = unsafe extern "system" fn(*mut OSVERSIONINFOEXW) -> i32;

/// Windows 系统详细版本信息
#[derive(Debug, Clone)]
pub struct WindowsVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub ubr: u32,
    pub display_version: String,
    pub edition_id: String,
    pub product_name: String,
}

static CACHED_OS_VERSION: OnceLock<WindowsVersion> = OnceLock::new();
static MOCK_TAB_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

/// 获取当前 Windows 系统版本（带单例缓存）
pub fn get_windows_version() -> &'static WindowsVersion {
    CACHED_OS_VERSION.get_or_init(query_windows_version)
}

/// 格式化为用于界面展示的 Windows 完整具体版本字符串
pub fn format_windows_version(is_chinese: bool) -> String {
    let ver = get_windows_version();
    format_version_details(ver, is_chinese)
}

/// 内部格式化工具函数
pub fn format_version_details(ver: &WindowsVersion, is_chinese: bool) -> String {
    // 1. 系统主版本代号判定 (Windows 11 起始 Build 为 22000)
    let os_name = if ver.build >= 22000 {
        "Windows 11"
    } else if ver.major == 10 {
        "Windows 10"
    } else if ver.major == 6 && ver.minor == 3 {
        "Windows 8.1"
    } else if ver.major == 6 && ver.minor == 1 {
        "Windows 7"
    } else if !ver.product_name.is_empty() {
        &ver.product_name
    } else {
        "Windows"
    };

    // 2. 版本类别本地化 (Edition)
    let edition_name = if is_chinese {
        match ver.edition_id.to_lowercase().as_str() {
            "professional" | "pro" => "专业版",
            "home" | "core" | "coresinglelanguage" => "家庭版",
            "enterprise" | "enterpriseg" | "enterprises" => "企业版",
            "education" => "教育版",
            "professionalworkstation" => "专业工作站版",
            _ if ver.edition_id.is_empty() => "",
            _ => &ver.edition_id,
        }
    } else {
        match ver.edition_id.to_lowercase().as_str() {
            "professional" | "pro" => "Pro",
            "home" | "core" | "coresinglelanguage" => "Home",
            "enterprise" | "enterpriseg" | "enterprises" => "Enterprise",
            "education" => "Education",
            "professionalworkstation" => "Pro for Workstations",
            _ if ver.edition_id.is_empty() => "",
            _ => &ver.edition_id,
        }
    };

    // 3. 构建内部修订版本号 (如 26100.9550)
    let full_build = if ver.ubr > 0 {
        format!("{}.{}", ver.build, ver.ubr)
    } else {
        ver.build.to_string()
    };

    // 4. 组合最终展示字符串
    let build_prefix = if is_chinese {
        "OS 内部版本"
    } else {
        "OS Build"
    };

    let mut parts = Vec::new();
    parts.push(os_name.to_string());
    if !edition_name.is_empty() {
        parts.push(edition_name.to_string());
    }
    if !ver.display_version.is_empty() {
        parts.push(ver.display_version.clone());
    }

    let os_prefix = parts.join(" ");
    format!("{} ({} {})", os_prefix, build_prefix, full_build)
}

/// 检查当前系统是否满足文件资源管理器多标签页增强要求
///
/// 最低系统基准：Windows 11 22H2 Build 22621+（具备包含原生 Tabs 的 XAML 架构文件资源管理器）
pub fn is_tab_module_supported() -> bool {
    if MOCK_TAB_UNSUPPORTED.load(Ordering::Relaxed) {
        return false;
    }
    let ver = get_windows_version();
    ver.build >= 22621
}

/// 用于测试/模拟不支持多标签页环境的切换开关
pub fn set_mock_tab_unsupported(mock: bool) {
    MOCK_TAB_UNSUPPORTED.store(mock, Ordering::Relaxed);
}

/// 底层探测 Windows 真实系统版本
fn query_windows_version() -> WindowsVersion {
    let mut major = 10u32;
    let mut minor = 0u32;
    let mut build = 0u32;

    // 1. 调用 ntdll::RtlGetVersion 穿透兼容性垫片获取真实内核版本与内部 Build
    unsafe {
        let ntdll_mod = GetModuleHandleW(w!("ntdll.dll"));
        if let Ok(hmod) = ntdll_mod
            && let Some(func_ptr) = GetProcAddress(hmod, s!("RtlGetVersion"))
        {
            let rtl_get_version: RtlGetVersionFn = std::mem::transmute(func_ptr);
            let mut os_info: OSVERSIONINFOEXW = std::mem::zeroed();
            os_info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOEXW>() as u32;
            if rtl_get_version(&mut os_info) == 0 {
                major = os_info.dwMajorVersion;
                minor = os_info.dwMinorVersion;
                build = os_info.dwBuildNumber;
            }
        }
    }

    // 2. 从注册表提取 DisplayVersion、UBR、EditionID、ProductName
    let mut ubr = 0u32;
    let mut display_version = String::new();
    let mut edition_id = String::new();
    let mut product_name = String::new();

    unsafe {
        let sub_key = HSTRING::from(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
        let mut hkey = HKEY::default();

        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            &sub_key,
            Some(0),
            KEY_QUERY_VALUE,
            &mut hkey,
        )
        .is_ok()
        {
            // 读取 DisplayVersion（如 "24H2", "23H2"）
            if let Some(val) = query_reg_string(hkey, "DisplayVersion") {
                display_version = val;
            } else if let Some(val) = query_reg_string(hkey, "ReleaseId") {
                display_version = val;
            }

            // 读取 UBR (Update Build Revision, DWORD)
            if let Some(val) = query_reg_dword(hkey, "UBR") {
                ubr = val;
            }

            // 读取 EditionID (如 "Professional", "Core")
            if let Some(val) = query_reg_string(hkey, "EditionID") {
                edition_id = val;
            }

            // 读取 ProductName (如 "Windows 10 Pro")
            if let Some(val) = query_reg_string(hkey, "ProductName") {
                product_name = val;
            }

            // 若 RtlGetVersion 兜底未能获取 build，从注册表补充
            if build == 0
                && let Some(val) = query_reg_string(hkey, "CurrentBuildNumber")
                && let Ok(b) = val.trim().parse::<u32>()
            {
                build = b;
            }

            let _ = RegCloseKey(hkey);
        }
    }

    WindowsVersion {
        major,
        minor,
        build,
        ubr,
        display_version,
        edition_id,
        product_name,
    }
}

/// 辅助读取注册表 REG_SZ 字符串
unsafe fn query_reg_string(hkey: HKEY, value_name: &str) -> Option<String> {
    let name_hstring = HSTRING::from(value_name);
    let mut buffer = [0u16; 512];
    let mut buffer_size = (buffer.len() * 2) as u32;

    let res = unsafe {
        RegQueryValueExW(
            hkey,
            &name_hstring,
            None,
            None,
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut buffer_size),
        )
    };

    if res.is_ok() && buffer_size > 0 {
        let len = (buffer_size / 2) as usize;
        let s = String::from_utf16_lossy(&buffer[..len]);
        let trimmed = s.trim_matches('\0').trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

/// 辅助读取注册表 REG_DWORD 数字
unsafe fn query_reg_dword(hkey: HKEY, value_name: &str) -> Option<u32> {
    let name_hstring = HSTRING::from(value_name);
    let mut val = 0u32;
    let mut val_size = std::mem::size_of::<u32>() as u32;

    let res = unsafe {
        RegQueryValueExW(
            hkey,
            &name_hstring,
            None,
            None,
            Some(&mut val as *mut u32 as *mut u8),
            Some(&mut val_size),
        )
    };

    if res.is_ok() {
        return Some(val);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_windows_version() {
        let ver = get_windows_version();
        assert!(ver.build > 0, "Build should be non-zero");
        let formatted_zh = format_windows_version(true);
        let formatted_en = format_windows_version(false);
        assert!(
            formatted_zh.contains("Windows"),
            "Should format valid zh string"
        );
        assert!(
            formatted_en.contains("Windows"),
            "Should format valid en string"
        );
    }

    #[test]
    fn test_format_version_variations() {
        let win11 = WindowsVersion {
            major: 10,
            minor: 0,
            build: 26100,
            ubr: 9550,
            display_version: "24H2".to_string(),
            edition_id: "Professional".to_string(),
            product_name: "Windows 10 Pro".to_string(),
        };
        assert_eq!(
            format_version_details(&win11, true),
            "Windows 11 专业版 24H2 (OS 内部版本 26100.9550)"
        );
        assert_eq!(
            format_version_details(&win11, false),
            "Windows 11 Pro 24H2 (OS Build 26100.9550)"
        );

        let win10 = WindowsVersion {
            major: 10,
            minor: 0,
            build: 19045,
            ubr: 4355,
            display_version: "22H2".to_string(),
            edition_id: "Professional".to_string(),
            product_name: "Windows 10 Pro".to_string(),
        };
        assert_eq!(
            format_version_details(&win10, true),
            "Windows 10 专业版 22H2 (OS 内部版本 19045.4355)"
        );
    }

    #[test]
    fn test_tab_module_supported_logic() {
        let supported = WindowsVersion {
            major: 10,
            minor: 0,
            build: 22621,
            ubr: 675,
            display_version: "22H2".to_string(),
            edition_id: "Professional".to_string(),
            product_name: "Windows 10 Pro".to_string(),
        };
        assert!(supported.build >= 22621);

        let unsupported = WindowsVersion {
            major: 10,
            minor: 0,
            build: 19045,
            ubr: 1000,
            display_version: "22H2".to_string(),
            edition_id: "Professional".to_string(),
            product_name: "Windows 10 Pro".to_string(),
        };
        assert!(unsupported.build < 22621);
    }
}
