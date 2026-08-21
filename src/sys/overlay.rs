use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, ReleaseDC,
    SetBkMode, SetTextColor, UpdateWindow, DT_CENTER, DT_VCENTER, DT_WORDBREAK, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowRect,
    GetWindowTextW, IsIconic, IsWindowVisible, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST,
    LWA_COLORKEY, SWP_NOACTIVATE, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND,
    WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use super::TagStore;
use crate::common::{get_userdata, set_userdata, widestring};

const COLOR_KEY: COLORREF = COLORREF(0x00000000);
/// 圆点回退颜色（橙色，BGR 布局：B=0x4D, G=0xB7, R=0xFF，对应 RGBA [255, 183, 77, 255]）
const FALLBACK_DOT_COLOR: COLORREF = COLORREF(0x0000_4DB7FF);
const DOT_RECT: RECT = RECT {
    left: 8,
    top: 8,
    right: 20,
    bottom: 20,
};
const WM_MOUSELEAVE: u32 = 0x02A3;

/// 单个覆盖层的悬停状态
///
/// `tracking` 记录本覆盖层是否已臂定 `TME_LEAVE`（鼠标离开通知）：
/// - [`handle_mouse_move`] 将其置位（`swap(true)`），并据此决定是否臂定；
/// - [`handle_mouse_leave`] 将其复位，使下一次悬停可重新臂定。
struct OverlayState {
    /// 覆盖层所跟随的目标窗口句柄
    target_hwnd: isize,
    /// 是否已臂定 TME_LEAVE
    tracking: AtomicBool,
}

/// 覆盖层注册表：覆盖层 HWND → 悬停状态
///
/// 仅在主线程（隐藏窗口 WndProc 消息处理链）内读写。
static TARGET_MAP: OnceLock<Mutex<HashMap<isize, OverlayState>>> = OnceLock::new();

/// 注入的全局标签存储（供悬停 tooltip 查询标签内容）
///
/// 通过 [`set_tag_store`] 注入；未注入时悬停静默（不显示 tooltip）。
static TAG_STORE_INNER: OnceLock<Arc<Mutex<TagStore>>> = OnceLock::new();

/// 注入全局标签存储引用
///
/// 必须在任何覆盖层创建之前调用（通常在程序启动、消息循环开始前）。
/// 注入后，覆盖层悬停查询即从该存储读取标签内容；
/// 未调用本函数时悬停静默：不显示 tooltip，也不产生任何错误。
pub fn set_tag_store(store: Arc<Mutex<TagStore>>) {
    let _ = TAG_STORE_INNER.set(store);
}

/// 注入的 tooltip 主题配色（元组：(背景色, 前景色)，`COLORREF` 为 `0x00BBGGRR` 布局）
///
/// 通过 [`set_tooltip_theme`] 注入；未注入时 tooltip 回退默认白底黑字，
/// 与注入前的行为完全一致。采用与 [`TAG_STORE_INNER`] 相同的 OnceLock 注入模式，
/// 重复调用仅首次生效。
///
/// 说明：`OnceLock` 本身已保证"写一次、读多次"的线程安全语义，
/// 值不可变，因此无需再包一层 `Mutex`（读取用 `.get()` 直接取引用，无需加锁）。
static TOOLTIP_THEME: OnceLock<(COLORREF, COLORREF)> = OnceLock::new();

/// 注入 tooltip 主题配色
///
/// 必须在任何 tooltip 显示之前调用（通常在程序启动、消息循环开始前）。
/// 未调用本函数时 tooltip 保持默认白底黑字；重复调用仅首次生效
/// （与 [`set_tag_store`] 一致的 OnceLock 语义）。
pub fn set_tooltip_theme(bg: COLORREF, fg: COLORREF) {
    let _ = TOOLTIP_THEME.set((bg, fg));
}

/// 透明覆盖层窗口
///
/// 覆盖层是一个 `WS_EX_LAYERED` 穿透式透明窗口，绘制在目标窗口左上角
/// 作为"已标记"指示圆点；鼠标悬停时弹出便签 tooltip。
///
/// 覆盖层窗口仅在主线程创建、访问与销毁，其生命周期与 `Overlay` 值绑定：
/// `Drop` 时先销毁仍显示的悬停 tooltip，再从 [`TARGET_MAP`] 注销，最后销毁自身窗口。
#[allow(dead_code)]
pub struct Overlay {
    hwnd: HWND,
    target_hwnd: HWND,
    running: AtomicBool,
    /// 覆盖层窗口当前是否可见（与目标窗口可见性联动）
    ///
    /// 仅主线程读写。事件驱动路径（Hide/Show 动作）与 500ms 兜底轮询
    /// （[`crate::main::poll_overlays`] 的可见性校正）共用此状态，
    /// 用于避免对同一窗口重复调用 `ShowWindow`。
    visible: AtomicBool,
    /// 上次已同步应用的目标窗口矩形（DWM 边界，元组为 left/top/width/height）
    ///
    /// 仅主线程读写，用于 [`sync_position`] 的变更去重：与当前矩形一致时跳过
    /// `SetWindowPos`。用 `Mutex` 承载以保持 `unsafe impl Send/Sync` 的一致性约定。
    last_rect: Mutex<Option<(i32, i32, i32, i32)>>,
}

// SAFETY: Overlay 仅承载 HWND 与原子状态，手动实现 Send/Sync 的依据如下：
// 1. windows-rs 0.58 的 HWND 未实现 Send/Sync，但本程序是单线程消息泵架构——
//    Overlay 及其底层 HWND 只在主线程创建、访问与销毁（见 main.rs 消息循环与各 WndProc）；
// 2. TARGET_MAP / TAG_STORE_INNER 等全局状态只在主线程（隐藏窗口 WndProc 消息处理链）内触碰；
// 3. running 为 AtomicBool，Drop 中仅自读自写，无跨线程数据竞争。
// 因此即使 HWND 缺乏自动 Send/Sync 标记，此处手动实现也不会产生未定义行为。
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
    /// 为目标窗口创建透明覆盖层
    ///
    /// - `target_hwnd`：目标窗口句柄（以 `isize` 表示）
    /// - 成功返回覆盖层；目标窗口尺寸无效或窗口创建失败时返回错误
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

        // SAFETY: RegisterClassW 失败（如类已注册）仅返回 0，此处忽略；
        // 类数据在覆盖层窗口生命周期内保持有效。
        unsafe {
            let _ = RegisterClassW(&wc);
        }

        let mut rect = RECT::default();
        // SAFETY: target 句柄由调用方保证存活（创建请求来自该窗口仍在前台时）；
        // GetWindowRect 为只读查询，句柄失效时返回错误由 `?` 向上传播。
        unsafe { GetWindowRect(target, &mut rect) }?;

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            anyhow::bail!("目标窗口尺寸无效: {}x{}", width, height);
        }

        let ex_style = WINDOW_EX_STYLE(
            WS_EX_LAYERED.0 | WS_EX_TOPMOST.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0,
        );

        // SAFETY: 传入已注册的窗口类名、有效样式与尺寸，父窗口/菜单/实例均不参与
        // （None）；创建失败（如资源不足）返回错误由 `?` 传播。
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
            )?
        };

        // 登记本覆盖层悬停状态（tracking 初始为 false，首次悬停即可臂定 TME_LEAVE）
        let map = TARGET_MAP.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(mut map) = map.lock() {
            map.insert(
                hwnd.0 as isize,
                OverlayState {
                    target_hwnd,
                    tracking: AtomicBool::new(false),
                },
            );
        }

        // SAFETY: hwnd 为本函数刚创建且存活的窗口；SetLayeredWindowAttributes 设置透明
        // 色键，ShowWindow/UpdateWindow 触发首次绘制，均为无害操作，失败忽略。
        unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, COLOR_KEY, 0, LWA_COLORKEY);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
        }

        Ok(Overlay {
            hwnd,
            target_hwnd: target,
            running: AtomicBool::new(true),
            visible: AtomicBool::new(true),
            last_rect: Mutex::new(None),
        })
    }

    /// 将覆盖层位置与尺寸同步到目标窗口当前可见区域（窗口移动/缩放时调用）
    ///
    /// 优先使用 DWM 扩展帧边界（[`DWMWA_EXTENDED_FRAME_BOUNDS`]）获取目标窗口的
    /// **可见区域**：该边界不含隐形 resize 边框，与用户视觉感知一致；在 Per-Monitor
    /// v2 DPI 感知下与物理像素一致，不受系统 DPI 缩放调整。获取失败时回退到
    /// `GetWindowRect`（含隐形边框的窗口矩形）。
    ///
    /// 可见性守卫：目标窗口最小化（`IsIconic`）或不可见（`!IsWindowVisible`）时
    /// **自动隐藏覆盖层**——这是 500ms 兜底轮询与事件丢失场景下的可见性校正路径，
    /// 确保目标最小化/隐藏后覆盖层不会悬浮残留；目标恢复可见后由 [`show`] 恢复。
    /// 变更去重：矩形与上次已应用结果一致时跳过 `SetWindowPos`，避免无谓窗口重排。
    pub fn sync_position(&self) -> Result<()> {
        // SAFETY: self.target_hwnd 由本 Overlay 持有且窗口存活；IsIconic/IsWindowVisible
        // 为只读查询，句柄失效时返回 FALSE/0，此处视为"不短路"，由后续窗口查询报错。
        let target_visible = unsafe {
            !IsIconic(self.target_hwnd).as_bool() && IsWindowVisible(self.target_hwnd).as_bool()
        };
        if !target_visible {
            // 目标窗口不可见：隐藏覆盖层（幂等），避免其悬浮在桌面其他窗口之上
            self.hide();
            return Ok(());
        }

        let mut rect = RECT::default();
        // SAFETY: self.target_hwnd 由本 Overlay 持有且窗口存活；DwmGetWindowAttribute 为
        // 只读查询，rect 为栈上有效缓冲区，cbattribute 为 RECT 大小；失败时回退 GetWindowRect。
        let dwm_ok = unsafe {
            DwmGetWindowAttribute(
                self.target_hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut rect as *mut RECT as *mut std::ffi::c_void,
                std::mem::size_of::<RECT>() as u32,
            )
            .is_ok()
        };
        if !dwm_ok {
            // SAFETY: self.target_hwnd 由本 Overlay 持有且窗口存活；
            // GetWindowRect 为只读查询，句柄失效时返回错误由 `?` 向上传播。
            unsafe { GetWindowRect(self.target_hwnd, &mut rect) }?;
        }

        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 {
            return Ok(());
        }

        // 变更去重：与上次已应用的矩形一致时跳过 SetWindowPos；锁中毒时继续执行，
        // 保持与之前相同的定位行为（下次同步仍会重新去重）。
        let current = (rect.left, rect.top, w, h);
        if let Ok(mut last) = self.last_rect.lock() {
            if last.as_ref() == Some(&current) {
                return Ok(());
            }
            *last = Some(current);
        }

        // SAFETY: self.hwnd 与 self.target_hwnd 由本 Overlay 持有且窗口存活；
        // SetWindowPos 仅调整位置尺寸，失败忽略（下次同步会重试）。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                w,
                h,
                SWP_NOACTIVATE,
            );
        }
        Ok(())
    }

    /// 隐藏覆盖层（幂等：已隐藏时不重复调用 `ShowWindow`）
    ///
    /// 同时更新 [`Overlay::visible`] 状态，供 500ms 兜底轮询的可见性校正判断。
    /// 隐藏时顺带销毁仍显示的悬停 tooltip，避免最小化/隐藏后 tooltip 残留。
    pub fn hide(&self) {
        // 已在隐藏状态时跳过，避免对同一窗口反复调用 ShowWindow(SW_HIDE)
        if !self.visible.load(Ordering::Relaxed) {
            return;
        }
        self.visible.store(false, Ordering::Relaxed);
        self.destroy_tooltip();
        // SAFETY: self.hwnd 由本 Overlay 独占持有，Drop 前始终存活。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// 销毁覆盖层上仍显示的悬停 tooltip（若存在）
    ///
    /// 先置空 userdata 再销毁 tooltip 窗口，防止 tooltip WndProc 后续访问悬垂指针。
    /// 在 [`Overlay::hide`] 与 `Drop` 中复用，避免重复实现。
    fn destroy_tooltip(&self) {
        // SAFETY: 主线程消息循环内执行；self.hwnd 存活，读取 userdata 无副作用。
        let tooltip_ptr = unsafe { get_userdata::<std::ffi::c_void>(self.hwnd) } as isize;
        if tooltip_ptr != 0 {
            unsafe {
                set_userdata(self.hwnd, std::ptr::null_mut());
                let _ = DestroyWindow(HWND(tooltip_ptr as *mut std::ffi::c_void));
            }
        }
    }

    /// 显示覆盖层并触发重绘（幂等：已显示时不重复调用）
    ///
    /// 同时更新 [`Overlay::visible`] 状态，供 500ms 兜底轮询的可见性校正判断。
    pub fn show(&self) {
        // 已显示时跳过，避免对同一窗口反复调用 ShowWindow(SW_SHOW)+UpdateWindow
        if self.visible.load(Ordering::Relaxed) {
            return;
        }
        self.visible.store(true, Ordering::Relaxed);
        // SAFETY: 同上，self.hwnd 存活；UpdateWindow 触发同步重绘（WM_PAINT）。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    /// 查询覆盖层窗口当前是否显示（幂等状态，非线程安全用途）
    ///
    /// 供兜底轮询判断：目标窗口恢复可见但覆盖层仍隐藏时，需要调用 [`show`] 恢复。
    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// 查询覆盖层是否仍处于运行状态（未被销毁）
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);

        // 销毁仍显示的悬停 tooltip，避免覆盖层销毁后 tooltip 孤儿化
        self.destroy_tooltip();

        // 从覆盖层注册表移除本覆盖层
        if let Some(map) = TARGET_MAP.get() {
            if let Ok(mut map) = map.lock() {
                map.remove(&(self.hwnd.0 as isize));
            }
        }

        // SAFETY: self.hwnd 由本 Overlay 独占持有，销毁后不再被任何代码访问。
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
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
            // SAFETY: hwnd 由系统在消息分发时传入且有效；WM_ERASEBKGND 的 wParam
            // 约定为窗口 DC（HDC）；GDI 对象创建后立即使用并删除，避免泄漏。
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
            // SAFETY: hwnd 由系统传入且有效；BeginPaint 仅可在 WM_PAINT 中调用，
            // 与 EndPaint 成对；画刷使用后立即删除。
            unsafe {
                let mut ps = Default::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let brush = CreateSolidBrush(tag_color_ref(hwnd));
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
            // SAFETY: hwnd 由系统传入且有效；GetWindowRect 为只读查询。
            unsafe {
                let _ = GetWindowRect(hwnd, &mut wr);
            }
            let cx = x - wr.left;
            let cy = y - wr.top;
            if (DOT_RECT.left..DOT_RECT.right).contains(&cx)
                && (DOT_RECT.top..DOT_RECT.bottom).contains(&cy)
            {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        WM_MOUSEMOVE => {
            handle_mouse_move(hwnd);
            // SAFETY: 消息参数由系统传入，转发给默认窗口过程处理。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        msg if msg == WM_MOUSELEAVE => {
            handle_mouse_leave(hwnd);
            LRESULT(0)
        }
        _ => {
            // SAFETY: 消息参数由系统传入，转发给默认窗口过程处理。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

// ============================================================
// 悬停便签工具提示
// ============================================================

fn ensure_tooltip_class() {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::Relaxed) {
        return;
    }
    let class_name = widestring("WinTagTooltip");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(tooltip_wndproc),
        hInstance: windows::Win32::Foundation::HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: RegisterClassW 失败仅返回 0，忽略；REGISTERED 保证本类只注册一次，
    // 类数据在程序生命周期内保持有效。
    unsafe {
        let _ = RegisterClassW(&wc);
    }
}

fn handle_mouse_move(overlay_hwnd: HWND) {
    // 臂定 TME_LEAVE（仅当本覆盖层尚未臂定）；mouse_leave 回调会复位该标记，
    // 从而保证第二次悬停可重新臂定。
    let should_arm = TARGET_MAP
        .get()
        .and_then(|map| map.lock().ok())
        .and_then(|map| {
            map.get(&(overlay_hwnd.0 as isize))
                .map(|s| !s.tracking.swap(true, Ordering::Relaxed))
        })
        .unwrap_or(false);

    if should_arm {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: overlay_hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: tme 为栈上有效结构体，hwndTrack 指向存活覆盖层窗口；
        // TrackMouseEvent 失败（返回 Err）仅代表本次未臂定，后续 MOUSEMOVE 会重试。
        let armed = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        if !armed {
            // 臂定失败：复位 tracking 标记，使下一次 MOUSEMOVE 可重新尝试臂定
            // （避免失败后 tracking 永久置位导致 TME_LEAVE 永远无法重新注册）
            if let Some(map) = TARGET_MAP.get() {
                if let Ok(map) = map.lock() {
                    if let Some(state) = map.get(&(overlay_hwnd.0 as isize)) {
                        state.tracking.store(false, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    // tooltip 已存在（userdata 非空）时不重复创建
    // SAFETY: overlay_hwnd 为存活覆盖层（由 WndProc 传入），读取 userdata 无副作用。
    let old = unsafe { get_userdata::<std::ffi::c_void>(overlay_hwnd) } as isize;
    if old != 0 {
        return;
    }

    let target_hwnd = get_target_hwnd(overlay_hwnd);
    if target_hwnd == 0 {
        return;
    }

    // 未注入标签存储时悬停静默（不显示 tooltip、不产生错误）
    // 从存储读取标签后立即释放锁（作用域限制），避免在后续窗口创建期间持有
    // TAG_STORE_INNER——防止 tooltip 创建过程触发需要该锁的代码时自死锁。
    let (title, note) = {
        let Some(store) = TAG_STORE_INNER.get() else {
            return;
        };
        let Some(store) = store.lock().ok() else {
            return;
        };
        let Some(tag) = store.get(&target_hwnd) else {
            return;
        };
        (tag.title.clone(), tag.note.clone())
    };

    ensure_tooltip_class();

    let ex_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0);
    let style = WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0);
    let text = if note.is_empty() {
        title.clone()
    } else {
        format!("{}  -  {}", title, note)
    };

    let mut pt = POINT::default();
    // SAFETY: GetCursorPos 无需前置条件，失败返回 0 且 pt 保持不变，忽略。
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }

    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: tooltip 类已注册（ensure_tooltip_class），wide 为 NUL 结尾的 UTF-16 文本；
    // 创建失败返回 Err，由 if let Ok 处理，不传播。
    let tooltip_hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(widestring("WinTagTooltip").as_ptr()),
            PCWSTR(wide.as_ptr()),
            style,
            pt.x - 10,
            pt.y + 16,
            300,
            100,
            None,
            None,
            None,
            None,
        )
    };

    if let Ok(tooltip_hwnd) = tooltip_hwnd {
        // SAFETY: 主线程消息循环内执行；tooltip 由本函数创建，其生命周期由
        // overlay_hwnd 的 userdata 管理（mouse_leave / Drop 时销毁并置空）。
        unsafe {
            set_userdata(overlay_hwnd, tooltip_hwnd.0);
        }

        // SAFETY: tooltip_hwnd 刚创建且存活；GetDC/ReleaseDC 成对调用；
        // DrawTextW 测量文本高度，SetWindowPos 调整尺寸，失败均忽略。
        unsafe {
            let hdc = GetDC(tooltip_hwnd);
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: 280,
                bottom: 0,
            };
            let _ = DrawTextW(
                hdc,
                &mut wide,
                &mut rc,
                DT_CENTER | DT_WORDBREAK | DT_VCENTER,
            );
            let _ = ReleaseDC(tooltip_hwnd, hdc);
            let height = (rc.bottom - rc.top).max(20) + 20;
            let _ = SetWindowPos(
                tooltip_hwnd,
                HWND_TOPMOST,
                pt.x - 10,
                pt.y + 16,
                300,
                height,
                SWP_NOACTIVATE,
            );
        }
    }
}

fn handle_mouse_leave(overlay_hwnd: HWND) {
    // 复位本覆盖层的 TME_LEAVE 跟踪标记，使下一次悬停可重新臂定
    if let Some(map) = TARGET_MAP.get() {
        if let Ok(map) = map.lock() {
            if let Some(state) = map.get(&(overlay_hwnd.0 as isize)) {
                state.tracking.store(false, Ordering::Relaxed);
            }
        }
    }

    // 销毁悬停 tooltip（若有）并清空 userdata
    // SAFETY: overlay_hwnd 为存活覆盖层（由 WndProc 传入），读取 userdata 无副作用。
    let tooltip_ptr = unsafe { get_userdata::<std::ffi::c_void>(overlay_hwnd) } as isize;
    if tooltip_ptr != 0 {
        // SAFETY: 主线程消息循环内执行；先置空 userdata 再销毁 tooltip，
        // 防止 tooltip WndProc 后续访问悬垂指针。
        unsafe {
            set_userdata(overlay_hwnd, std::ptr::null_mut());
        }
        // SAFETY: tooltip 由本覆盖层创建且经 userdata 校验存活，销毁后不再被访问。
        unsafe {
            let _ = DestroyWindow(HWND(tooltip_ptr as *mut std::ffi::c_void));
        }
    }
}

fn get_target_hwnd(overlay_hwnd: HWND) -> isize {
    TARGET_MAP
        .get()
        .and_then(|map| map.lock().ok())
        .and_then(|map| map.get(&(overlay_hwnd.0 as isize)).map(|s| s.target_hwnd))
        .unwrap_or(0)
}

/// 取覆盖层圆点绘制颜色（Win32 `COLORREF`，`0x00BBGGRR` 布局）
///
/// 从注入的标签存储中查找覆盖层对应目标窗口的标签颜色：
/// - 存储已注入且查到标签 → 取 `Tag::color.as_rgba()` 的 RGB 通道转换为 BGR；
/// - 存储未注入、目标窗口未知或标签缺失 → 回退橙色（[`FALLBACK_DOT_COLOR`]）。
fn tag_color_ref(overlay_hwnd: HWND) -> COLORREF {
    let target_hwnd = get_target_hwnd(overlay_hwnd);
    if target_hwnd == 0 {
        return FALLBACK_DOT_COLOR;
    }
    let Some(store) = TAG_STORE_INNER.get() else {
        return FALLBACK_DOT_COLOR;
    };
    let Ok(store) = store.lock() else {
        return FALLBACK_DOT_COLOR;
    };
    let Some(tag) = store.get(&target_hwnd) else {
        return FALLBACK_DOT_COLOR;
    };
    let [r, g, b, _] = tag.color.as_rgba();
    COLORREF(((r as u32) << 16) | ((g as u32) << 8) | b as u32)
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
            // SAFETY: hwnd 由系统传入且有效；BeginPaint/EndPaint 成对；GDI 对象及时删除；
            // buf 为栈上 512 元素缓冲，GetWindowTextW 返回长度有界，不会越界访问。
            unsafe {
                let mut ps = Default::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                // 取注入的 tooltip 配色：已注入则用注入色；未注入（或锁中毒）时
                // 回退默认白底黑字，行为与注入前完全一致。
                let (bg, fg) = TOOLTIP_THEME
                    .get()
                    .copied()
                    .unwrap_or((COLORREF(0x00FFFFFF), COLORREF(0x00000000)));
                let brush = CreateSolidBrush(bg);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let _ = FillRect(hdc, &rc, brush);
                let _ = DeleteObject(brush);

                let mut buf = [0u16; 512];
                let len = GetWindowTextW(hwnd, &mut buf) as usize;
                if len > 0 {
                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, fg);
                    let mut tr = RECT {
                        left: 10,
                        top: 10,
                        right: rc.right - 10,
                        bottom: rc.bottom - 10,
                    };
                    let _ = DrawTextW(hdc, &mut buf[..len], &mut tr, DT_WORDBREAK);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: 消息参数由系统传入，转发给默认窗口过程处理。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}
