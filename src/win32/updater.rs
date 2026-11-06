#![allow(dead_code)]

//! GitHub Release 启动版本检测引擎。
//!
//! 基于 Windows 原生 WinHttp API 异步查询 GitHub releases，
//! 严格控制 5 秒超时，遵循“访问失败立即静默放弃，绝不重试，绝不打扰用户”原则。

use std::ptr;
use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_ACCESS_TYPE_NAMED_PROXY,
    WINHTTP_CURRENT_USER_IE_PROXY_CONFIG, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_PROXY,
    WINHTTP_OPTION_SECURITY_FLAGS, WINHTTP_PROXY_INFO, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpGetIEProxyConfigForCurrentUser, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::core::{PCWSTR, w};

/// 官方 Release 页面兜底下载地址
pub const DEFAULT_RELEASES_URL: &str = "https://github.com/clhome/WindPath/releases";

/// GitHub API Release 简要信息
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub html_url: Option<String>,
}

/// 版本检测结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckResult {
    pub latest_version: String,
    pub download_url: String,
}

/// 解析语义版本号 (major, minor, patch, build)。
///
/// 容忍前导 `v`/`V`，截断任何 pre-release（如 `-beta`）或 build 元数据。
pub fn parse_version(v: &str) -> Option<(u64, u64, u64, u64)> {
    let trimmed = v.trim().trim_start_matches(['v', 'V']);
    let core = trimmed.split(['-', '+', ' ']).next()?;
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let build: u64 = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch, build))
}

/// 判断远程版本是否严格高于本地版本。
pub fn is_newer_version(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

/// RAII 守卫：确保 HINTERNET 句柄在离开作用域时调用 WinHttpCloseHandle 关闭。
struct WinHttpHandle(*mut core::ffi::c_void);

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// 尝试读取当前用户的系统/IE 代理配置并应用到 WinHttp 会话。
///
/// Windows 手动设置的代理（或代理客户端开启的系统代理）存储于 WinINet 配置中。
/// WinHttp 默认不读取该配置，本函数自动继承并挂载，同时以 RAII 守卫确保释放系统分配的堆内存。
unsafe fn apply_current_user_ie_proxy(session: *mut core::ffi::c_void) {
    let mut ie_proxy = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    unsafe {
        if WinHttpGetIEProxyConfigForCurrentUser(&mut ie_proxy).is_err() {
            return;
        }
    }

    struct IeProxyGuard(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG);
    impl Drop for IeProxyGuard {
        fn drop(&mut self) {
            unsafe {
                if !self.0.lpszProxy.is_null() {
                    let _ = GlobalFree(Some(HGLOBAL(self.0.lpszProxy.0 as *mut _)));
                }
                if !self.0.lpszProxyBypass.is_null() {
                    let _ = GlobalFree(Some(HGLOBAL(self.0.lpszProxyBypass.0 as *mut _)));
                }
                if !self.0.lpszAutoConfigUrl.is_null() {
                    let _ = GlobalFree(Some(HGLOBAL(self.0.lpszAutoConfigUrl.0 as *mut _)));
                }
            }
        }
    }
    let guard = IeProxyGuard(ie_proxy);

    if !guard.0.lpszProxy.is_null() {
        let proxy_info = WINHTTP_PROXY_INFO {
            dwAccessType: WINHTTP_ACCESS_TYPE_NAMED_PROXY,
            lpszProxy: guard.0.lpszProxy,
            lpszProxyBypass: guard.0.lpszProxyBypass,
        };
        unsafe {
            let _ = WinHttpSetOption(
                Some(session),
                WINHTTP_OPTION_PROXY,
                Some(std::slice::from_raw_parts(
                    &proxy_info as *const _ as *const u8,
                    std::mem::size_of::<WINHTTP_PROXY_INFO>(),
                )),
            );
        }
    }
}

/// 执行单次轻量 HTTP GET 请求，获取 JSON 响应字符串。
///
/// 严格 5 秒超时，任何阶段失败均静默返回 `None`。
fn fetch_latest_release_json() -> Option<String> {
    unsafe {
        // 1. 初始化 WinHttp 会话，使用系统默认代理模式
        let session = WinHttpOpen(
            w!("WindPath-Update-Checker"),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        let _session_guard = WinHttpHandle(session);

        // 尝试继承当前用户的 IE / 系统代理（透明支持本地代理与加速工具）
        apply_current_user_ie_proxy(session);

        // 设置有界超时：解析 3s，连接 5s，发送 5s，接收 5s
        let _ = WinHttpSetTimeouts(session, 3000, 5000, 5000, 5000);

        // 2. 连接 api.github.com HTTPS (443)
        let connect = WinHttpConnect(
            session,
            w!("api.github.com"),
            443, // INTERNET_DEFAULT_HTTPS_PORT
            0,
        );
        if connect.is_null() {
            return None;
        }
        let _connect_guard = WinHttpHandle(connect);

        // 3. 打开 GET 请求
        let request = WinHttpOpenRequest(
            connect,
            w!("GET"),
            w!("/repos/clhome/WindPath/releases/latest"),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            return None;
        }
        let _request_guard = WinHttpHandle(request);

        // 4. 发送请求，附带必要的 GitHub API 请求头
        let headers: Vec<u16> =
            "User-Agent: WindPath-App\r\nAccept: application/vnd.github.v3+json\r\n"
                .encode_utf16()
                .collect();
        let mut sent = WinHttpSendRequest(request, Some(&headers), None, 0, 0, 0);
        if sent.is_err() {
            // 若遇到安全握手错误（例如在国内网络环境下 DigiCert OCSP/CRL 吊销服务器不可达返回 12175: ERROR_WINHTTP_SECURE_FAILURE），
            // 尝试设置忽略吊销校验并重试一次，避免因吊销服务器脱机阻断版本检测。
            const SECURITY_FLAG_IGNORE_REVOCATION: u32 = 0x0000_0080;
            let sec_flags: u32 = SECURITY_FLAG_IGNORE_REVOCATION;
            let _ = WinHttpSetOption(
                Some(request),
                WINHTTP_OPTION_SECURITY_FLAGS,
                Some(std::slice::from_raw_parts(
                    &sec_flags as *const u32 as *const u8,
                    std::mem::size_of::<u32>(),
                )),
            );
            sent = WinHttpSendRequest(request, Some(&headers), None, 0, 0, 0);
        }
        if sent.is_err() {
            return None;
        }

        // 5. 接收响应头
        if WinHttpReceiveResponse(request, ptr::null_mut()).is_err() {
            return None;
        }

        // 6. 验证 HTTP 状态码是否为 200
        let mut status_code: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status_ok = WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status_code as *mut u32 as *mut _),
            &mut size,
            ptr::null_mut(),
        );
        if status_ok.is_err() || status_code != 200 {
            return None;
        }

        // 7. 读取响应数据体（限制上限 64KB，防止异常超大响应占用内存）
        let mut response_bytes = Vec::new();
        const MAX_BYTES: usize = 64 * 1024;

        loop {
            let mut available: u32 = 0;
            if WinHttpQueryDataAvailable(request, &mut available).is_err() || available == 0 {
                break;
            }

            let to_read = (available as usize).min(MAX_BYTES.saturating_sub(response_bytes.len()));
            if to_read == 0 {
                break;
            }

            let mut chunk = vec![0u8; to_read];
            let mut read_bytes: u32 = 0;
            if WinHttpReadData(
                request,
                chunk.as_mut_ptr() as *mut _,
                to_read as u32,
                &mut read_bytes,
            )
            .is_err()
                || read_bytes == 0
            {
                break;
            }

            response_bytes.extend_from_slice(&chunk[..read_bytes as usize]);
            if response_bytes.len() >= MAX_BYTES {
                break;
            }
        }

        String::from_utf8(response_bytes).ok()
    }
}

/// 检查是否有新版本（轻量无干扰静默模式）。
///
/// 若查询失败、解析失败或当前已是最新，静默返回 `None`；
/// 仅当确认存在严格更高版本时，返回新版本标签与直达下载链接。
pub fn check_for_updates_quietly(current_version: &str) -> Option<UpdateCheckResult> {
    let json_text = fetch_latest_release_json()?;
    let release: GitHubRelease = serde_json::from_str(&json_text).ok()?;

    if is_newer_version(&release.tag_name, current_version) {
        let download_url = release
            .html_url
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_RELEASES_URL.to_string());
        Some(UpdateCheckResult {
            latest_version: release.tag_name,
            download_url,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3, 0)));
        assert_eq!(parse_version("V2.0.0"), Some((2, 0, 0, 0)));
        assert_eq!(parse_version("v1.2.4-beta.1"), Some((1, 2, 4, 0)));
        assert_eq!(parse_version("  v0.9.1+build2026 "), Some((0, 9, 1, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0, 0)));
        assert_eq!(parse_version("v1.2.3.4"), Some((1, 2, 3, 4)));
        assert_eq!(parse_version("invalid"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn test_is_newer_version() {
        // 典型正向升级
        assert!(is_newer_version("v1.2.4", "1.2.3"));
        assert!(is_newer_version("v1.2.5", "1.2.4"));
        assert!(is_newer_version("1.2.5", "1.2.4"));
        assert!(is_newer_version("1.3.0", "1.2.3"));
        assert!(is_newer_version("2.0.0", "1.2.3"));
        assert!(is_newer_version("v1.2.3.1", "1.2.3"));

        // 相同版本，不应升级
        assert!(!is_newer_version("1.2.3", "1.2.3"));
        assert!(!is_newer_version("v1.2.3", "1.2.3"));
        assert!(!is_newer_version("v1.2.4", "1.2.4"));
        assert!(!is_newer_version("v1.2.5", "1.2.5"));

        // 本地版本更高或测试版，不应升级
        assert!(!is_newer_version("1.2.2", "1.2.3"));
        assert!(!is_newer_version("1.2.0", "1.2.3"));
        assert!(!is_newer_version("1.2.4", "1.2.5"));
        assert!(!is_newer_version("0.9.9", "1.2.3"));

        // 异常格式输入
        assert!(!is_newer_version("unknown", "1.2.3"));
        assert!(!is_newer_version("v1.2.4", "invalid"));
    }

    #[test]
    fn test_release_json_deserialization() {
        let sample_json = r#"{
            "tag_name": "v1.2.4",
            "html_url": "https://github.com/clhome/WindPath/releases/tag/v1.2.4",
            "name": "Release v1.2.4"
        }"#;
        let release: GitHubRelease = serde_json::from_str(sample_json).unwrap();
        assert_eq!(release.tag_name, "v1.2.4");
        assert_eq!(
            release.html_url.as_deref(),
            Some("https://github.com/clhome/WindPath/releases/tag/v1.2.4")
        );

        // 真实 GitHub API 返回的包含多字段的 payload
        let full_payload = r#"{
            "url": "https://api.github.com/repos/clhome/WindPath/releases/429813443",
            "tag_name": "v1.2.5",
            "target_commitish": "main",
            "name": "WindPath v1.2.5",
            "draft": false,
            "prerelease": false,
            "html_url": "https://github.com/clhome/WindPath/releases/tag/v1.2.5",
            "assets": []
        }"#;
        let full_release: GitHubRelease = serde_json::from_str(full_payload).unwrap();
        assert_eq!(full_release.tag_name, "v1.2.5");
        assert_eq!(
            full_release.html_url.as_deref(),
            Some("https://github.com/clhome/WindPath/releases/tag/v1.2.5")
        );
    }

    #[test]
    fn test_download_url_fallback() {
        let sample_json = r#"{
            "tag_name": "v1.2.4",
            "html_url": ""
        }"#;
        let release: GitHubRelease = serde_json::from_str(sample_json).unwrap();
        let download_url = release
            .html_url
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_RELEASES_URL.to_string());
        assert_eq!(download_url, DEFAULT_RELEASES_URL);
    }

    #[test]
    fn test_update_i18n_messages() {
        use crate::rules::Language;

        let (title_cn, text_cn) = crate::rules::I18n::tray_update_bubble(&Language::ZhCN, "v1.2.4");
        assert!(title_cn.contains("v1.2.4"));
        assert!(text_cn.contains("GitHub"));

        let menu_cn = crate::rules::I18n::tray_new_version(&Language::ZhCN, "v1.2.4");
        assert!(menu_cn.contains("v1.2.4"));
        assert!(menu_cn.contains("点击下载"));

        let (title_en, text_en) = crate::rules::I18n::tray_update_bubble(&Language::EnUS, "v1.2.4");
        assert!(title_en.contains("v1.2.4"));
        assert!(text_en.contains("GitHub"));

        let menu_en = crate::rules::I18n::tray_new_version(&Language::EnUS, "v1.2.4");
        assert!(menu_en.contains("v1.2.4"));
        assert!(menu_en.contains("Click to Download"));
    }
}
