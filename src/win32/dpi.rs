#![allow(dead_code)]

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;

/// 获取目标窗口的系统 DPI（默认为 96 即 100% 缩放）
pub fn get_window_dpi(hwnd: HWND) -> u32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            96
        } else {
            dpi
        }
    }
}

/// 计算基于 96 DPI 的缩放倍率
pub fn get_scale_factor(hwnd: HWND) -> f32 {
    get_window_dpi(hwnd) as f32 / 96.0
}

/// 将逻辑像素缩放为物理像素
pub fn scale_px(value: i32, scale: f32) -> i32 {
    (value as f32 * scale).round() as i32
}
