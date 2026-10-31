#![allow(dead_code)]

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

/// 检查窗口是否被 DWM Cloak 隐藏（不在当前虚拟桌面，或被系统/应用 Cloaked）
///
/// 纯 Win32 原生系统调用，无需 COM 初始化，微秒级执行，可安全在钩子或任何线程中调用。
pub fn is_window_cloaked(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return true;
    }
    let mut cloaked: u32 = 0;
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if hr.is_ok() {
        cloaked != 0
    } else {
        false
    }
}

/// 权威核验窗口是否位于当前活跃的虚拟桌面
///
/// 结合原生 DWM Cloaked 与 Windows 官方 COM `IVirtualDesktopManager` 双保险核验：
/// 1. 若被 DWM Cloak，必然不在当前活跃虚拟桌面，极速短路返回 false；
/// 2. 尝试通过 `IVirtualDesktopManager::IsWindowOnCurrentVirtualDesktop` 权威判定；
/// 3. 若 COM 无法创建（如调用方线程未初始化 COM）或 API 报错，回退至未 Cloaked 判定。
pub fn is_window_on_current_desktop(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    // 1. 若已被 Cloaked（Windows 虚拟桌面离开时自动打上 DWM_CLOAKED_SHELL），立即返回 false
    if is_window_cloaked(hwnd) {
        return false;
    }

    // 2. 尝试使用 COM 官方 IVirtualDesktopManager 进行权威校验
    unsafe {
        if let Ok(vdm) =
            CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_INPROC_SERVER)
            && let Ok(on_current) = vdm.IsWindowOnCurrentVirtualDesktop(hwnd)
        {
            return on_current.as_bool();
        }
    }

    // 3. Fallback：如果未被 Cloaked 且 COM 不可用，认为在当前桌面
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_hwnd_handled_safely() {
        assert!(is_window_cloaked(HWND(std::ptr::null_mut())));
        assert!(!is_window_on_current_desktop(HWND(std::ptr::null_mut())));
    }
}
