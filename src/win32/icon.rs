#![allow(dead_code)]

use std::sync::OnceLock;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::Graphics::GdiPlus::{
    GdiplusStartup, GdiplusStartupInput, GdipCreateBitmapFromFile,
    GdipCreateBitmapFromStream, GdipCreateFromHDC, GdipDeleteGraphics,
    GdipDrawImageRectI, GdipSetInterpolationMode, GpBitmap, GpGraphics,
    GpImage, InterpolationModeHighQualityBicubic, Status,
};
use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
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

static GDIP_TOKEN: OnceLock<usize> = OnceLock::new();
static LOGO_BITMAP: OnceLock<usize> = OnceLock::new();

/// 初始化 GDI+ 环境
pub fn ensure_gdiplus() {
    GDIP_TOKEN.get_or_init(|| {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: windows::core::BOOL(0),
            SuppressExternalCodecs: windows::core::BOOL(0),
        };
        let mut token = 0;
        unsafe {
            let _ = GdiplusStartup(&mut token, &input, std::ptr::null_mut());
        }
        token
    });
}

/// 获取高清 logo.png 图片句柄
/// 优先加载本地 assets/logo.png 文件；若无则自动回退加载嵌入式资源流
pub fn get_logo_png_image() -> Option<*mut GpImage> {
    ensure_gdiplus();
    let ptr = LOGO_BITMAP.get_or_init(|| {
        unsafe {
            let mut bitmap: *mut GpBitmap = std::ptr::null_mut();

            // 1. 尝试从本地 assets/logo.png 加载
            let paths = ["assets/logo.png", "logo.png"];
            for p in paths {
                if std::path::Path::new(p).exists() {
                    let wide: Vec<u16> = p.encode_utf16().chain(Some(0)).collect();
                    if GdipCreateBitmapFromFile(PCWSTR(wide.as_ptr()), &mut bitmap) == Status(0) && !bitmap.is_null() {
                        return bitmap as usize;
                    }
                }
            }

            // 2. 备用：从编译嵌入的 logo.png 二进制流解码加载
            let bytes = include_bytes!("../../assets/logo.png");
            if let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) {
                if !hglobal.0.is_null() {
                    let dest = GlobalLock(hglobal);
                    if !dest.is_null() {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest as *mut u8, bytes.len());
                        let _ = GlobalUnlock(hglobal);
                        if let Ok(stream) = CreateStreamOnHGlobal(hglobal, true) {
                            if GdipCreateBitmapFromStream(&stream, &mut bitmap) == Status(0) && !bitmap.is_null() {
                                return bitmap as usize;
                            }
                        }
                    }
                }
            }

            0
        }
    });

    if *ptr != 0 {
        Some(*ptr as *mut GpImage)
    } else {
        None
    }
}

/// 高质量平滑绘制 logo.png 原图
/// 支持自动以双三次平滑采样（Bicubic）缩放至指定尺寸
pub unsafe fn draw_logo_png(
    hdc: HDC,
    dest_x: i32,
    dest_y: i32,
    dest_w: i32,
    dest_h: i32,
) -> bool {
    if let Some(img) = get_logo_png_image() {
        unsafe {
            let mut graphics: *mut GpGraphics = std::ptr::null_mut();
            if GdipCreateFromHDC(hdc, &mut graphics) == Status(0) && !graphics.is_null() {
                let _ = GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);
                let _ = GdipDrawImageRectI(graphics, img, dest_x, dest_y, dest_w, dest_h);
                let _ = GdipDeleteGraphics(graphics);
                return true;
            }
        }
    }
    false
}
