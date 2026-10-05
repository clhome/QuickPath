use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::Ime::{
    HIMC, ImmGetContext, ImmGetOpenStatus, ImmReleaseContext, ImmSetOpenStatus,
};

/// 输入法状态隔离守卫 (RAII)
/// 在向输入控件注入路径前临时关闭中文输入法，防止输入法顶词拦截，在离开作用域时自动恢复原状态
pub struct ImeGuard {
    hwnd: HWND,
    himc: HIMC,
    was_open: bool,
}

impl ImeGuard {
    pub fn new(hwnd: HWND) -> Self {
        unsafe {
            let himc = ImmGetContext(hwnd);
            let mut was_open = false;
            if !himc.is_invalid() {
                was_open = ImmGetOpenStatus(himc).as_bool();
                if was_open {
                    // 临时关闭输入法状态
                    let _ = ImmSetOpenStatus(himc, false);
                }
            }
            Self {
                hwnd,
                himc,
                was_open,
            }
        }
    }
}

impl Drop for ImeGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.himc.is_invalid() {
                if self.was_open {
                    // 恢复输入法原本的打开状态
                    let _ = ImmSetOpenStatus(self.himc, true);
                }
                let _ = ImmReleaseContext(self.hwnd, self.himc);
            }
        }
    }
}
