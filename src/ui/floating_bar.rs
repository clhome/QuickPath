#![allow(dead_code)]
// `show()` 是吸附条对外的展示入口，参数即对话框上下文（窗口/编辑框/矩形/候选/
// 透明度/语言/类型），拆包反而让调用端更难读（clippy::too_many_arguments 说明）。
#![allow(clippy::too_many_arguments)]

use crate::dialog::{DialogKind, inject_path_to_dialog};
use crate::managers::FolderCandidate;
use crate::managers::explicit_probe::TriggerSnapshot;
use crate::rules::{I18n, Language};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FW_BOLD, FW_NORMAL,
    FW_SEMIBOLD, FillRect, GetMonitorInfoW, GetStockObject, HBRUSH, HDC, HGDIOBJ, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, UpdateWindow, WHITE_BRUSH,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetSystemMetrics,
    GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LWA_ALPHA, LoadCursorW, RegisterClassW,
    SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_DESTROY,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WNDCLASSW,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

/// 读取悬浮吸附条实例的**非拥有**指针（存储在窗口私有存储 `GWLP_USERDATA`）。
/// 实例内存始终由 `FloatingBar` 内的 `Box<FloatingBarState>` 拥有，窗口侧只借用；
/// 因此窗口过程**绝不**在此 `Box::from_raw` 释放它（否则与 `FloatingBar` 的 drop 双重释放）。
unsafe fn bar_state(hwnd: HWND) -> *mut FloatingBarState {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FloatingBarState }
}

/// 写入（`Some`）或清空（`None`）悬浮吸附条实例指针。清空后窗口过程读到 0 即走
/// 「无实例则跳过」分支，杜绝读取到 `FloatingBar` drop 后已释放的内存。
unsafe fn set_bar_state(hwnd: HWND, state: Option<&mut FloatingBarState>) {
    let ptr = match state {
        Some(state) => std::ptr::from_mut(state),
        None => std::ptr::null_mut(),
    };
    let _ = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize) };
}

pub struct FloatingBarState {
    pub hwnd: HWND,
    pub target_dialog_hwnd: HWND,
    pub target_edit_hwnd: HWND,
    pub target_dialog_kind: DialogKind,
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
    pub producer_rect: RECT,
    pub is_producer_hovered: bool,
    /// 顶栏的「显式获取第三方管理器当前路径」按钮（审计报告 U-01）。
    /// 顺序固定为 `[Total Commander, XYplorer]`。
    pub probe_buttons: [ProbeButton; 2],
}

/// 顶栏「显式探测」按钮：只在用户已授权**且**冻结到有效目标快照时绘制
/// （授权 ≠ 自动探测；快照 ≠ 实时前台，见 `explicit_probe` 模块文档）。
#[derive(Debug, Clone, Copy)]
pub struct ProbeButton {
    /// 是否绘制（= 已授权 **且** 对话框打开事件冻结到了有效目标）
    pub visible: bool,
    /// 本次对话框 generation 冻结的不可变目标快照（点击只消费它，不再猜目标）
    pub target: Option<TriggerSnapshot>,
    pub rect: RECT,
    pub hovered: bool,
}

impl ProbeButton {
    const fn hidden() -> Self {
        Self {
            visible: false,
            target: None,
            rect: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            hovered: false,
        }
    }

    /// 命中测试:**剪贴板事务**进行中(独占 lease 已被占用)时一律不响应,
    /// 即使绘制态尚未刷新也不会发起第二个请求。
    ///
    /// 范围限定为剪贴板事务(报告 D-01C):WPS 仅走 UIA `SetValue` 时不建事务,
    /// 不在此处置灰;该情况下的并发冲突由 WPS 操作级 lease 拒绝。
    fn contains(&self, x: i32, y: i32) -> bool {
        self.visible
            && !crate::win32::clipboard::is_transaction_in_flight()
            && x >= self.rect.left
            && x <= self.rect.right
            && y >= self.rect.top
            && y <= self.rect.bottom
    }
}

pub struct FloatingBar {
    state: Box<FloatingBarState>,
}

impl FloatingBar {
    pub fn new() -> Result<Self, String> {
        let class_name = w!("QuickPath_FloatingBar_Class");
        unsafe {
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(floating_bar_wnd_proc),
                hInstance: HINSTANCE::default(),
                hCursor: cursor,
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
                target_dialog_kind: DialogKind::StandardWin32,
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
                producer_rect: RECT::default(),
                is_producer_hovered: false,
                probe_buttons: [ProbeButton::hidden(), ProbeButton::hidden()],
            });

            set_bar_state(hwnd, Some(&mut *state));

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
        kind: DialogKind,
        probe_targets: [Option<TriggerSnapshot>; 2],
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
            if GetDpiForMonitor(h_mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok()
                && dpi_x > 0
            {
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
        self.state.target_dialog_kind = kind;
        self.state.candidates = candidates;
        self.state.selected_index = 0;
        self.state.hovered_index = None;
        self.state.is_producer_hovered = false;
        // 审计报告 U-01 / U-01A：按钮是否出现取决于「已授权 **且** 本对话框打开事件
        // 冻结到了有效目标快照」；授权本身不发起探测，也不回退到任何墙钟记录。
        for (idx, target) in probe_targets.iter().enumerate() {
            self.state.probe_buttons[idx].visible = target.is_some();
            self.state.probe_buttons[idx].target = *target;
            self.state.probe_buttons[idx].hovered = false;
        }

        // 单行紧凑高密度展示，最多展示 8 行
        let visible_items = self.state.candidates.len().min(8) as i32;
        let total_height =
            self.state.header_height + visible_items * self.state.item_height + self.state.padding;

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
        // 对话框关闭：与本次对话框绑定的触发快照立即失效（U-01A generation 语义）。
        crate::managers::explicit_probe::invalidate_bar_targets();
        unsafe {
            let _ = ShowWindow(self.state.hwnd, SW_HIDE);
        }
    }

    /// 重新绘制（探测开始 / 结束等外部状态变化后由宿主调用）。
    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.state.hwnd), None, false);
        }
    }

    /// 当前绑定的目标对话框（对话框 / 文件名编辑框 / 对话框类型）。
    ///
    /// 供宿主在显式探测取得路径后决定「注入目标」；窗口已销毁或尚未绑定时返回 `None`。
    pub fn target_dialog(&self) -> Option<(HWND, HWND, DialogKind)> {
        let dialog = self.state.target_dialog_hwnd;
        if dialog.0.is_null()
            || !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(dialog)).as_bool() }
        {
            return None;
        }
        Some((
            dialog,
            self.state.target_edit_hwnd,
            self.state.target_dialog_kind,
        ))
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
            let dlg_hwnd = self.state.target_dialog_hwnd;
            let edit_hwnd = self.state.target_edit_hwnd;
            let kind = self.state.target_dialog_kind;
            self.hide();
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(dlg_hwnd);
            }
            inject_path_to_dialog(dlg_hwnd, edit_hwnd, &path, kind, None);
        }
    }
}

impl Drop for FloatingBar {
    fn drop(&mut self) {
        // 兜底置空窗口私有存储：`FloatingBarState` 的内存由本结构体的 `Box` 拥有，
        // 随本 `drop` 返回后立即释放；若不置空，仍存活窗口上的后续消息（`WM_PAINT` /
        // `WM_SETCURSOR` 等）会读到已释放内存。窗口若已被外部 `DestroyWindow` 销毁，
        // 则 `WM_NCDESTROY` 已置空，此处再次置空幂等（对无效 `hwnd` 调用无害）。
        unsafe {
            set_bar_state(self.state.hwnd, None);
        }
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
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_mouse_move(hwnd, x, y);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            handle_mouse_click(hwnd, x, y);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let state_ptr = unsafe { bar_state(hwnd) };
            if !state_ptr.is_null() {
                let state = unsafe { &*state_ptr };
                let cursor_id = if state.is_producer_hovered {
                    IDC_HAND
                } else {
                    IDC_ARROW
                };
                unsafe {
                    let _ = SetCursor(Some(LoadCursorW(None, cursor_id).unwrap_or_default()));
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DPICHANGED => {
            let new_dpi = (wparam.0 & 0xffff) as u32;
            let scale = (new_dpi as f32 / 96.0).max(1.0);
            let state_ptr = unsafe { bar_state(hwnd) };
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
        WM_NCDESTROY => {
            // 窗口销毁兜底置空：实例内存由 `FloatingBar` 的 `Box` 拥有，
            // 这里只清指针、绝不 `Box::from_raw`（否则与 `FloatingBar` 的 drop 双重释放）。
            unsafe {
                set_bar_state(hwnd, None);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn render_floating_bar(hwnd: HWND, hdc: HDC) {
    let state_ptr = unsafe { bar_state(hwnd) };
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
            font_title_size,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_item_title = CreateFontW(
            font_item_title_size,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );
        let font_path = CreateFontW(
            font_path_size,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );
        let font_tag = CreateFontW(
            font_tag_size,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );

        let font_producer = CreateFontW(
            (-12.0 * scale).round() as i32,
            0,
            0,
            0,
            if state.is_producer_hovered {
                FW_SEMIBOLD.0 as i32
            } else {
                FW_NORMAL.0 as i32
            },
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(1),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(5),
            0,
            w!("Microsoft YaHei UI"),
        );

        // 3. 绘制顶栏标题与右侧出品方（多国语言适配与官网跳转支持）
        let pad = state.padding;
        let right_edge = rect.right - pad - (6.0 * scale) as i32;

        // 3.1 测算并绘制右侧出品方文字（靠右显示）
        let producer_text = I18n::floating_producer(&state.language);
        let mut producer_buf: Vec<u16> = producer_text.encode_utf16().collect();

        SelectObject(hdc, HGDIOBJ(font_producer.0 as _));
        let mut measure_r = RECT::default();
        DrawTextW(
            hdc,
            &mut producer_buf,
            &mut measure_r,
            DT_CALCRECT | DT_SINGLELINE,
        );
        let prod_w = measure_r.right - measure_r.left;

        let prod_right = right_edge;
        let prod_left = prod_right - prod_w;
        let mut prod_draw_rect = RECT {
            left: prod_left,
            top: (2.0 * scale) as i32,
            right: prod_right,
            bottom: state.header_height,
        };

        // 记录出品方热区（左右预留适当点击余量，垂直覆盖顶栏）
        let state_mut = &mut *state_ptr;
        state_mut.producer_rect = RECT {
            left: prod_left - (4.0 * scale) as i32,
            top: 0,
            right: prod_right + (4.0 * scale) as i32,
            bottom: state.header_height,
        };

        // 悬停时 Fluent Accent 高亮天蓝 #60cdff (0x00ffcd60)，普通态为优雅柔和灰白 #9c9ca4 (0x00a49c9c)
        let prod_color = if state.is_producer_hovered {
            COLORREF(0x00ffcd60)
        } else {
            COLORREF(0x00a49c9c)
        };
        SetTextColor(hdc, prod_color);
        DrawTextW(
            hdc,
            &mut producer_buf,
            &mut prod_draw_rect,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
        );

        // 3.2 绘制左侧主标题与快捷提示（右边界留出安全间距，杜绝重叠）
        //     若显式探测按钮可见（审计报告 U-01），标题右边界还要让开按钮区。
        let header_left = pad + (6.0 * scale) as i32;
        let header_height = state.header_height;
        let mut header_right_limit = prod_left;
        // 3.1b 显式探测按钮：[TC] [XY] 紧贴出品方左侧，仅授权后出现
        {
            let state_mut = &mut *state_ptr;
            let btn_w = (46.0 * scale).round() as i32;
            let btn_h = (20.0 * scale).round() as i32;
            let btn_gap = (6.0 * scale).round() as i32;
            let btn_top = (header_height - btn_h) / 2;
            let mut right = prod_left - (12.0 * scale) as i32;
            // 从右往左排布：数组顺序为 [TotalCmd, XYplorer]，故倒序遍历
            for idx in (0..state_mut.probe_buttons.len()).rev() {
                let visible = state_mut.probe_buttons[idx].visible;
                let rect = RECT {
                    left: right - btn_w,
                    top: btn_top,
                    right,
                    bottom: btn_top + btn_h,
                };
                state_mut.probe_buttons[idx].rect = rect;
                if !visible {
                    continue;
                }
                header_right_limit = header_right_limit.min(rect.left);

                // 忙碌态只反映**剪贴板事务**(报告 D-01C),不作为安全边界。
                let in_flight = crate::win32::clipboard::is_transaction_in_flight();
                let hovered = state_mut.probe_buttons[idx].hovered && !in_flight;
                let bg = if in_flight {
                    COLORREF(0x001a1a1a)
                } else if hovered {
                    COLORREF(0x00453e38)
                } else {
                    COLORREF(0x00282424)
                };
                let brush = CreateSolidBrush(bg);
                FillRect(hdc, &rect, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0 as _));

                SelectObject(hdc, HGDIOBJ(font_tag.0 as _));
                SetTextColor(
                    hdc,
                    if in_flight {
                        // 探测进行中：置灰 + 省略号，明确“已有一次探测在跑”（P0-06）
                        COLORREF(0x00606060)
                    } else if hovered {
                        COLORREF(0x00ffcd60)
                    } else {
                        COLORREF(0x00a49c9c)
                    },
                );
                let label = if in_flight {
                    "…"
                } else if idx == 0 {
                    "TC"
                } else {
                    "XY"
                };
                let mut label_buf: Vec<u16> = label.encode_utf16().collect();
                let mut label_rect = rect;
                DrawTextW(
                    hdc,
                    &mut label_buf,
                    &mut label_rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
                right = rect.left - btn_gap;
            }
        }
        let header_right = (header_right_limit - (16.0 * scale) as i32).max(header_left);
        let mut header_rect = RECT {
            left: header_left,
            top: (2.0 * scale) as i32,
            right: header_right,
            bottom: state.header_height,
        };
        SelectObject(hdc, HGDIOBJ(font_header.0 as _));
        SetTextColor(hdc, COLORREF(0x00e0e0e0));
        let title_text = I18n::floating_title(&state.language);
        let mut title_buf: Vec<u16> = title_text.encode_utf16().collect();
        DrawTextW(
            hdc,
            &mut title_buf,
            &mut header_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );

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
            DrawTextW(
                hdc,
                &mut item_title_buf,
                &mut measure_r,
                DT_CALCRECT | DT_SINGLELINE,
            );
            let title_w = (measure_r.right - measure_r.left)
                .clamp((60.0 * scale) as i32, (220.0 * scale) as i32);

            let mut title_r = RECT {
                left: title_left,
                top: item_rect.top,
                right: title_left + title_w,
                bottom: item_rect.bottom,
            };
            SetTextColor(
                hdc,
                if is_selected {
                    COLORREF(0x00ffffff)
                } else {
                    COLORREF(0x00f0f0f0)
                },
            );
            DrawTextW(
                hdc,
                &mut item_title_buf,
                &mut title_r,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );

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
            DrawTextW(
                hdc,
                &mut tag_buf,
                &mut tag_r,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
            );

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
                SetTextColor(
                    hdc,
                    if is_selected {
                        COLORREF(0x00d0d0d0)
                    } else {
                        COLORREF(0x00888888)
                    },
                );
                let mut path_buf: Vec<u16> = item.path.encode_utf16().collect();
                DrawTextW(
                    hdc,
                    &mut path_buf,
                    &mut path_r,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                );
            }

            curr_y += state.item_height;
        }

        let _ = DeleteObject(HGDIOBJ(font_header.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_producer.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_item_title.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_path.0 as _));
        let _ = DeleteObject(HGDIOBJ(font_tag.0 as _));
    }
}

fn translate_source<'a>(source: &'a str, lang: &'a Language) -> &'a str {
    match source {
        "资源管理器" => I18n::tag_explorer(lang),
        "最近历史" => I18n::tag_history(lang),
        "常用固定" => I18n::tag_pinned(lang),
        "系统目录" => I18n::tag_system(lang),
        // 未收录的来源（如 "Directory Opus"）直接显示原名：
        // 旧实现一律回退为 "Explorer"，会把第三方管理器的候选误标成资源管理器。
        _ => source,
    }
}

fn handle_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let state_ptr = unsafe { bar_state(hwnd) };
    if state_ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *state_ptr };

    // 1. 判断是否悬停在顶栏右侧出品方文字上
    let in_producer = x >= state.producer_rect.left
        && x <= state.producer_rect.right
        && y >= state.producer_rect.top
        && y <= state.producer_rect.bottom;

    // 1b. 顶栏显式探测按钮（审计报告 U-01）
    let mut in_probe_button = false;
    for idx in 0..state.probe_buttons.len() {
        let button = state.probe_buttons[idx];
        let hit = button.contains(x, y);
        in_probe_button |= hit;
        if hit != button.hovered {
            state.probe_buttons[idx].hovered = hit;
            unsafe {
                let r = button.rect;
                let _ = InvalidateRect(Some(hwnd), Some(&r), false);
            }
        }
    }

    if in_producer != state.is_producer_hovered {
        state.is_producer_hovered = in_producer;
        unsafe {
            let r = state.producer_rect;
            let _ = InvalidateRect(Some(hwnd), Some(&r), false);
        }
    }

    let cursor_id = if in_producer || in_probe_button {
        IDC_HAND
    } else {
        IDC_ARROW
    };
    unsafe {
        let _ = SetCursor(Some(LoadCursorW(None, cursor_id).unwrap_or_default()));
    }

    // 2. 位于顶栏范围时，取消下方候选行的高亮并直接返回
    if y < state.header_height {
        if state.hovered_index.is_some() {
            state.hovered_index = None;
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        return;
    }

    // 3. 鼠标离开顶栏进入候选项区域，确保出品方悬停状态重置
    if state.is_producer_hovered {
        state.is_producer_hovered = false;
        unsafe {
            let r = state.producer_rect;
            let _ = InvalidateRect(Some(hwnd), Some(&r), false);
        }
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

fn handle_mouse_click(hwnd: HWND, x: i32, y: i32) {
    let state_ptr = unsafe { bar_state(hwnd) };
    if state_ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *state_ptr };

    // 0. 顶栏「显式探测」按钮（审计报告 U-01 / U-01A）：点击只消费**本对话框
    //    generation 冻结的不可变快照**，不再去猜目标窗口；授权、lease、剪贴板事务的
    //    fail-closed 判定全部在宿主侧复核。这里不做任何同步等待，避免卡住吸附条窗口过程。
    for idx in 0..state.probe_buttons.len() {
        if state.probe_buttons[idx].contains(x, y) {
            let target = state.probe_buttons[idx].target;
            // 立即重绘为忙碌态（宿主在后台线程探测，不阻塞本窗口过程）
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            crate::request_explicit_probe_with_snapshot(target);
            return;
        }
    }

    // 1. 点击顶栏右侧出品方文字 -> 跳转官网 https://qp.yftec.top 并隐藏浮动栏
    if x >= state.producer_rect.left
        && x <= state.producer_rect.right
        && y >= state.producer_rect.top
        && y <= state.producer_rect.bottom
    {
        let url: Vec<u16> = "https://qp.yftec.top\0".encode_utf16().collect();
        let op: Vec<u16> = "open\0".encode_utf16().collect();
        unsafe {
            ShellExecuteW(
                None,
                windows::core::PCWSTR(op.as_ptr()),
                windows::core::PCWSTR(url.as_ptr()),
                windows::core::PCWSTR::null(),
                windows::core::PCWSTR::null(),
                SW_SHOW,
            );
            let _ = ShowWindow(state.hwnd, SW_HIDE);
        }
        return;
    }

    // 2. 点击候选项 -> 注入路径到目标对话框
    if y >= state.header_height {
        let rel_y = y - state.header_height;
        let idx = (rel_y / state.item_height) as usize;
        let visible_count = state.candidates.len().min(8);

        if idx < visible_count
            && let Some(target) = state.candidates.get(idx)
        {
            let path = target.path.clone();
            let dlg_hwnd = state.target_dialog_hwnd;
            let edit_hwnd = state.target_edit_hwnd;
            let kind = state.target_dialog_kind;
            unsafe {
                let _ = ShowWindow(state.hwnd, SW_HIDE);
                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(dlg_hwnd);
            }
            inject_path_to_dialog(dlg_hwnd, edit_hwnd, &path, kind, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

    /// 回归守卫（R-05c）：`FloatingBar` 持有 `FloatingBarState` 的所有权，
    /// 实例 drop 后必须把窗口私有存储 `GWLP_USERDATA` 置空，否则仍存活的窗口
    /// （本程序退出前不销毁吸附条窗口）在后续 `WM_PAINT`/`WM_SETCURSOR` 里会读到
    /// 已释放的 `Box` 内部指针。此用例直接观测置空结果。
    #[test]
    fn drop_clears_window_userdata_to_avoid_dangling_read() {
        let bar = FloatingBar::new().expect("创建悬浮吸附条窗口失败");
        let hwnd = bar.hwnd();
        assert!(!hwnd.0.is_null(), "FloatingBar::new 必须返回有效 HWND");

        // new() 写入的是非拥有指针，指向 FloatingBar 自己持有的 Box 内部。
        let stored = unsafe { bar_state(hwnd) };
        assert!(!stored.is_null(), "new() 必须把实例指针写入 GWLP_USERDATA");
        assert_eq!(
            stored,
            std::ptr::from_ref(&*bar.state).cast_mut(),
            "窗口侧存的应是非拥有指针（FloatingBar 仍持有 Box）"
        );

        drop(bar);
        let after = unsafe { bar_state(hwnd) };
        assert!(
            after.is_null(),
            "FloatingBar drop 后 GWLP_USERDATA 必须置空，杜绝悬垂读取"
        );

        // 清理：销毁窗口触发 WM_NCDESTROY 兜底置空（此时已为 0，幂等且不 double free）。
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}
