use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM, RECT, COLORREF, POINT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, UpdateWindow,
    DrawTextW, SetBkMode, SetTextColor, TRANSPARENT, DT_CENTER, DT_VCENTER, DT_WORDBREAK,
    GetDC, ReleaseDC,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowRect,
    GetWindowTextW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    SetWindowLongPtrW, ShowWindow, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HWND_TOPMOST,
    LWA_COLORKEY, SWP_NOACTIVATE, SW_SHOW, SW_HIDE, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_ERASEBKGND, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WNDCLASSW, HTCLIENT, HTTRANSPARENT,
};

use crate::core::tag::TAG_STORE;

const COLOR_KEY: COLORREF = COLORREF(0x00000000);
const DOT_RECT: RECT = RECT { left: 8, top: 8, right: 20, bottom: 20 };
const WM_MOUSELEAVE: u32 = 0x02A3;

static TARGET_MAP: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();

#[allow(dead_code)]
pub struct Overlay {
    hwnd: HWND,
    target_hwnd: HWND,
    running: AtomicBool,
}

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

        unsafe { let _ = RegisterClassW(&wc); }

        let mut rect = RECT::default();
        unsafe { GetWindowRect(target, &mut rect) }?;

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            anyhow::bail!("目标窗口尺寸无效: {}x{}", width, height);
        }

        let ex_style = WINDOW_EX_STYLE(
            WS_EX_LAYERED.0 | WS_EX_TOPMOST.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0,
        );

        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                PCWSTR(class_name.as_ptr()),
                windows::core::w!(""),
                WS_POPUP | WS_VISIBLE,
                rect.left, rect.top, width, height,
                None, None, None, None,
            )?
        };

        let map = TARGET_MAP.get_or_init(|| Mutex::new(HashMap::new()));
        map.lock().unwrap().insert(hwnd.0 as isize, target_hwnd);

        unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, COLOR_KEY, 0, LWA_COLORKEY);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
        }

        Ok(Overlay { hwnd, target_hwnd: target, running: AtomicBool::new(true) })
    }

    pub fn sync_position(&self) -> Result<()> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.target_hwnd, &mut rect) }?;
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 { return Ok(()); }
        unsafe { let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, rect.left, rect.top, w, h, SWP_NOACTIVATE); }
        Ok(())
    }

    pub fn hide(&self) { unsafe { let _ = ShowWindow(self.hwnd, SW_HIDE); } }
    pub fn show(&self) { unsafe { let _ = ShowWindow(self.hwnd, SW_SHOW); let _ = UpdateWindow(self.hwnd); } }
    pub fn is_running(&self) -> bool { self.running.load(Ordering::Relaxed) }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(map) = TARGET_MAP.get() {
            map.lock().unwrap().remove(&(self.hwnd.0 as isize));
        }
        unsafe { let _ = DestroyWindow(self.hwnd); }
    }
}

// ============================================================
// 覆盖层窗口过程
// ============================================================

extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            unsafe {
                let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut std::ffi::c_void);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let brush = CreateSolidBrush(COLOR_KEY);
                let _ = FillRect(hdc, &rect, brush);
                let _ = DeleteObject(brush);
            }
            LRESULT(1)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = Default::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let brush = CreateSolidBrush(COLORREF(0x0000_4DB7FF));
                let _ = FillRect(hdc, &DOT_RECT, brush);
                let _ = DeleteObject(brush);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut wr = RECT::default();
            unsafe { let _ = GetWindowRect(hwnd, &mut wr); }
            let cx = x - wr.left;
            let cy = y - wr.top;
            if cx >= DOT_RECT.left && cx < DOT_RECT.right && cy >= DOT_RECT.top && cy < DOT_RECT.bottom {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        WM_MOUSEMOVE => {
            handle_mouse_move(hwnd);
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        msg if msg == WM_MOUSELEAVE => {
            handle_mouse_leave(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ============================================================
// 悬停便签工具提示
// ============================================================

fn ensure_tooltip_class() {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::Relaxed) { return; }
    let class_name = widestring("WinTagTooltip");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(tooltip_wndproc),
        hInstance: windows::Win32::Foundation::HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    unsafe { let _ = RegisterClassW(&wc); }
}

fn handle_mouse_move(overlay_hwnd: HWND) {
    static TRACKING: AtomicBool = AtomicBool::new(false);
    if !TRACKING.swap(true, Ordering::Relaxed) {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: overlay_hwnd,
            dwHoverTime: 0,
        };
        unsafe { let _ = TrackMouseEvent(&mut tme); }
    }

    let old = unsafe { get_userdata::<std::ffi::c_void>(overlay_hwnd) as isize };
    if old != 0 { return; }

    let target_hwnd = get_target_hwnd(overlay_hwnd);
    if target_hwnd == 0 { return; }

    let (title, note) = match TAG_STORE.get() {
        Some(store) => {
            let s = store.lock().unwrap();
            match s.get(&target_hwnd) {
                Some(tag) => (tag.title.clone(), tag.note.clone()),
                None => return,
            }
        }
        None => return,
    };

    ensure_tooltip_class();

    let ex_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0);
    let style = WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0);
    let text = if note.is_empty() { title.clone() } else { format!("{}  -  {}", title, note) };

    let mut pt = POINT::default();
    unsafe { let _ = GetCursorPos(&mut pt); }

    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let tooltip_hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(widestring("WinTagTooltip").as_ptr()),
            PCWSTR(wide.as_ptr()),
            style,
            pt.x - 10, pt.y + 16, 300, 100,
            None, None, None, None,
        )
    };

    if let Ok(tooltip_hwnd) = tooltip_hwnd {
        unsafe { set_userdata(overlay_hwnd, tooltip_hwnd.0 as *mut std::ffi::c_void); }

        unsafe {
            let hdc = GetDC(tooltip_hwnd);
            let mut rc = RECT { left: 0, top: 0, right: 280, bottom: 0 };
            let _ = DrawTextW(hdc, &mut wide, &mut rc, DT_CENTER | DT_WORDBREAK | DT_VCENTER);
            let _ = ReleaseDC(tooltip_hwnd, hdc);
            let height = (rc.bottom - rc.top).max(20) + 20;
            let _ = SetWindowPos(tooltip_hwnd, HWND_TOPMOST, pt.x - 10, pt.y + 16, 300, height, SWP_NOACTIVATE);
        }
    }
}

fn handle_mouse_leave(overlay_hwnd: HWND) {
    static TRACKING: AtomicBool = AtomicBool::new(false);
    TRACKING.store(false, Ordering::Relaxed);

    let tooltip_ptr = unsafe { get_userdata::<std::ffi::c_void>(overlay_hwnd) as isize };
    if tooltip_ptr != 0 {
        unsafe { set_userdata(overlay_hwnd, std::ptr::null_mut()); }
        unsafe { let _ = DestroyWindow(HWND(tooltip_ptr as *mut std::ffi::c_void)); }
    }
}

fn get_target_hwnd(overlay_hwnd: HWND) -> isize {
    TARGET_MAP.get()
        .and_then(|map| map.lock().ok())
        .and_then(|map| map.get(&(overlay_hwnd.0 as isize)).copied())
        .unwrap_or(0)
}

unsafe fn get_userdata<T>(hwnd: HWND) -> *mut T {
    let old = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, old);
    old as *mut T
}

unsafe fn set_userdata(hwnd: HWND, data: *mut std::ffi::c_void) {
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize);
}

// ============================================================
// 工具提示窗口过程
// ============================================================

extern "system" fn tooltip_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe {
                let mut ps = Default::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let brush = CreateSolidBrush(COLORREF(0x00FFFFFF));
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let _ = FillRect(hdc, &rc, brush);
                let _ = DeleteObject(brush);

                let mut buf = [0u16; 512];
                let len = GetWindowTextW(hwnd, &mut buf) as usize;
                if len > 0 {
                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, COLORREF(0x00000000));
                    let mut tr = RECT { left: 10, top: 10, right: rc.right - 10, bottom: rc.bottom - 10 };
                    let _ = DrawTextW(hdc, &mut buf[..len], &mut tr, DT_WORDBREAK);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}