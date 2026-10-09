#![allow(unsafe_op_in_unsafe_fn)]

use crate::monitor::metrics::MetricsSnapshot;
use crate::rules::{I18n, Language};
use std::sync::atomic::{AtomicPtr, Ordering};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, FillRect, GetMonitorInfoW, MonitorFromWindow, SelectObject, SetBkMode, SetTextColor,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DT_CALCRECT, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, FW_BOLD,
    FW_NORMAL, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, TRANSPARENT, BLENDFUNCTION,
    AC_SRC_ALPHA, AC_SRC_OVER,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowRect, LoadCursorW,
    RegisterClassW, SetCursor, SetWindowPos, ShowWindow, UpdateLayeredWindow,
    HWND_TOPMOST, IDC_ARROW, IDC_HAND, SWP_NOACTIVATE, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE,
    ULW_ALPHA, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETCURSOR, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const WM_MOUSELEAVE: u32 = 0x02A3;

static TOOLTIP_INSTANCE: AtomicPtr<TooltipInner> = AtomicPtr::new(std::ptr::null_mut());

pub struct MonitorTooltip {
    hwnd: HWND,
}

struct TooltipInner {
    hwnd: HWND,
    width: i32,
    height: i32,
    scale: f32,
    is_dark: bool,
    producer_rect: RECT,
    is_hover_producer: bool,
    data: Option<TooltipData>,
}

#[derive(Clone)]
struct TooltipData {
    title: String,
    producer: String,
    lines: Vec<String>,
}

impl TooltipData {
    fn from_snapshot(snapshot: &MetricsSnapshot, lang: &Language) -> Self {
        let bundle = I18n::get_bundle(lang);
        let title = if bundle.monitor.tooltip_title.is_empty() {
            "QuickPath硬件看板".to_string()
        } else {
            bundle.monitor.tooltip_title.clone()
        };

        let producer = if bundle.floating_bar.producer.is_empty() {
            "衢州御风科技有限公司出品".to_string()
        } else {
            bundle.floating_bar.producer.clone()
        };

        let net_line = format!(
            "实时网速: ↑ {:.1} Mbps ({:.2} MB/s)  ↓ {:.1} Mbps ({:.2} MB/s)",
            snapshot.network.upload_mbps,
            snapshot.network.upload_mb_s,
            snapshot.network.download_mbps,
            snapshot.network.download_mb_s,
        );

        let total_mb = snapshot.network.total_bytes_session as f64 / (1024.0 * 1024.0);
        let adapter_line = format!(
            "网络接口: {} (本次累计: {:.1} MB)",
            snapshot.network.adapter_name, total_mb
        );

        let cpu_top = if !snapshot.details.top_process_display.is_empty() && snapshot.details.top_process_display != "--" {
            format!(" (Top: {})", snapshot.details.top_process_display)
        } else {
            "".to_string()
        };
        let cpu_line = format!("处 理 器: {:.0}%{}", snapshot.cpu.usage_percent, cpu_top);

        let mem_line = format!(
            "物 理 内存: {}% (已用 {:.1} GB / 共 {:.1} GB)",
            snapshot.memory.usage_percent, snapshot.memory.used_gb, snapshot.memory.total_gb
        );

        let gpu_vram = snapshot.details.vram_display.as_deref().unwrap_or("显存: --");
        let gpu_val = snapshot.gpu.usage_percent.map(|v| format!("{:.0}%", v)).unwrap_or_else(|| "--%".to_string());
        let gpu_line = format!("独 立 显卡: {} ({})", gpu_val, gpu_vram);

        let disk_line = format!(
            "磁 盘 I/O: {:.0}% ({})",
            snapshot.disk.activity_percent, snapshot.disk.tooltip_display
        );

        let uptime_line = format!("连续运行: {}", snapshot.details.uptime_display);

        let mut lines = vec![
            net_line,
            adapter_line,
            cpu_line,
            mem_line,
            gpu_line,
            disk_line,
            uptime_line,
        ];

        if let Some(power) = &snapshot.details.power_display {
            lines.push(format!("供电状态: {}", power));
        }

        Self {
            title,
            producer,
            lines,
        }
    }
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
                480,
                260,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => return Err("创建 Tooltip 悬浮看板窗口失败".to_string()),
            };

            let inner = Box::new(TooltipInner {
                hwnd,
                width: 480,
                height: 260,
                scale: 1.0,
                is_dark: true,
                producer_rect: RECT::default(),
                is_hover_producer: false,
                data: None,
            });

            TOOLTIP_INSTANCE.store(Box::into_raw(inner), Ordering::SeqCst);

            Ok(Self { hwnd })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn show(&mut self, bar_rect: RECT, snapshot: &MetricsSnapshot, lang: &Language, scale: f32, is_dark: bool) {
        let data = TooltipData::from_snapshot(snapshot, lang);

        let ptr = TOOLTIP_INSTANCE.load(Ordering::SeqCst);
        if ptr.is_null() {
            return;
        }
        let inner = unsafe { &mut *ptr };

        inner.scale = scale;
        inner.is_dark = is_dark;
        inner.data = Some(data.clone());

        let pad_x = (16.0 * scale).round() as i32;
        let pad_top = (14.0 * scale).round() as i32;
        let pad_bot = (14.0 * scale).round() as i32;
        let title_h = (24.0 * scale).round() as i32;
        let line_h = (23.0 * scale).round() as i32;
        let title_gap = (6.0 * scale).round() as i32;
        let sep_gap = (8.0 * scale).round() as i32;

        let font_title_size = (-14.0 * scale).round() as i32;
        let font_prod_size = (-12.0 * scale).round() as i32;
        let font_text_size = (-12.0 * scale).round() as i32;

        let (title_w, prod_w, max_w) = unsafe {
            let screen_dc = windows::Win32::Graphics::Gdi::GetDC(None);
            let font_title = CreateFontW(
                font_title_size, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
                FONT_CHARSET(1), FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0),
                FONT_QUALITY(5), 0, w!("Microsoft YaHei UI"),
            );
            let font_prod = CreateFontW(
                font_prod_size, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
                FONT_CHARSET(1), FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0),
                FONT_QUALITY(5), 0, w!("Microsoft YaHei UI"),
            );
            let font_text = CreateFontW(
                font_text_size, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
                FONT_CHARSET(1), FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0),
                FONT_QUALITY(5), 0, w!("Microsoft YaHei UI"),
            );

            // 测量标题宽度
            let old_font = SelectObject(screen_dc, HGDIOBJ(font_title.0 as _));
            let mut u16_title: Vec<u16> = data.title.encode_utf16().collect();
            let mut r = RECT::default();
            let _ = DrawTextW(screen_dc, &mut u16_title, &mut r, DT_CALCRECT | DT_SINGLELINE);
            let tw = r.right - r.left;

            // 测量出品方文字宽度
            let _ = SelectObject(screen_dc, HGDIOBJ(font_prod.0 as _));
            let mut u16_prod: Vec<u16> = data.producer.encode_utf16().collect();
            let mut r_prod = RECT::default();
            let _ = DrawTextW(screen_dc, &mut u16_prod, &mut r_prod, DT_CALCRECT | DT_SINGLELINE);
            let pw = r_prod.right - r_prod.left;

            // 测量正文每行宽度
            let _ = SelectObject(screen_dc, HGDIOBJ(font_text.0 as _));
            let mut mw = 0i32;
            for line in &data.lines {
                let mut u16_line: Vec<u16> = line.encode_utf16().collect();
                let mut r = RECT::default();
                let _ = DrawTextW(screen_dc, &mut u16_line, &mut r, DT_CALCRECT | DT_SINGLELINE);
                mw = mw.max(r.right - r.left);
            }

            let _ = SelectObject(screen_dc, old_font);
            let _ = DeleteObject(HGDIOBJ(font_title.0 as _));
            let _ = DeleteObject(HGDIOBJ(font_prod.0 as _));
            let _ = DeleteObject(HGDIOBJ(font_text.0 as _));
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(None, screen_dc);

            (tw, pw, mw)
        };

        // 顶栏左右最小间距 24px * scale
        let header_min_w = title_w + prod_w + (24.0 * scale).round() as i32;
        let content_w = max_w.max(header_min_w);

        // 宽度自适应：最长内容 + 左右内边距 + 安全微间距，最小 420px * scale (保证顶栏排布舒展)
        let w = (content_w + pad_x * 2 + (16.0 * scale).round() as i32).max((420.0 * scale).round() as i32);
        let h = pad_top + title_h + title_gap + 1 + sep_gap + (data.lines.len() as i32 * line_h) + pad_bot;

        inner.width = w;
        inner.height = h;

        // 计算出品方文字在客户区的绝对矩形
        let prod_x = w - pad_x - prod_w;
        let prod_y = pad_top;
        inner.producer_rect = RECT {
            left: prod_x - 4,
            top: prod_y,
            right: prod_x + prod_w + 4,
            bottom: prod_y + title_h,
        };

        // 计算定位与安全屏幕边界约束防出界
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            let hmon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let _ = GetMonitorInfoW(hmon, &mut mi);
        }
        let work = mi.rcWork;

        // X: 优先与 bar_rect.left 对齐，若溢出右侧则向左贴靠
        let mut x = bar_rect.left;
        if x + w > work.right - 8 {
            x = work.right - w - 8;
        }
        if x < work.left + 8 {
            x = work.left + 8;
        }

        // Y: 优先置于任务栏上方，若溢出上边缘则置于任务栏下方
        let mut y = bar_rect.top - h - (8.0 * scale).round() as i32;
        if y < work.top + 8 {
            y = bar_rect.bottom + (8.0 * scale).round() as i32;
        }

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
            inner.render(&data, scale, is_dark);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

impl TooltipInner {
    unsafe fn render(&self, data: &TooltipData, scale: f32, is_dark: bool) {
        let width = self.width;
        let height = self.height;

        let screen_dc = windows::Win32::Graphics::Gdi::GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
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

        // 1. 填充完全不透明的纯净深黑背景（0% 底层透光，隔绝底层代码与壁纸干扰）
        let pixel_count = (width * height) as usize;
        let pixels = std::slice::from_raw_parts_mut(bits_ptr as *mut u32, pixel_count);

        let (r, g, b) = if is_dark {
            (24u32, 24u32, 26u32) // 沉稳纯正深炭黑
        } else {
            (248u32, 248u32, 250u32)
        };
        let bg_pixel = (255u32 << 24) | (r << 16) | (g << 8) | b;
        pixels.fill(bg_pixel);

        // 2. 绘制 1px 精致外边框
        let border_brush = CreateSolidBrush(if is_dark {
            COLORREF(0x00444444) // 精致深灰边框
        } else {
            COLORREF(0x00D0D0D0)
        });
        let top_r = RECT { left: 0, top: 0, right: width, bottom: 1 };
        let bot_r = RECT { left: 0, top: height - 1, right: width, bottom: height };
        let left_r = RECT { left: 0, top: 0, right: 1, bottom: height };
        let right_r = RECT { left: width - 1, top: 0, right: width, bottom: height };
        FillRect(mem_dc, &top_r, border_brush);
        FillRect(mem_dc, &bot_r, border_brush);
        FillRect(mem_dc, &left_r, border_brush);
        FillRect(mem_dc, &right_r, border_brush);
        let _ = DeleteObject(HGDIOBJ(border_brush.0 as _));

        // 3. 字体设置与文字绘制 (全量采用 Microsoft YaHei UI + ClearType)
        SetBkMode(mem_dc, TRANSPARENT);

        let text_color_title = if is_dark { COLORREF(0x00FFFFFF) } else { COLORREF(0x001A1A1A) };
        let text_color_body = if is_dark { COLORREF(0x00F0F0F0) } else { COLORREF(0x002B2B2B) };
        let text_color_producer = if is_dark {
            if self.is_hover_producer {
                COLORREF(0x00FFFFFF) // 悬停纯白高亮
            } else {
                COLORREF(0x009E9E9E) // 常态精致次级浅灰
            }
        } else {
            if self.is_hover_producer {
                COLORREF(0x00000000)
            } else {
                COLORREF(0x00666666)
            }
        };

        let pad_x = (16.0 * scale).round() as i32;
        let mut cur_y = (14.0 * scale).round() as i32;
        let title_h = (24.0 * scale).round() as i32;
        let line_h = (23.0 * scale).round() as i32;

        let font_title = CreateFontW(
            (-14.0 * scale).round() as i32, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
            FONT_CHARSET(1), FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5), 0, w!("Microsoft YaHei UI"),
        );

        let font_prod = CreateFontW(
            (-12.0 * scale).round() as i32, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            FONT_CHARSET(1), FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5), 0, w!("Microsoft YaHei UI"),
        );

        let font_text = CreateFontW(
            (-12.0 * scale).round() as i32, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            FONT_CHARSET(1), FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5), 0, w!("Microsoft YaHei UI"),
        );

        // 绘制左侧标题
        let old_font = SelectObject(mem_dc, HGDIOBJ(font_title.0 as _));
        SetTextColor(mem_dc, text_color_title);
        draw_gdi_line(mem_dc, &data.title, pad_x, cur_y, width - pad_x * 2, title_h);

        // 绘制右侧出品方文字
        let _ = SelectObject(mem_dc, HGDIOBJ(font_prod.0 as _));
        SetTextColor(mem_dc, text_color_producer);
        let prod_w = self.producer_rect.right - self.producer_rect.left;
        draw_gdi_line(mem_dc, &data.producer, self.producer_rect.left + 4, cur_y, prod_w, title_h);

        // 悬停时在出品方文字底部绘制精致 1px 下划线
        if self.is_hover_producer {
            let underline_y = cur_y + title_h - (3.0 * scale).round() as i32;
            let underline_rect = RECT {
                left: self.producer_rect.left + 4,
                top: underline_y,
                right: self.producer_rect.right - 4,
                bottom: underline_y + (1.0 * scale).max(1.0).round() as i32,
            };
            let underline_brush = CreateSolidBrush(text_color_producer);
            FillRect(mem_dc, &underline_rect, underline_brush);
            let _ = DeleteObject(HGDIOBJ(underline_brush.0 as _));
        }

        cur_y += title_h + (6.0 * scale).round() as i32;

        // 顶栏分割线
        let line_rect = RECT {
            left: pad_x,
            top: cur_y,
            right: width - pad_x,
            bottom: cur_y + 1,
        };
        let sep_brush = CreateSolidBrush(if is_dark { COLORREF(0x003D3D3D) } else { COLORREF(0x00E0E0E0) });
        FillRect(mem_dc, &line_rect, sep_brush);
        let _ = DeleteObject(HGDIOBJ(sep_brush.0 as _));
        cur_y += 1 + (8.0 * scale).round() as i32;

        // 绘制硬件指标正文各行
        let _ = SelectObject(mem_dc, HGDIOBJ(font_text.0 as _));
        SetTextColor(mem_dc, text_color_body);
        for line in &data.lines {
            draw_gdi_line(mem_dc, line, pad_x, cur_y, width - pad_x * 2, line_h);
            cur_y += line_h;
        }

        let _ = SelectObject(mem_dc, old_font);
        let _ = DeleteObject(HGDIOBJ(font_title.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_prod.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_text.0 as _));

        // 4. Alpha 通道完整保持不透明 (255)
        for p in pixels.iter_mut() {
            *p |= 0xFF000000;
        }

        // 5. UpdateLayeredWindow
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
    let ptr = TOOLTIP_INSTANCE.load(Ordering::SeqCst);
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let inner = &mut *ptr;

    match msg {
        WM_SETCURSOR => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut win_r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut win_r);
            let x = pt.x - win_r.left;
            let y = pt.y - win_r.top;

            if x >= inner.producer_rect.left
                && x <= inner.producer_rect.right
                && y >= inner.producer_rect.top
                && y <= inner.producer_rect.bottom
            {
                let cursor = LoadCursorW(None, IDC_HAND).unwrap_or_default();
                let _ = SetCursor(Some(cursor));
                LRESULT(1)
            } else {
                let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
                let _ = SetCursor(Some(cursor));
                LRESULT(1)
            }
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut win_r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut win_r);
            let x = pt.x - win_r.left;
            let y = pt.y - win_r.top;

            let is_in = x >= inner.producer_rect.left
                && x <= inner.producer_rect.right
                && y >= inner.producer_rect.top
                && y <= inner.producer_rect.bottom;

            if is_in != inner.is_hover_producer {
                inner.is_hover_producer = is_in;
                if let Some(data) = &inner.data {
                    inner.render(data, inner.scale, inner.is_dark);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut win_r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut win_r);
            let x = pt.x - win_r.left;
            let y = pt.y - win_r.top;

            if x >= inner.producer_rect.left
                && x <= inner.producer_rect.right
                && y >= inner.producer_rect.top
                && y <= inner.producer_rect.bottom
            {
                let url: Vec<u16> = "https://qp.yftec.top\0".encode_utf16().collect();
                let op: Vec<u16> = "open\0".encode_utf16().collect();
                let _ = ShellExecuteW(
                    None,
                    windows::core::PCWSTR(op.as_ptr()),
                    windows::core::PCWSTR(url.as_ptr()),
                    windows::core::PCWSTR::null(),
                    windows::core::PCWSTR::null(),
                    windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
                );
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if inner.is_hover_producer {
                inner.is_hover_producer = false;
                if let Some(data) = &inner.data {
                    inner.render(data, inner.scale, inner.is_dark);
                }
            }

            // 检查鼠标离开后是否在任务栏监控条内，若都不在则隐匿
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let in_bar = if let Some(bar_hwnd) = crate::monitor::window::bar_window::get_bar_hwnd() {
                let mut r = RECT::default();
                let _ = GetWindowRect(bar_hwnd, &mut r);
                pt.x >= r.left && pt.x <= r.right && pt.y >= r.top && pt.y <= r.bottom
            } else {
                false
            };
            if !in_bar {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

impl Drop for MonitorTooltip {
    fn drop(&mut self) {
        let ptr = TOOLTIP_INSTANCE.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let _ = Box::from_raw(ptr);
            }
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
