//! 可信路径工具（审计报告 S-01）
//!
//! 背景：当 QuickPath 以“最高权限自启”（任务计划程序 `/rl highest`）运行时，
//! 任何以**裸程序名**启动的外部程序都会被 Windows 按 `PATH` 搜索，
//! 用户 `PATH` 或当前工作目录下的同名恶意程序就可能被高权限进程拉起。
//!
//! 因此本模块提供两个基础能力，供 autostart / managers 等模块复用：
//! 1. `system32_tool`：只接受 System32 下的系统工具绝对路径，绝不回退 PATH 搜索；
//! 2. `is_trusted_install_dir`：判定路径是否位于受信安装根目录（Windows / Program Files），
//!    这些根目录来自系统 API（而非可被用户环境变量覆盖的值）。

use std::path::{Path, PathBuf};

/// 解析 System32 下的系统工具绝对路径；文件不存在或名字含路径分隔符则返回 None。
/// 绝不回退到 PATH 搜索。
pub fn system32_tool(file_name: &str) -> Option<PathBuf> {
    if !is_plain_file_name(file_name) {
        return None;
    }
    let candidate = system_directory()?.join(file_name);
    candidate.is_file().then_some(candidate)
}

/// 解析“系统目录内”的可执行文件绝对路径：先 System32，再 Windows 目录
/// （`explorer.exe` 位于 Windows 目录而非 System32，但两者均为受信根目录）。
/// 均不存在则返回 None，绝不回退 PATH 搜索。
pub fn system_tool(file_name: &str) -> Option<PathBuf> {
    if !is_plain_file_name(file_name) {
        return None;
    }
    let mut dirs = Vec::new();
    if let Some(dir) = system_directory() {
        dirs.push(dir);
    }
    if let Some(dir) = windows_directory() {
        dirs.push(dir);
    }
    dirs.into_iter()
        .map(|dir| dir.join(file_name))
        .find(|candidate| candidate.is_file())
}

/// Windows 目录（形如 `C:\Windows`），取自已知文件夹 API，不依赖环境变量
pub fn windows_directory() -> Option<PathBuf> {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Windows)
}

/// 系统目录（形如 `C:\Windows\System32`），取自系统 API，不依赖环境变量
pub fn system_directory() -> Option<PathBuf> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    /// 防御性上限：正常 Windows 系统目录长度远小于此值
    const MAX_PATH_CHARS: usize = 4096;

    unsafe {
        let mut buf = vec![0u16; 260];
        loop {
            let len = GetSystemDirectoryW(Some(&mut buf)) as usize;
            if len == 0 {
                return None;
            }
            if len >= buf.len() {
                if len > MAX_PATH_CHARS {
                    return None;
                }
                buf.resize(len + 1, 0);
                continue;
            }
            return Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
        }
    }
}

/// 受信根目录：Windows 目录 + 系统 Program Files / Program Files (x86)
///
/// 全部来自系统 API，避免使用可被用户会话覆盖的环境变量。
pub fn trusted_roots() -> Vec<PathBuf> {
    use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86};

    let mut roots = Vec::new();
    if let Some(windows_dir) = windows_directory() {
        roots.push(windows_dir);
    }
    if let Some(p) = known_folder(&FOLDERID_ProgramFiles) {
        roots.push(p);
    }
    if let Some(p) = known_folder(&FOLDERID_ProgramFilesX86) {
        roots.push(p);
    }
    roots
}

/// 判定路径（文件或目录）是否位于受信安装根目录内
pub fn is_trusted_install_dir(path: &Path) -> bool {
    let candidate = normalize_for_compare(path);
    if candidate.is_empty() {
        return false;
    }
    trusted_roots()
        .iter()
        .any(|root| is_under_root(&candidate, &normalize_for_compare(root)))
}

fn known_folder(folder_id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    unsafe {
        let raw = SHGetKnownFolderPath(folder_id, KF_FLAG_DEFAULT, None).ok()?;
        let value = raw.to_string().ok();
        // 无论成败都要释放 API 分配的字符串
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        value.map(PathBuf::from)
    }
}

/// 仅接受“纯文件名”：无路径分隔符、无盘符、无目录成分
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', '/', ':'])
        && Path::new(name).components().count() == 1
}

/// 纯字符串规范化：统一小写与分隔符、剥掉 `\\?\` 前缀、去掉尾部多余分隔符
fn normalize_for_compare(path: &Path) -> String {
    let mut s = path.to_string_lossy().to_lowercase().replace('/', "\\");
    if let Some(rest) = s.strip_prefix("\\\\?\\") {
        s = rest.to_string();
    }
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

/// 纯判定：`path` 是否等于 `root` 或位于 `root` 之下（按目录边界比较，
/// 避免 `c:\foobar` 被 `c:\foo` 误判为受信路径）
fn is_under_root(path: &str, root: &str) -> bool {
    if path.is_empty() || root.is_empty() {
        return false;
    }
    if path == root {
        return true;
    }
    let mut prefix = root.to_string();
    if !prefix.ends_with('\\') {
        prefix.push('\\');
    }
    path.starts_with(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_under_root_respects_directory_boundary() {
        assert!(is_under_root(
            "c:\\program files\\dopus\\dopusrt.exe",
            "c:\\program files"
        ));
        assert!(is_under_root("c:\\program files", "c:\\program files"));
        assert!(is_under_root("c:\\windows\\system32\\schtasks.exe", "c:\\"));
        // 目录边界：不得把 C:\Program FilesEvil 当成 Program Files 之内
        assert!(!is_under_root(
            "c:\\program filesevil\\dopusrt.exe",
            "c:\\program files"
        ));
        assert!(!is_under_root(
            "d:\\program files\\x.exe",
            "c:\\program files"
        ));
        assert!(!is_under_root("", "c:\\program files"));
        assert!(!is_under_root("c:\\program files", ""));
    }

    #[test]
    fn test_normalize_for_compare_unifies_separators_and_prefix() {
        let a = normalize_for_compare(Path::new(r"\\?\C:\Program Files\GPSoftware\"));
        let b = normalize_for_compare(Path::new("C:/Program Files/GPSoftware"));
        assert_eq!(a, b);
        assert_eq!(normalize_for_compare(Path::new("C:\\")), "c:\\");
    }

    #[test]
    fn test_is_plain_file_name_rejects_paths() {
        assert!(is_plain_file_name("schtasks.exe"));
        assert!(!is_plain_file_name(r"..\schtasks.exe"));
        assert!(!is_plain_file_name(r"C:\Windows\System32\schtasks.exe"));
        assert!(!is_plain_file_name("sub/dopusrt.exe"));
        assert!(!is_plain_file_name(""));
    }

    #[test]
    fn test_system_and_windows_directories_are_inside_trusted_roots() {
        let sys = system_directory().expect("必须能取得系统目录");
        assert!(sys.is_absolute(), "系统目录必须是绝对路径: {:?}", sys);
        assert!(is_trusted_install_dir(&sys));

        let win = windows_directory().expect("必须能取得 Windows 目录");
        assert!(win.is_absolute(), "Windows 目录必须是绝对路径: {:?}", win);
        assert!(is_trusted_install_dir(&win));

        // 工具名含路径分隔符时一律拒绝（防止拼接越界到用户可控路径）
        assert!(system_tool(r"..\evil.exe").is_none());
        assert!(system32_tool(r"..\evil.exe").is_none());
    }

    #[test]
    fn test_system_tool_resolves_real_system_binaries() {
        // Win+E 兑底与自启依赖这两个系统程序：必须能解析到受信绝对路径，
        // 否则说明受信路径判定过严（会导致“按键被吞但无任何结果”）。
        assert!(
            system32_tool("schtasks.exe").is_some(),
            "schtasks.exe 必须能在系统目录中解析到"
        );
        assert!(
            system_tool("explorer.exe").is_some(),
            "explorer.exe 必须能在系统目录中解析到"
        );
        assert!(system_tool("definitely_not_exist_qp_probe.exe").is_none());
    }

    #[test]
    fn test_trusted_roots_are_absolute_and_never_user_writable_data_dirs() {
        let roots = trusted_roots();
        assert!(!roots.is_empty(), "系统必须能给出至少一个受信根目录");
        for root in &roots {
            assert!(root.is_absolute(), "受信根目录必须是绝对路径: {:?}", root);
            let lower = root.to_string_lossy().to_lowercase();
            assert!(
                !lower.contains("appdata") && !lower.contains("\\temp"),
                "受信根目录不得是用户可写目录: {:?}",
                root
            );
        }
        // 用户目录/相对路径一律不受信
        assert!(!is_trusted_install_dir(Path::new(
            r"C:\Users\Public\dopusrt.exe"
        )));
        assert!(!is_trusted_install_dir(Path::new("dopusrt.exe")));
        assert!(!is_trusted_install_dir(Path::new(
            r"C:\ProgramData\evil\dopusrt.exe"
        )));
    }
}
