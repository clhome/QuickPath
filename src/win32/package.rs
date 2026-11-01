#![allow(dead_code)]

//! Windows 应用包身份（Package Identity）探测模块。
//!
//! 用于在运行时识别 QuickPath 是否运行于 MSIX / Windows Store 等具备 Package Identity 的应用容器中。
//! - 若运行于 MSIX 商店包内：关闭应用内部的 GitHub 自更新检测，由 Windows 微软商店管道负责静默安全更新。
//! - 若运行于独立 EXE 便携版或普通 MSI 安装版：保持原生 GitHub Release 更新检测与托盘提醒。

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;

/// APPMODEL_ERROR_NO_PACKAGE (15700 / 0x3D54): 进程不具备包身份（如非 MSIX 运行）。
const APPMODEL_ERROR_NO_PACKAGE_CODE: u32 = 15700;

/// 检测当前进程是否运行于具备 Package Identity 的打包容器（如 MSIX / 微软商店）中。
///
/// 遵循微软官方桌面桥接（Desktop Bridge / MSIX）规范：
/// 通过 `GetCurrentPackageFullName` 查询包全名，仅需探测返回状态码，无需实际分配大块字符缓冲区。
pub fn is_packaged() -> bool {
    let mut length = 0u32;
    // 传入 None 探测包身份：
    // - 若具备包身份，系统返回 ERROR_INSUFFICIENT_BUFFER 并填入实际所需长度。
    // - 若无包身份（普通便携版、MSI 传统安装版），系统返回 APPMODEL_ERROR_NO_PACKAGE (15700)。
    let status = unsafe { GetCurrentPackageFullName(&mut length, None) };
    if status == WIN32_ERROR(APPMODEL_ERROR_NO_PACKAGE_CODE) {
        false
    } else {
        status == ERROR_SUCCESS || status == ERROR_INSUFFICIENT_BUFFER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_packaged_returns_bool_without_panic() {
        // 在开发机普通单元测试环境中，测试宿主进程一般非 MSIX 容器，应平稳返回且不发生 panic。
        let packaged = is_packaged();
        // 验证执行确定性：多次连续调用结果必须一致
        assert_eq!(packaged, is_packaged());
    }

    #[test]
    fn test_appmodel_error_constant() {
        assert_eq!(APPMODEL_ERROR_NO_PACKAGE_CODE, 15700);
    }
}
