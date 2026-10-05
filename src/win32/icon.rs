#![allow(dead_code)]

use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadIconW, LoadImageW, HICON, IDI_APPLICATION, IMAGE_ICON,
    LR_DEFAULTCOLOR, LR_SHARED, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON,
};

/// 智能获取应用专属 Logo 图标
/// small = true: 小图标（系统托盘、窗口左上角徽标，自适应 16x16 / 32x32）
/// small = false: 大图标（任务栏切换徽标，自适应 32x32 / 48x48）
pub fn get_app_icon(small: bool) -> HICON {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let (cx, cy) = if small {
            (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON))
        } else {
            (GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON))
        };

        if let Ok(handle) = LoadImageW(
            Some(HINSTANCE(hinstance.0)),
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR | LR_SHARED,
        ) {
            if !handle.0.is_null() {
                return HICON(handle.0);
            }
        }

        // 回退备选为系统默认应用图标
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}
