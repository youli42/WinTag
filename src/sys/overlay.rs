use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM, RECT, COLORREF};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, UpdateWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowRect, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    HWND_TOPMOST, LWA_COLORKEY, SWP_NOACTIVATE, SW_SHOW, SW_HIDE, WINDOW_EX_STYLE,
    WM_ERASEBKGND, WM_PAINT, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE, WNDCLASSW,
};

/// 透明色键（纯黑，RGB(0,0,0)）
const COLOR_KEY: COLORREF = COLORREF(0x00000000);

#[allow(dead_code)]
pub struct Overlay {
    hwnd: HWND,
    target_hwnd: HWND,
    running: AtomicBool,
}

// SAFETY: HWND 是线程安全的指针，Overlay 仅在主线程操作
unsafe impl Send for Overlay {}
unsafe impl Sync for Overlay {}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overlay")
            .field("hwnd", &self.hwnd.0)
            .field("target_hwnd", &self.target_hwnd.0)
            .finish()
    }
}

#[allow(dead_code)]
impl Overlay {
    #[allow(dead_code)]
    pub fn create(target_hwnd: isize) -> Result<Self> {
        let target = HWND(target_hwnd as *mut std::ffi::c_void);
        let class_name = widestring("WinTagOverlay");

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wndproc),
            hInstance: windows::Win32::Foundation::HINSTANCE::default(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };

        // SAFETY: 注册自定义窗口类
        unsafe {
            let _ = RegisterClassW(&wc);
        }

        let mut rect = RECT::default();
        // SAFETY: GetWindowRect 是只读 API
        unsafe { GetWindowRect(target, &mut rect) }?;

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;

        if width <= 0 || height <= 0 {
            anyhow::bail!("目标窗口尺寸无效: {}x{}", width, height);
        }

        let ex_style = WINDOW_EX_STYLE(
            WS_EX_LAYERED.0
                | WS_EX_TRANSPARENT.0
                | WS_EX_TOPMOST.0
                | WS_EX_NOACTIVATE.0
                | WS_EX_TOOLWINDOW.0,
        );

        // SAFETY: 创建覆盖层窗口，参数合法
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                PCWSTR(class_name.as_ptr()),
                windows::core::w!(""),
                WS_POPUP | WS_VISIBLE,
                rect.left,
                rect.top,
                width,
                height,
                None,
                None,
                None,
                None,
            )
        }?;

        // 设置透明色键：黑色像素变为透明
        // SAFETY: hwnd 是有效窗口句柄
        unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, COLOR_KEY, 0, LWA_COLORKEY);
        }

        // SAFETY: 显示覆盖层窗口
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
        }

        Ok(Overlay {
            hwnd,
            target_hwnd: target,
            running: AtomicBool::new(true),
        })
    }

    pub fn sync_position(&self) -> Result<()> {
        let mut rect = RECT::default();
        // SAFETY: GetWindowRect 是只读 API
        unsafe { GetWindowRect(self.target_hwnd, &mut rect) }?;

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;

        if width <= 0 || height <= 0 {
            return Ok(());
        }

        // SAFETY: SetWindowPos 移动窗口，参数合法
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                width,
                height,
                SWP_NOACTIVATE,
            );
        }

        Ok(())
    }

    pub fn hide(&self) {
        // SAFETY: ShowWindow 隐藏窗口
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn show(&self) {
        // SAFETY: ShowWindow 显示窗口
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // SAFETY: 销毁覆盖层窗口
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// 覆盖层窗口过程
#[allow(dead_code)]
extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            // 用黑色（透明色键）填充背景
            // SAFETY: wparam 是 HDC
            unsafe {
                let hdc =
                    windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut std::ffi::c_void);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let brush = CreateSolidBrush(COLOR_KEY);
                let _ = FillRect(hdc, &rect, brush);
                let _ = DeleteObject(brush);
            }
            LRESULT(1)
        }
        WM_PAINT => {
            // SAFETY: 基本绘制操作
            unsafe {
                let mut ps = Default::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                // 橙色圆点标记 RBG(0xFF, 0xB7, 0x4D) -> COLORREF = 0x00_4DB7FF
                let brush = CreateSolidBrush(COLORREF(0x0000_4DB7FF));
                let mark_rect = RECT {
                    left: 8,
                    top: 8,
                    right: 20,
                    bottom: 20,
                };
                let _ = FillRect(hdc, &mark_rect, brush);
                let _ = DeleteObject(brush);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: 默认窗口过程
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

#[allow(dead_code)]
fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}