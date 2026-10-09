#![allow(unsafe_op_in_unsafe_fn)]

use crate::monitor::metrics::MetricsSnapshot;
use crate::rules::{I18n, Language};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    FillRect, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL,
    HDC, HGDIOBJ, TRANSPARENT, BLENDFUNCTION, AC_SRC_ALPHA, AC_SRC_OVER,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, LoadCursorW, RegisterClassW, SetCursor,
    SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE,
    SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_SETCURSOR, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

pub struct MonitorTooltip {
    hwnd: HWND,
    width: i32,
    height: i32,
}

impl MonitorTooltip {
    pub fn new() -> Result<Self, String> {
        let class_name = w!("QuickPath_Monitor_Tooltip_Class");
        unsafe {
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(tooltip_wnd_proc),
                hInstance: HINSTANCE::default(),
                hCursor: cursor,
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let hwnd = match CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                class_name,
                w!("QuickPath Monitor Tooltip"),
                WS_POPUP,
                0,
                0,
                420,
                260,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => return Err("创建 Tooltip 悬浮看板窗口失败".to_string()),
            };

            Ok(Self {
                hwnd,
                width: 420,
                height: 260,
            })
        }
    }

    pub fn show(&mut self, bar_rect: RECT, snapshot: &MetricsSnapshot, lang: &Language, scale: f32, is_dark: bool) {
        let w = (380.0 * scale).round() as i32;
        let h = (240.0 * scale).round() as i32;
        self.width = w;
        self.height = h;

        // 计算定位：位于任务栏上方安全间距 (8px)
        let x = bar_rect.left.max(10);
        let y = (bar_rect.top - h - (8.0 * scale).round() as i32).max(10);

        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE,
            );
            self.render(snapshot, lang, scale, is_dark);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    unsafe fn render(&self, snapshot: &MetricsSnapshot, lang: &Language, scale: f32, is_dark: bool) {
        let width = self.width;
        let height = self.height;

        let screen_dc = windows::Win32::Graphics::Gdi::GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib_bitmap = CreateDIBSection(
            Some(mem_dc),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits_ptr,
            None,
            0,
        );

        if dib_bitmap.is_err() || bits_ptr.is_null() {
            let _ = DeleteDC(mem_dc);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(None, screen_dc);
            return;
        }

        let hbitmap = dib_bitmap.unwrap();
        let old_bmp = SelectObject(mem_dc, HGDIOBJ(hbitmap.0 as _));

        // 填充深色半透明毛玻璃底色
        let pixel_count = (width * height) as usize;
        let pixels = std::slice::from_raw_parts_mut(bits_ptr as *mut u32, pixel_count);
        let alpha = 240u32;
        let (r, g, b) = if is_dark { (30u32, 30u32, 32u32) } else { (248u32, 248u32, 250u32) };
        let premul_r = (r * alpha) / 255;
        let premul_g = (g * alpha) / 255;
        let premul_b = (b * alpha) / 255;
        let bg_pixel = (alpha << 24) | (premul_r << 16) | (premul_g << 8) | premul_b;
        pixels.fill(bg_pixel);

        // 绘制边框与高质感圆角卡片文字
        let bundle = I18n::get_bundle(lang);
        let text_color_title = if is_dark { COLORREF(0x00FFFFFF) } else { COLORREF(0x001A1A1A) };
        let text_color_body = if is_dark { COLORREF(0x00CCCCCC) } else { COLORREF(0x00333333) };
        let _text_color_sub = if is_dark { COLORREF(0x008E8E8E) } else { COLORREF(0x00666666) };

        SetBkMode(mem_dc, TRANSPARENT);

        let font_title = CreateFontW(
            (-13.0 * scale).round() as i32, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );

        let font_text = CreateFontW(
            (-11.0 * scale).round() as i32, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );

        let pad_x = (16.0 * scale).round() as i32;
        let mut cur_y = (12.0 * scale).round() as i32;
        let line_h = (22.0 * scale).round() as i32;

        // 1. 标题
        SelectObject(mem_dc, HGDIOBJ(font_title.0 as _));
        SetTextColor(mem_dc, text_color_title);
        let title_str = if bundle.monitor.tooltip_title.is_empty() {
            "硬件监控详情看板"
        } else {
            &bundle.monitor.tooltip_title
        };
        draw_gdi_line(mem_dc, title_str, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h + (4.0 * scale).round() as i32;

        // 分割线
        let line_rect = RECT {
            left: pad_x,
            top: cur_y,
            right: width - pad_x,
            bottom: cur_y + 1,
        };
        let line_brush = windows::Win32::Graphics::Gdi::CreateSolidBrush(COLORREF(0x00444444));
        FillRect(mem_dc, &line_rect, line_brush);
        let _ = DeleteObject(HGDIOBJ(line_brush.0 as _));
        cur_y += (8.0 * scale).round() as i32;

        // 2. 网速与网卡
        SelectObject(mem_dc, HGDIOBJ(font_text.0 as _));
        SetTextColor(mem_dc, text_color_body);
        let net_line = format!(
            "实时网速: ↑ {:.1} Mbps ({:.2} MB/s)  ↓ {:.1} Mbps ({:.2} MB/s)",
            snapshot.network.upload_mbps,
            snapshot.network.upload_mb_s,
            snapshot.network.download_mbps,
            snapshot.network.download_mb_s,
        );
        draw_gdi_line(mem_dc, &net_line, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h;

        let total_mb = snapshot.network.total_bytes_session as f64 / (1024.0 * 1024.0);
        let adapter_line = format!(
            "网络接口: {} (本次累计: {:.1} MB)",
            snapshot.network.adapter_name, total_mb
        );
        draw_gdi_line(mem_dc, &adapter_line, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h;

        // 3. 硬件指标
        let cpu_top = if !snapshot.details.top_process_display.is_empty() && snapshot.details.top_process_display != "--" {
            format!(" (Top: {})", snapshot.details.top_process_display)
        } else {
            "".to_string()
        };
        let cpu_line = format!("处 理 器: {:.0}%{}", snapshot.cpu.usage_percent, cpu_top);
        draw_gdi_line(mem_dc, &cpu_line, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h;

        let mem_line = format!(
            "物 理 内存: {}% (已用 {:.1} GB / 共 {:.1} GB)",
            snapshot.memory.usage_percent, snapshot.memory.used_gb, snapshot.memory.total_gb
        );
        draw_gdi_line(mem_dc, &mem_line, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h;

        let gpu_vram = snapshot.details.vram_display.as_deref().unwrap_or("显存: --");
        let gpu_val = snapshot.gpu.usage_percent.map(|v| format!("{:.0}%", v)).unwrap_or_else(|| "--%".to_string());
        let gpu_line = format!("独 立 显卡: {} ({})", gpu_val, gpu_vram);
        draw_gdi_line(mem_dc, &gpu_line, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h;

        let disk_line = format!(
            "磁 盘 I/O: {:.0}% ({})",
            snapshot.disk.activity_percent, snapshot.disk.tooltip_display
        );
        draw_gdi_line(mem_dc, &disk_line, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h;

        // 4. 环境与系统
        let uptime_line = format!("连续运行: {}", snapshot.details.uptime_display);
        draw_gdi_line(mem_dc, &uptime_line, pad_x, cur_y, width - pad_x * 2, line_h);
        cur_y += line_h;

        if let Some(power) = &snapshot.details.power_display {
            let power_line = format!("供电状态: {}", power);
            draw_gdi_line(mem_dc, &power_line, pad_x, cur_y, width - pad_x * 2, line_h);
        }

        let _ = DeleteObject(HGDIOBJ(font_title.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_text.0 as _));

        // 确保像素 Alpha
        for p in pixels.iter_mut() {
            let a = (*p >> 24) & 0xFF;
            if a < alpha {
                *p |= alpha << 24;
            }
        }

        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        let mut pt_src = POINT { x: 0, y: 0 };
        let mut sz = SIZE { cx: width, cy: height };

        let _ = UpdateLayeredWindow(
            self.hwnd,
            Some(screen_dc),
            None,
            Some(&mut sz),
            Some(mem_dc),
            Some(&mut pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        let _ = SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(HGDIOBJ(hbitmap.0 as _));
        let _ = DeleteDC(mem_dc);
        let _ = windows::Win32::Graphics::Gdi::ReleaseDC(None, screen_dc);
    }
}

unsafe fn draw_gdi_line(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    let _ = DrawTextW(hdc, &mut buf, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
}

unsafe extern "system" fn tooltip_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_SETCURSOR {
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let _ = SetCursor(Some(cursor));
        return LRESULT(1);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl Drop for MonitorTooltip {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
