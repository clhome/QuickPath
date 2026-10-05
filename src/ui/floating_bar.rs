#![allow(dead_code)]

use crate::dialog::inject_path_to_dialog;
use crate::managers::FolderCandidate;
use crate::rules::{I18n, Language};
use std::sync::atomic::{AtomicPtr, Ordering};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, GetMonitorInfoW, GetStockObject, InvalidateRect, MonitorFromWindow,
    SelectObject, SetBkMode, SetTextColor, UpdateWindow, DT_CALCRECT, DT_END_ELLIPSIS,
    DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, FW_SEMIBOLD,
    HBRUSH, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
    TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, LWA_ALPHA,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WM_DESTROY,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WNDCLASSW, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

static FLOATING_BAR_INSTANCE: AtomicPtr<FloatingBarState> =
    AtomicPtr::new(std::ptr::null_mut());

pub struct FloatingBarState {
    pub hwnd: HWND,
    pub target_dialog_hwnd: HWND,
    pub target_edit_hwnd: HWND,
    pub candidates: Vec<FolderCandidate>,
    pub selected_index: usize,
    pub hovered_index: Option<usize>,
    pub dpi_scale: f32,
    pub bar_width: i32,
    pub header_height: i32,
    pub item_height: i32,
    pub padding: i32,
    pub opacity: u8,
    pub language: Language,
}

pub struct FloatingBar {
    state: Box<FloatingBarState>,
}

impl FloatingBar {
    pub fn new() -> Result<Self, String> {
        let class_name = w!("QuickPath_FloatingBar_Class");
        unsafe {
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(floating_bar_wnd_proc),
                hInstance: HINSTANCE::default(),
                lpszClassName: class_name,
                hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            // 启用 WS_EX_LAYERED 支持分层半透明
            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                class_name,
                w!("QuickPath Floating Bar"),
                WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                780,
                320,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => return Err("创建悬浮吸附条窗口失败".to_string()),
            };

            // 开启 Windows 11 现代圆角 (Round Corner)
            let round_pref = DWMWCP_ROUND.0 as u32;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );

            // 启用沉浸式深色模式
            let dark_val = 1u32;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_val as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );

            // 开启 Windows 11 原生亚克力材质
            let backdrop_type = 3u32; // Acrylic 亚克力材质
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop_type as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );

            // 默认设置 88% 不透明度（轻透通透且清晰）
            let default_alpha = ((88.0f32 / 100.0) * 255.0).round() as u8;
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), default_alpha, LWA_ALPHA);

            let mut state = Box::new(FloatingBarState {
                hwnd,
                target_dialog_hwnd: HWND::default(),
                target_edit_hwnd: HWND::default(),
                candidates: Vec::new(),
                selected_index: 0,
                hovered_index: None,
                dpi_scale: 1.0,
                bar_width: 780,
                header_height: 36,
                item_height: 38,
                padding: 10,
                opacity: 88,
                language: Language::Auto,
            });

            FLOATING_BAR_INSTANCE.store(&raw mut *state, Ordering::SeqCst);

            Ok(Self { state })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.state.hwnd
    }

    /// 设置弹出层半透明程度（百分比，40 ~ 100）
    pub fn set_opacity(&mut self, opacity_percent: u8) {
        let valid_pct = opacity_percent.clamp(40, 100);
        self.state.opacity = valid_pct;
        let alpha = ((valid_pct as f32 / 100.0) * 255.0).round() as u8;
        unsafe {
            let _ = SetLayeredWindowAttributes(self.state.hwnd, COLORREF(0), alpha, LWA_ALPHA);
        }
    }

    /// 显示并根据多屏幕物理工作区与 DPI 动态定位，贴合吸附在目标文件对话框的边缘
    pub fn show(
        &mut self,
        dialog_hwnd: HWND,
        edit_hwnd: HWND,
        dialog_rect: RECT,
        candidates: Vec<FolderCandidate>,
        opacity: u8,
        language: Language,
    ) {
        if candidates.is_empty() {
            self.hide();
            return;
        }

        self.state.opacity = opacity;
        self.state.language = language;
        self.set_opacity(opacity);

        // 1. 获取对话框所在物理显示器的精确物理 DPI（通过 MonitorFromWindow + GetDpiForMonitor 直接获取目标屏 DPI）
        let h_mon = unsafe { MonitorFromWindow(dialog_hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        let dpi = unsafe {
            if GetDpiForMonitor(h_mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() && dpi_x > 0 {
                dpi_x
            } else {
                let d = GetDpiForWindow(dialog_hwnd);
                if d > 0 { d } else { 96 }
            }
        };
        let scale = (dpi as f32 / 96.0).max(1.0);
        self.state.dpi_scale = scale;

        // 2. 根据目标屏幕真实 DPI 动态计算单行高密度几何尺寸（基准宽度 740px，行高 38px）
        self.state.bar_width = (740.0 * scale).round() as i32;
        self.state.header_height = (36.0 * scale).round() as i32;
        self.state.item_height = (38.0 * scale).round() as i32;
        self.state.padding = (10.0 * scale).round() as i32;

        self.state.target_dialog_hwnd = dialog_hwnd;
        self.state.target_edit_hwnd = edit_hwnd;
        self.state.candidates = candidates;
        self.state.selected_index = 0;
        self.state.hovered_index = None;

        // 单行紧凑高密度展示，最多展示 8 行
        let visible_items = self.state.candidates.len().min(8) as i32;
        let total_height = self.state.header_height + visible_items * self.state.item_height + self.state.padding;

        // 3. 多显示器感知定位：精准获取对话框所在显示器的物理工作区 rcWork
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let has_mon_info = unsafe { GetMonitorInfoW(h_mon, &mut mi).as_bool() };
        let work_area = if has_mon_info {
            mi.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: unsafe { GetSystemMetrics(SM_CXSCREEN) },
                bottom: unsafe { GetSystemMetrics(SM_CYSCREEN) },
            }
        };

        // 屏幕宽度保护：确保宽度不超过当前屏幕物理工作区可用宽度的 90%
        let max_w = (work_area.right - work_area.left - 24).max(400);
        let mut bar_w = self.state.bar_width;
        if bar_w > max_w {
            bar_w = max_w;
            self.state.bar_width = max_w;
        }

        let dlg_w = dialog_rect.right - dialog_rect.left;

        // 默认居中贴附在对话框底部边缘
        let mut x = dialog_rect.left + (dlg_w - bar_w) / 2;
        let mut y = dialog_rect.bottom + (6.0 * scale).round() as i32;

        unsafe {
            // 若超出当前显示器工作区底部，向上吸附在对话框顶部上方
            if y + total_height > work_area.bottom - (8.0 * scale).round() as i32 {
                y = dialog_rect.top - total_height - (6.0 * scale).round() as i32;
                if y < work_area.top + 8 {
                    y = work_area.top + 8;
                }
            }

            // 水平坐标严格约束在当前显示器工作区范围内，杜绝跳出屏幕
            if x < work_area.left + 8 {
                x = work_area.left + 8;
            } else if x + bar_w > work_area.right - 8 {
                x = work_area.right - bar_w - 8;
            }

            let _ = SetWindowPos(
                self.state.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                bar_w,
                total_height,
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );

            let _ = InvalidateRect(Some(self.state.hwnd), None, false);
            let _ = UpdateWindow(self.state.hwnd);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.state.hwnd, SW_HIDE);
        }
    }

    pub fn handle_key_nav(&mut self, key_code: u32) -> bool {
        if self.state.candidates.is_empty() {
            return false;
        }

        let max_idx = self.state.candidates.len().min(8) - 1;
        match key_code {
            0x26 => {
                if self.state.selected_index > 0 {
                    self.state.selected_index -= 1;
                } else {
                    self.state.selected_index = max_idx;
                }
                unsafe {
                    let _ = InvalidateRect(Some(self.state.hwnd), None, false);
                    let _ = UpdateWindow(self.state.hwnd);
                }
                true
            }
            0x28 => {
                if self.state.selected_index < max_idx {
                    self.state.selected_index += 1;
                } else {
                    self.state.selected_index = 0;
                }
                unsafe {
                    let _ = InvalidateRect(Some(self.state.hwnd), None, false);
                    let _ = UpdateWindow(self.state.hwnd);
                }
                true
            }
            0x0D => {
                self.confirm_selection(self.state.selected_index);
                true
            }
            0x1B => {
                self.hide();
                true
            }
            _ => false,
        }
    }

    pub fn confirm_selection(&mut self, index: usize) {
        if let Some(target) = self.state.candidates.get(index) {
            let path = target.path.clone();
            inject_path_to_dialog(
                self.state.target_dialog_hwnd,
                self.state.target_edit_hwnd,
                &path,
            );
        }
        self.hide();
    }
}

unsafe extern "system" fn floating_bar_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                render_floating_bar(hwnd, hdc);

                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_mouse_move(hwnd, y);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_mouse_click(hwnd, y);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DPICHANGED => {
            let new_dpi = (wparam.0 & 0xffff) as u32;
            let scale = (new_dpi as f32 / 96.0).max(1.0);
            let state_ptr = FLOATING_BAR_INSTANCE.load(Ordering::SeqCst);
            if !state_ptr.is_null() {
                unsafe {
                    let state = &mut *state_ptr;
                    state.dpi_scale = scale;
                    state.bar_width = (740.0 * scale).round() as i32;
                    state.header_height = (36.0 * scale).round() as i32;
                    state.item_height = (38.0 * scale).round() as i32;
                    state.padding = (10.0 * scale).round() as i32;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn render_floating_bar(hwnd: HWND, hdc: HDC) {
    let state_ptr = FLOATING_BAR_INSTANCE.load(Ordering::SeqCst);
    if state_ptr.is_null() {
        return;
    }
    let state = unsafe { &*state_ptr };

    let scale = state.dpi_scale;
    let mut rect = RECT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect);

        // 1. Fluent 暗黑底色 #1a1a1e
        let bg_color = COLORREF(0x001e1a1a);
        let bg_brush = CreateSolidBrush(bg_color);
        FillRect(hdc, &rect, bg_brush);
        let _ = DeleteObject(HGDIOBJ(bg_brush.0 as _));

        SetBkMode(hdc, TRANSPARENT);

        // 2. 根据 DPI 动态构建高精度字体（按比例缩放，目录名加粗，路径保持原字号）
        let font_title_size = (-14.0 * scale).round() as i32;
        let font_item_title_size = (-14.0 * scale).round() as i32;
        let font_path_size = (-12.0 * scale).round() as i32;
        let font_tag_size = (-11.0 * scale).round() as i32;

        let font_header = CreateFontW(
            font_title_size, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_item_title = CreateFontW(
            font_item_title_size, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_path = CreateFontW(
            font_path_size, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );
        let font_tag = CreateFontW(
            font_tag_size, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );

        // 3. 绘制顶栏标题与快捷提示（多国语言支持）
        let pad = state.padding;
        let mut header_rect = RECT {
            left: pad + (6.0 * scale) as i32,
            top: (2.0 * scale) as i32,
            right: rect.right - pad,
            bottom: state.header_height,
        };
        SelectObject(hdc, HGDIOBJ(font_header.0 as _));
        SetTextColor(hdc, COLORREF(0x00e0e0e0));
        let title_text = I18n::floating_title(&state.language);
        let mut title_buf: Vec<u16> = title_text.encode_utf16().collect();
        DrawTextW(hdc, &mut title_buf, &mut header_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        // 4. 循环单行排布绘制每个候选项目
        let visible_count = state.candidates.len().min(8);
        let mut curr_y = state.header_height;

        for (idx, item) in state.candidates.iter().take(visible_count).enumerate() {
            let is_selected = state.selected_index == idx;
            let is_hovered = state.hovered_index == Some(idx);

            let item_rect = RECT {
                left: pad,
                top: curr_y + (1.0 * scale) as i32,
                right: rect.right - pad,
                bottom: curr_y + state.item_height - (1.0 * scale) as i32,
            };

            // 背景卡片颜色：选中态亮蓝灰 #383845，悬停态 #2d2d34，普通态 #242428
            let card_color = if is_selected {
                COLORREF(0x00453838)
            } else if is_hovered {
                COLORREF(0x00342d2d)
            } else {
                COLORREF(0x00282424)
            };

            let card_brush = CreateSolidBrush(card_color);
            FillRect(hdc, &item_rect, card_brush);
            let _ = DeleteObject(HGDIOBJ(card_brush.0 as _));

            // 如果是选中态，在左侧绘制 Windows 11 Accent 蓝色竖向指示条
            if is_selected {
                let indicator_rect = RECT {
                    left: item_rect.left,
                    top: item_rect.top + (4.0 * scale) as i32,
                    right: item_rect.left + (4.0 * scale).round() as i32,
                    bottom: item_rect.bottom - (4.0 * scale) as i32,
                };
                let accent_brush = CreateSolidBrush(COLORREF(0x00d47800)); // Win11 Accent Blue
                FillRect(hdc, &indicator_rect, accent_brush);
                let _ = DeleteObject(HGDIOBJ(accent_brush.0 as _));
            }

            // 4.1 文件夹名称（左段单行排布）
            let title_left = item_rect.left + (14.0 * scale) as i32;
            let mut item_title_buf: Vec<u16> = item.title.encode_utf16().collect();
            
            // 测算文件夹名实际文字宽度，以便平滑紧接路径
            SelectObject(hdc, HGDIOBJ(font_item_title.0 as _));
            let mut measure_r = RECT::default();
            DrawTextW(hdc, &mut item_title_buf, &mut measure_r, DT_CALCRECT | DT_SINGLELINE);
            let title_w = (measure_r.right - measure_r.left).clamp((60.0 * scale) as i32, (220.0 * scale) as i32);

            let mut title_r = RECT {
                left: title_left,
                top: item_rect.top,
                right: title_left + title_w,
                bottom: item_rect.bottom,
            };
            SetTextColor(hdc, if is_selected { COLORREF(0x00ffffff) } else { COLORREF(0x00f0f0f0) });
            DrawTextW(hdc, &mut item_title_buf, &mut title_r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);

            // 4.2 右侧来源标签徽章（右段单行对齐）
            let tag_w = (110.0 * scale) as i32;
            let tag_right = item_rect.right - (12.0 * scale) as i32;
            let mut tag_r = RECT {
                left: tag_right - tag_w,
                top: item_rect.top,
                right: tag_right,
                bottom: item_rect.bottom,
            };
            SelectObject(hdc, HGDIOBJ(font_tag.0 as _));
            let tag_color = if item.is_active {
                COLORREF(0x0066d97a) // 活动绿色
            } else {
                COLORREF(0x00cca37a) // 浅青灰
            };
            SetTextColor(hdc, tag_color);
            let localized_source = translate_source(&item.source, &state.language);
            let tag_text = format!("[{}]", localized_source);
            let mut tag_buf: Vec<u16> = tag_text.encode_utf16().collect();
            DrawTextW(hdc, &mut tag_buf, &mut tag_r, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);

            // 4.3 完整路径（中段单行排布，平滑填充在文件夹名与标签之间）
            let path_left = title_left + title_w + (12.0 * scale) as i32;
            let path_right = tag_r.left - (12.0 * scale) as i32;
            if path_right > path_left {
                let mut path_r = RECT {
                    left: path_left,
                    top: item_rect.top,
                    right: path_right,
                    bottom: item_rect.bottom,
                };
                SelectObject(hdc, HGDIOBJ(font_path.0 as _));
                SetTextColor(hdc, if is_selected { COLORREF(0x00d0d0d0) } else { COLORREF(0x00888888) });
                let mut path_buf: Vec<u16> = item.path.encode_utf16().collect();
                DrawTextW(hdc, &mut path_buf, &mut path_r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            }

            curr_y += state.item_height;
        }

        let _ = DeleteObject(HGDIOBJ(font_header.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_item_title.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_path.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_tag.0 as _));
    }
}

fn translate_source<'a>(source: &str, lang: &'a Language) -> &'a str {
    match source {
        "资源管理器" => I18n::tag_explorer(lang),
        "最近历史" => I18n::tag_history(lang),
        "常用固定" => I18n::tag_pinned(lang),
        "系统目录" => I18n::tag_system(lang),
        _ => "Explorer",
    }
}


fn handle_mouse_move(hwnd: HWND, y: i32) {
    let state_ptr = FLOATING_BAR_INSTANCE.load(Ordering::SeqCst);
    if state_ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *state_ptr };

    if y < state.header_height {
        if state.hovered_index.is_some() {
            state.hovered_index = None;
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        return;
    }

    let rel_y = y - state.header_height;
    let idx = (rel_y / state.item_height) as usize;
    let visible_count = state.candidates.len().min(8);

    if idx < visible_count {
        if state.hovered_index != Some(idx) {
            state.hovered_index = Some(idx);
            state.selected_index = idx;
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    } else if state.hovered_index.is_some() {
        state.hovered_index = None;
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

fn handle_mouse_click(_hwnd: HWND, y: i32) {
    let state_ptr = FLOATING_BAR_INSTANCE.load(Ordering::SeqCst);
    if state_ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *state_ptr };

    if y >= state.header_height {
        let rel_y = y - state.header_height;
        let idx = (rel_y / state.item_height) as usize;
        let visible_count = state.candidates.len().min(8);

        if idx < visible_count {
            if let Some(target) = state.candidates.get(idx) {
                let path = target.path.clone();
                inject_path_to_dialog(
                    state.target_dialog_hwnd,
                    state.target_edit_hwnd,
                    &path,
                );
            }
            unsafe {
                let _ = ShowWindow(state.hwnd, SW_HIDE);
            }
        }
    }
}
