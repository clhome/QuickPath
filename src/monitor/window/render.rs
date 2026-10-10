#![allow(unsafe_op_in_unsafe_fn)]

use crate::monitor::metrics::MetricsSnapshot;
use crate::rules::MonitorConfig;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, FillRect, GetTextExtentPoint32W, Polyline, SelectObject, SetBkMode,
    SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, DT_CALCRECT,
    DT_LEFT, DT_NOCLIP, DT_SINGLELINE, DT_VCENTER, FONT_CHARSET, FONT_CLIP_PRECISION,
    FONT_OUTPUT_PRECISION, FONT_QUALITY, FW_NORMAL, HDC, HGDIOBJ, PS_SOLID, TRANSPARENT,
    AC_SRC_ALPHA, AC_SRC_OVER,
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
        draw_network_column(mem_dc, col, snapshot, text_color_pri);
    }

    if let Some(col) = &layout.col_cpu_mem {
        draw_cpu_mem_column(mem_dc, col, snapshot, config, text_color_pri);
    }

    if let Some(col) = &layout.col_gpu_disk {
        draw_gpu_disk_column(mem_dc, col, snapshot, config, text_color_pri);
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

// 核心状态指示多色 (与底部色块完全统一)
const COLOR_METRIC_GREEN: COLORREF = COLORREF(0x0059C734); // 鲜绿 (20% ~ 70% / 10 ~ 50 Mbps)
const COLOR_METRIC_YELLOW: COLORREF = COLORREF(0x0000CCFF); // 亮黄 (70% ~ 85% / 50 ~ 80 Mbps)
const COLOR_METRIC_RED: COLORREF = COLORREF(0x00303BFF); // 鲜红 (> 85% / > 80 Mbps)

/// 根据百分比利用率计算动态四档状态色 (<20% 基色/白, 20%~70% 绿, 70%~85% 黄, >85% 红)
#[inline]
fn get_metric_status_color(percent: f32, base_color: COLORREF) -> COLORREF {
    if percent > 85.0 {
        COLOR_METRIC_RED
    } else if percent >= 70.0 {
        COLOR_METRIC_YELLOW
    } else if percent >= 20.0 {
        COLOR_METRIC_GREEN
    } else {
        base_color
    }
}

/// 根据实时网络速率 (Mbps) 计算动态四档状态色 (<10 基色/白, 10~50 绿, 50~80 黄, >80 红)
#[inline]
fn get_network_status_color(speed_mbps: f64, base_color: COLORREF) -> COLORREF {
    if speed_mbps > 80.0 {
        COLOR_METRIC_RED
    } else if speed_mbps >= 50.0 {
        COLOR_METRIC_YELLOW
    } else if speed_mbps >= 10.0 {
        COLOR_METRIC_GREEN
    } else {
        base_color
    }
}

/// 精准测量单行文本的像素行进宽度 (Win32 GDI)
unsafe fn measure_text_width(hdc: HDC, text: &str) -> i32 {
    let buf: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    if GetTextExtentPoint32W(hdc, &buf, &mut size).as_bool() {
        size.cx
    } else {
        let mut buf_clone = buf;
        let mut rect = RECT::default();
        let _ = DrawTextW(hdc, &mut buf_clone, &mut rect, DT_CALCRECT | DT_SINGLELINE);
        rect.right - rect.left
    }
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

/// 绘制单行文字片段 (Win32 ClearType，带 DT_NOCLIP 杜绝边缘亚像素裁切)
unsafe fn draw_gdi_text_segment(hdc: HDC, text: &str, x: i32, y: i32, h: i32) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT {
        left: x,
        top: y,
        right: x + 200,
        bottom: y + h,
    };
    let _ = DrawTextW(hdc, &mut buf, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP);
}

/// 解析指标字符串，拆分为 (前缀, 数值部分, 后缀%)
#[inline]
fn parse_metric_display<'a>(display: &'a str) -> Option<(&'a str, &'a str, &'a str)> {
    if let Some(colon_pos) = display.find(": ") {
        if display.ends_with('%') && colon_pos + 2 < display.len() - 1 {
            let prefix = &display[..colon_pos + 2];
            let num_part = &display[colon_pos + 2..display.len() - 1];
            let suffix = "%";
            return Some((prefix, num_part, suffix));
        }
    }
    None
}

/// 解析网速显示字符串，拆分为 (前缀箭头, 速率数值部分)
#[inline]
fn parse_network_display<'a>(display: &'a str) -> Option<(&'a str, &'a str)> {
    let mut chars = display.char_indices();
    if let Some((_, first_char)) = chars.next() {
        if (first_char == '↑' || first_char == '↓') && chars.next().is_some() {
            // 前缀为箭头与紧随的1个标准空格 (例如 "↑ " 或 "↓ ")
            let prefix_end = first_char.len_utf8() + 1;
            if display.len() >= prefix_end {
                let prefix = &display[..prefix_end];
                let num_part = &display[prefix_end..];
                return Some((prefix, num_part));
            }
        }
    }
    None
}

/// 绘制硬件指标单行（前缀保持基色、数值按负载动态多色分级、% 保持基色不变）
unsafe fn draw_metric_text_with_threshold(
    hdc: HDC,
    display_text: &str,
    percent: Option<f32>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    base_color: COLORREF,
) {
    // 匹配如 "C: 19%"、"M: 42%"、"G: --%"、"D:  0%"
    if let Some((prefix, num_part, suffix)) = parse_metric_display(display_text) {
        // 计算数值颜色 (<20% 基色, 20%~70% 绿, 70%~85% 黄, >85% 红)
        let num_color = if let Some(p) = percent {
            if num_part.trim() == "--" {
                base_color
            } else {
                get_metric_status_color(p, base_color)
            }
        } else {
            base_color
        };

        let prefix_w = measure_text_width(hdc, prefix);
        let num_w = measure_text_width(hdc, num_part);

        // 1. 绘制前缀（原色不变）
        SetTextColor(hdc, base_color);
        draw_gdi_text_segment(hdc, prefix, x, y, h);

        // 2. 绘制数值（动态多色分级）
        SetTextColor(hdc, num_color);
        draw_gdi_text_segment(hdc, num_part, x + prefix_w, y, h);

        // 3. 绘制末尾 % 符号（原色一直不变）
        SetTextColor(hdc, base_color);
        draw_gdi_text_segment(hdc, suffix, x + prefix_w + num_w, y, h);

        return;
    }

    // 格式不符合时降级全量单色渲染
    SetTextColor(hdc, base_color);
    draw_gdi_text(hdc, display_text, x, y, w, h);
}

/// 绘制微型警戒色块条 (GDI)
unsafe fn draw_gauge_bar_gdi(hdc: HDC, x: i32, y: i32, w: i32, h: i32, percent: f32, base_color: COLORREF) {
    let color = get_metric_status_color(percent, base_color);

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

/// 绘制网络指标单行（前缀箭头保持基色、数值按网速动态多色分级）
unsafe fn draw_network_text_with_threshold(
    hdc: HDC,
    display_text: &str,
    speed_mbps: f64,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    base_color: COLORREF,
) {
    if let Some((prefix, num_part)) = parse_network_display(display_text) {
        let num_color = get_network_status_color(speed_mbps, base_color);
        let prefix_w = measure_text_width(hdc, prefix);

        // 1. 绘制前缀箭头（保持基色不变）
        SetTextColor(hdc, base_color);
        draw_gdi_text_segment(hdc, prefix, x, y, h);

        // 2. 绘制速率数值（动态按网速分级变色）
        SetTextColor(hdc, num_color);
        draw_gdi_text_segment(hdc, num_part, x + prefix_w, y, h);

        return;
    }

    // 格式不符合时降级全量单色渲染
    SetTextColor(hdc, base_color);
    draw_gdi_text(hdc, display_text, x, y, w, h);
}

/// 绘制网络列 (两行：上传 / 下载 + 动态速率分档着色)
unsafe fn draw_network_column(
    hdc: HDC,
    col: &RECT,
    snapshot: &MetricsSnapshot,
    base_color: COLORREF,
) {
    let row_h = (col.bottom - col.top) / 2;
    let y1 = col.top;
    let y2 = col.top + row_h;

    draw_network_text_with_threshold(
        hdc,
        &snapshot.network.upload_display,
        snapshot.network.upload_mbps,
        col.left,
        y1,
        col.right - col.left,
        row_h,
        base_color,
    );
    draw_network_text_with_threshold(
        hdc,
        &snapshot.network.download_display,
        snapshot.network.download_mbps,
        col.left,
        y2,
        col.right - col.left,
        row_h,
        base_color,
    );
}

/// 绘制算力/内存列 (两行：CPU / 内存 + 2px 警戒指示条)
unsafe fn draw_cpu_mem_column(
    hdc: HDC,
    col: &RECT,
    snapshot: &MetricsSnapshot,
    config: &MonitorConfig,
    base_color: COLORREF,
) {
    let row_h = (col.bottom - col.top) / 2;
    let y1 = col.top;
    let y2 = col.top + row_h;

    if config.show_cpu {
        draw_metric_text_with_threshold(
            hdc,
            &snapshot.cpu.display,
            Some(snapshot.cpu.usage_percent),
            col.left,
            y1,
            col.right - col.left,
            row_h - 2,
            base_color,
        );
        let bar_y = y1 + row_h - 2;
        draw_gauge_bar_gdi(hdc, col.left, bar_y, col.right - col.left - 2, 2, snapshot.cpu.usage_percent, base_color);
    }

    if config.show_memory {
        draw_metric_text_with_threshold(
            hdc,
            &snapshot.memory.display,
            Some(snapshot.memory.usage_percent as f32),
            col.left,
            y2,
            col.right - col.left,
            row_h - 2,
            base_color,
        );
        let bar_y = col.bottom - 2;
        draw_gauge_bar_gdi(hdc, col.left, bar_y, col.right - col.left - 2, 2, snapshot.memory.usage_percent as f32, base_color);
    }
}

/// 绘制扩展设备列 (两行：GPU / 磁盘)
unsafe fn draw_gpu_disk_column(
    hdc: HDC,
    col: &RECT,
    snapshot: &MetricsSnapshot,
    config: &MonitorConfig,
    base_color: COLORREF,
) {
    let row_h = (col.bottom - col.top) / 2;
    let y1 = col.top;
    let y2 = col.top + row_h;

    if config.show_gpu {
        draw_metric_text_with_threshold(
            hdc,
            &snapshot.gpu.display,
            snapshot.gpu.usage_percent,
            col.left,
            y1,
            col.right - col.left,
            row_h,
            base_color,
        );
    }

    if config.show_disk {
        draw_metric_text_with_threshold(
            hdc,
            &snapshot.disk.display,
            Some(snapshot.disk.activity_percent),
            col.left,
            y2,
            col.right - col.left,
            row_h,
            base_color,
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_status_color_thresholds() {
        let base = COLORREF(0x00FFFFFF);

        // < 20% 为基准色 (白色)
        assert_eq!(get_metric_status_color(0.0, base), base);
        assert_eq!(get_metric_status_color(10.0, base), base);
        assert_eq!(get_metric_status_color(19.9, base), base);

        // 20% ~ 70% 为鲜绿色
        assert_eq!(get_metric_status_color(20.0, base), COLOR_METRIC_GREEN);
        assert_eq!(get_metric_status_color(50.0, base), COLOR_METRIC_GREEN);
        assert_eq!(get_metric_status_color(69.9, base), COLOR_METRIC_GREEN);

        // 70% ~ 85% 为亮黄色
        assert_eq!(get_metric_status_color(70.0, base), COLOR_METRIC_YELLOW);
        assert_eq!(get_metric_status_color(78.5, base), COLOR_METRIC_YELLOW);
        assert_eq!(get_metric_status_color(85.0, base), COLOR_METRIC_YELLOW);

        // > 85% 为鲜红色
        assert_eq!(get_metric_status_color(85.01, base), COLOR_METRIC_RED);
        assert_eq!(get_metric_status_color(90.0, base), COLOR_METRIC_RED);
        assert_eq!(get_metric_status_color(100.0, base), COLOR_METRIC_RED);
    }

    #[test]
    fn test_network_status_color_thresholds() {
        let base = COLORREF(0x00FFFFFF);

        // < 10 Mbps 为基准色 (白色)
        assert_eq!(get_network_status_color(0.0, base), base);
        assert_eq!(get_network_status_color(5.2, base), base);
        assert_eq!(get_network_status_color(9.99, base), base);

        // 10 ~ 50 Mbps 为鲜绿色
        assert_eq!(get_network_status_color(10.0, base), COLOR_METRIC_GREEN);
        assert_eq!(get_network_status_color(25.0, base), COLOR_METRIC_GREEN);
        assert_eq!(get_network_status_color(49.9, base), COLOR_METRIC_GREEN);

        // 50 ~ 80 Mbps 为亮黄色
        assert_eq!(get_network_status_color(50.0, base), COLOR_METRIC_YELLOW);
        assert_eq!(get_network_status_color(65.5, base), COLOR_METRIC_YELLOW);
        assert_eq!(get_network_status_color(80.0, base), COLOR_METRIC_YELLOW);

        // > 80 Mbps 为鲜红色
        assert_eq!(get_network_status_color(80.01, base), COLOR_METRIC_RED);
        assert_eq!(get_network_status_color(120.0, base), COLOR_METRIC_RED);
        assert_eq!(get_network_status_color(999.0, base), COLOR_METRIC_RED);
    }

    #[test]
    fn test_parse_network_display() {
        // 带前导空格的个位数速率
        let (prefix, num) = parse_network_display("↑  0.0").unwrap();
        assert_eq!(prefix, "↑ ");
        assert_eq!(num, " 0.0");

        // 两位数速率
        let (prefix, num) = parse_network_display("↓ 12.3").unwrap();
        assert_eq!(prefix, "↓ ");
        assert_eq!(num, "12.3");

        // 紧凑百兆速率
        let (prefix, num) = parse_network_display("↑  120").unwrap();
        assert_eq!(prefix, "↑ ");
        assert_eq!(num, " 120");

        // 四位数速率
        let (prefix, num) = parse_network_display("↑ 1200").unwrap();
        assert_eq!(prefix, "↑ ");
        assert_eq!(num, "1200");

        // 溢出文本
        let (prefix, num) = parse_network_display("↓ 999+").unwrap();
        assert_eq!(prefix, "↓ ");
        assert_eq!(num, "999+");

        // 异常格式不匹配
        assert!(parse_network_display("C: 19%").is_none());
        assert!(parse_network_display("Invalid").is_none());
    }

    #[test]
    fn test_parse_metric_display() {
        // 标准两位数
        let (prefix, num, suffix) = parse_metric_display("C: 19%").unwrap();
        assert_eq!(prefix, "C: ");
        assert_eq!(num, "19");
        assert_eq!(suffix, "%");

        // 带前导补齐空格的个位数
        let (prefix, num, suffix) = parse_metric_display("C:  9%").unwrap();
        assert_eq!(prefix, "C: ");
        assert_eq!(num, " 9");
        assert_eq!(suffix, "%");

        // 100% 满载
        let (prefix, num, suffix) = parse_metric_display("M: 100%").unwrap();
        assert_eq!(prefix, "M: ");
        assert_eq!(num, "100");
        assert_eq!(suffix, "%");

        // GPU 占位符 "--%"
        let (prefix, num, suffix) = parse_metric_display("G: --%").unwrap();
        assert_eq!(prefix, "G: ");
        assert_eq!(num, "--");
        assert_eq!(suffix, "%");

        // 磁盘 0%
        let (prefix, num, suffix) = parse_metric_display("D:  0%").unwrap();
        assert_eq!(prefix, "D: ");
        assert_eq!(num, " 0");
        assert_eq!(suffix, "%");

        // 非指标文本不匹配
        assert!(parse_metric_display("↑ 120 KB/s").is_none());
        assert!(parse_metric_display("Invalid").is_none());
    }
}
