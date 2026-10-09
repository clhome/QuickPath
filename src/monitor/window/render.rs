#![allow(unsafe_op_in_unsafe_fn)]

use crate::monitor::metrics::MetricsSnapshot;
use crate::rules::MonitorConfig;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, FillRect, Polyline, SelectObject, SetBkMode, SetTextColor,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION,
    FONT_QUALITY, FW_NORMAL, HDC, HGDIOBJ, PS_SOLID, TRANSPARENT, AC_SRC_ALPHA, AC_SRC_OVER,
};
use windows::Win32::UI::WindowsAndMessaging::{
    UpdateLayeredWindow, ULW_ALPHA,
};

/// 动态列布局信息
pub struct LayoutInfo {
    pub total_width: i32,
    pub total_height: i32,
    pub col_net: Option<RECT>,
    pub col_cpu_mem: Option<RECT>,
    pub col_gpu_disk: Option<RECT>,
}

/// 计算自适应宽度与各列矩形
pub fn calculate_layout(config: &MonitorConfig, scale: f32) -> LayoutInfo {
    let pad_x = (6.0 * scale).round() as i32;
    let col_gap = (6.0 * scale).round() as i32;
    let height = (38.0 * scale).round() as i32;

    let has_net = config.show_network;
    let has_cpu_mem = config.show_cpu || config.show_memory;
    let has_gpu_disk = config.show_gpu || config.show_disk;

    let w_net = (42.0 * scale).round() as i32;
    let w_cpu_mem = (48.0 * scale).round() as i32;
    let w_gpu_disk = (48.0 * scale).round() as i32;

    let mut current_x = pad_x;
    let mut rect_net = None;
    let mut rect_cpu_mem = None;
    let mut rect_gpu_disk = None;

    if has_net {
        rect_net = Some(RECT {
            left: current_x,
            top: 0,
            right: current_x + w_net,
            bottom: height,
        });
        current_x += w_net + col_gap;
    }

    if has_cpu_mem {
        rect_cpu_mem = Some(RECT {
            left: current_x,
            top: 0,
            right: current_x + w_cpu_mem,
            bottom: height,
        });
        current_x += w_cpu_mem + col_gap;
    }

    if has_gpu_disk {
        rect_gpu_disk = Some(RECT {
            left: current_x,
            top: 0,
            right: current_x + w_gpu_disk,
            bottom: height,
        });
        current_x += w_gpu_disk + col_gap;
    }

    let total_width = if current_x > pad_x {
        current_x - col_gap + pad_x
    } else {
        0
    };

    LayoutInfo {
        total_width,
        total_height: height,
        col_net: rect_net,
        col_cpu_mem: rect_cpu_mem,
        col_gpu_disk: rect_gpu_disk,
    }
}

/// 渲染常驻状态面板并通过 UpdateLayeredWindow 合成上屏
pub unsafe fn render_bar_window(
    hwnd: HWND,
    snapshot: &MetricsSnapshot,
    config: &MonitorConfig,
    scale: f32,
    is_dark_theme: bool,
) {
    let layout = calculate_layout(config, scale);
    if layout.total_width <= 0 || layout.total_height <= 0 {
        return;
    }

    let width = layout.total_width;
    let height = layout.total_height;

    let screen_dc = windows::Win32::Graphics::Gdi::GetDC(None);
    let mem_dc = CreateCompatibleDC(Some(screen_dc));

    // 创建 32 位 ARGB DIBSection
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

    // 1. 初始化 32bpp 像素内存为沉稳深色背景（预乘 Alpha）
    let pixel_count = (width * height) as usize;
    let pixels = std::slice::from_raw_parts_mut(bits_ptr as *mut u32, pixel_count);

    // 将用户设置的 opacity (50%~100%) 精确映射为 0~255 Alpha
    let alpha_ratio = (config.opacity as f32 / 100.0).clamp(0.5, 1.0);
    let base_alpha = (alpha_ratio * 255.0).round() as u32;

    // 沉稳纯净底色（深色模式深灰黑 RGB(24, 24, 26)，浅色模式浅灰白 RGB(245, 245, 248)）
    let (bg_r, bg_g, bg_b) = if is_dark_theme {
        (24u32, 24u32, 26u32)
    } else {
        (245u32, 245u32, 248u32)
    };

    let premul_r = (bg_r * base_alpha) / 255;
    let premul_g = (bg_g * base_alpha) / 255;
    let premul_b = (bg_b * base_alpha) / 255;
    let premul_bg_pixel = (base_alpha << 24) | (premul_r << 16) | (premul_g << 8) | premul_b;

    pixels.fill(premul_bg_pixel);

    // 2. 绘制实时历史折线波形背景（如开启）
    if config.show_graph_bg && layout.col_cpu_mem.is_some() {
        let col = layout.col_cpu_mem.as_ref().unwrap();
        draw_waveform_graph_gdi(mem_dc, snapshot, col, scale, is_dark_theme);
    }

    // 3. 采用 Windows 原生 ClearType 亚像素引擎绘制文字
    SetBkMode(mem_dc, TRANSPARENT);

    let font_size = (-12.0 * scale).round() as i32;
    let font = CreateFontW(
        font_size,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        FONT_CHARSET(1),
        FONT_OUTPUT_PRECISION(0),
        FONT_CLIP_PRECISION(0),
        FONT_QUALITY(5), // 5 = CLEARTYPE_QUALITY
        0,
        w!("Microsoft YaHei UI"),
    );
    let old_font = SelectObject(mem_dc, HGDIOBJ(font.0 as _));

    let text_color_pri = if is_dark_theme {
        COLORREF(0x00FFFFFF) // 纯白 (0x00BBGGRR)
    } else {
        COLORREF(0x001A1A1A) // 深炭黑
    };
    SetTextColor(mem_dc, text_color_pri);

    // 4. 绘制各列指标
    if let Some(col) = &layout.col_net {
        draw_network_column(mem_dc, col, snapshot);
    }

    if let Some(col) = &layout.col_cpu_mem {
        draw_cpu_mem_column(mem_dc, col, snapshot, config);
    }

    if let Some(col) = &layout.col_gpu_disk {
        draw_gpu_disk_column(mem_dc, col, snapshot, config);
    }

    let _ = SelectObject(mem_dc, old_font);
    let _ = DeleteObject(HGDIOBJ(font.0 as _));

    // 5. 平滑保证 Alpha 通道底限，彻底杜绝文字边缘硬化发虚
    for p in pixels.iter_mut() {
        let a = (*p >> 24) & 0xFF;
        if a < base_alpha {
            *p = (*p & 0x00FFFFFF) | (base_alpha << 24);
        }
    }

    // 6. 调用 UpdateLayeredWindow 合成上屏
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };

    let mut pt_src = POINT { x: 0, y: 0 };
    let mut size = SIZE {
        cx: width,
        cy: height,
    };

    let _ = UpdateLayeredWindow(
        hwnd,
        Some(screen_dc),
        None,
        Some(&mut size),
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

/// 绘制单行文字 (Win32 ClearType)
unsafe fn draw_gdi_text(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    let _ = DrawTextW(hdc, &mut buf, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
}

/// 绘制微型警戒色块条 (GDI)
unsafe fn draw_gauge_bar_gdi(hdc: HDC, x: i32, y: i32, w: i32, h: i32, percent: f32) {
    let color = if percent >= 95.0 {
        COLORREF(0x00303BFF) // 鲜红 (0x00BBGGRR)
    } else if percent >= 85.0 {
        COLORREF(0x0000CCFF) // 亮黄
    } else {
        COLORREF(0x0059C734) // 鲜绿
    };

    let fill_w = ((w as f32 * (percent / 100.0)).round() as i32).clamp(1, w);
    let rect = RECT {
        left: x,
        top: y,
        right: x + fill_w,
        bottom: y + h,
    };
    let brush = CreateSolidBrush(color);
    FillRect(hdc, &rect, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0 as _));
}

/// 绘制网络列 (两行：上传 / 下载)
unsafe fn draw_network_column(hdc: HDC, col: &RECT, snapshot: &MetricsSnapshot) {
    let row_h = (col.bottom - col.top) / 2;
    let y1 = col.top;
    let y2 = col.top + row_h;

    draw_gdi_text(hdc, &snapshot.network.upload_display, col.left, y1, col.right - col.left, row_h);
    draw_gdi_text(hdc, &snapshot.network.download_display, col.left, y2, col.right - col.left, row_h);
}

/// 绘制算力/内存列 (两行：CPU / 内存 + 2px 警戒指示条)
unsafe fn draw_cpu_mem_column(
    hdc: HDC,
    col: &RECT,
    snapshot: &MetricsSnapshot,
    config: &MonitorConfig,
) {
    let row_h = (col.bottom - col.top) / 2;
    let y1 = col.top;
    let y2 = col.top + row_h;

    if config.show_cpu {
        draw_gdi_text(hdc, &snapshot.cpu.display, col.left, y1, col.right - col.left, row_h - 2);
        let bar_y = y1 + row_h - 2;
        draw_gauge_bar_gdi(hdc, col.left, bar_y, col.right - col.left - 2, 2, snapshot.cpu.usage_percent);
    }

    if config.show_memory {
        draw_gdi_text(hdc, &snapshot.memory.display, col.left, y2, col.right - col.left, row_h - 2);
        let bar_y = col.bottom - 2;
        draw_gauge_bar_gdi(hdc, col.left, bar_y, col.right - col.left - 2, 2, snapshot.memory.usage_percent as f32);
    }
}

/// 绘制扩展设备列 (两行：GPU / 磁盘)
unsafe fn draw_gpu_disk_column(
    hdc: HDC,
    col: &RECT,
    snapshot: &MetricsSnapshot,
    config: &MonitorConfig,
) {
    let row_h = (col.bottom - col.top) / 2;
    let y1 = col.top;
    let y2 = col.top + row_h;

    if config.show_gpu {
        draw_gdi_text(hdc, &snapshot.gpu.display, col.left, y1, col.right - col.left, row_h);
    }

    if config.show_disk {
        draw_gdi_text(hdc, &snapshot.disk.display, col.left, y2, col.right - col.left, row_h);
    }
}

/// 绘制背景历史折线微型波形图 (GDI)
unsafe fn draw_waveform_graph_gdi(
    hdc: HDC,
    snapshot: &MetricsSnapshot,
    col: &RECT,
    scale: f32,
    is_dark: bool,
) {
    let points = snapshot.cpu_waveform.points();
    if points.len() < 2 {
        return;
    }

    let x_start = col.left as f32;
    let width = (col.right - col.left) as f32;
    let height = (col.bottom - col.top) as f32;
    let step_x = width / (points.len() - 1) as f32;

    let pen_color = if is_dark {
        COLORREF(0x007A5020) // 浅淡柔和微蓝 (0x00BBGGRR)
    } else {
        COLORREF(0x00D0B080)
    };

    let pen_w = (1.0 * scale).round() as i32;
    let pen = CreatePen(PS_SOLID, pen_w, pen_color);
    let old_pen = SelectObject(hdc, HGDIOBJ(pen.0 as _));

    let mut gdi_pts: Vec<POINT> = Vec::with_capacity(points.len());
    for (i, &p) in points.iter().enumerate() {
        let x = (x_start + i as f32 * step_x).round() as i32;
        let y = (col.bottom as f32 - (p.clamp(0.0, 100.0) / 100.0) * (height * 0.75)).round() as i32;
        gdi_pts.push(POINT { x, y });
    }

    let _ = Polyline(hdc, &gdi_pts);

    let _ = SelectObject(hdc, old_pen);
    let _ = DeleteObject(HGDIOBJ(pen.0 as _));
}
