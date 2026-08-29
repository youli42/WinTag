use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, GetDC, ReleaseDC, RoundRect, SelectObject, SetBkMode,
    SetTextColor, UpdateWindow, ValidateRect, BLENDFUNCTION, DT_WORDBREAK, HGDIOBJ, PS_SOLID,
    TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowRect,
    GetWindowTextW, IsIconic, IsWindowVisible, RegisterClassW, SetWindowPos, ShowWindow,
    UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOW, ULW_ALPHA, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND,
    WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use super::badge::{render_badge, BadgeParams};
use super::TagStore;
use crate::common::{get_userdata, set_userdata, widestring};

/// 角标逻辑像素边长（贴窗口左上角的等腰直角三角形腰长）
const BADGE_SIZE: i32 = 18;
/// 角标在覆盖层窗口中的偏移（距窗口左上角）
const BADGE_OFFSET: i32 = 2;
/// 覆盖层窗口边长 = 三角形腰长 + 左右/上下各留 [`BADGE_OFFSET`] 透明边
///
/// 覆盖层窗口只覆盖角标这一小块（贴目标窗口左上角），而非整块目标窗口，
/// 从而避免铺满窗口时吞掉目标窗口的点击（`HTTRANSPARENT` 无法穿透其他进程窗口）。
const BADGE_WIN_SIZE: i32 = BADGE_SIZE + BADGE_OFFSET * 2;
/// 圆点回退颜色（橙色，RGB [255, 183, 77]）
const FALLBACK_DOT_RGBA: [u8; 4] = [255, 183, 77, 255];
/// WM_MOUSELEAVE：TrackMouseEvent(TME_LEAVE) 触发的鼠标离开消息
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
/// 与注入前的行为完全一致。采用 `Mutex` 承载以支持主题切换后热更新
/// （决策记录 D11：修复主题切换后 tooltip 沿用启动配色的遗留问题），
/// 读取用 `lock().ok()` 取当前值。
static TOOLTIP_THEME: OnceLock<Mutex<(COLORREF, COLORREF)>> = OnceLock::new();

/// 注入 tooltip 主题配色
///
/// 首次调用初始化 Mutex 存储；此后每次调用覆盖为最新配色，使主题切换后
/// 新建的 tooltip 即时采用新配色。未调用本函数时 tooltip 保持默认白底黑字。
pub fn set_tooltip_theme(bg: COLORREF, fg: COLORREF) {
    let state = TOOLTIP_THEME.get_or_init(|| Mutex::new((bg, fg)));
    if let Ok(mut guard) = state.lock() {
        *guard = (bg, fg);
    }
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

        // 覆盖层窗口只占角标大小、贴在目标窗口左上角，而非铺满整个目标窗口。
        // 铺满目标窗口会吞掉其点击（`HTTRANSPARENT` 无法穿透其他进程窗口）。
        let side = BADGE_WIN_SIZE;

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
                side,
                side,
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

        // SAFETY: hwnd 为本函数刚创建且存活的窗口；ShowWindow/UpdateWindow 触发
        // 首次绘制（WM_PAINT 内经 UpdateLayeredWindow 提交预乘 RGBA）。
        unsafe {
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

    /// 将覆盖层位置同步到目标窗口当前可见区域的左上角（窗口移动/缩放时调用）
    ///
    /// 覆盖层窗口尺寸固定为角标大小（[`BADGE_WIN_SIZE`]），仅跟随目标窗口左上角位置；
    /// 目标窗口缩放而左上角不变时无需重排（角标始终锚定该角）。
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

        // 覆盖层窗口固定为角标大小（badge 窗口），仅跟随目标窗口左上角位置；
        // 尺寸不再随目标缩放而变化。
        let side = BADGE_WIN_SIZE;

        // 变更去重：与上次已应用的矩形一致时跳过 SetWindowPos；锁中毒时继续执行，
        // 保持与之前相同的定位行为（下次同步仍会重新去重）。
        let current = (rect.left, rect.top, side, side);
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
                side,
                side,
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

/// 点是否在角标三角形内部（命中测试，纯函数）
///
/// 三角形顶点（以左上角为原点）：A(0,0) B(size,0) C(0,size)。
/// 内部判定：x≥0 且 y≥0 且 x+y ≤ size（斜边方程 x+y=size）。
fn point_in_badge_triangle(cx: i32, cy: i32, size: i32) -> bool {
    cx >= 0 && cy >= 0 && cx + cy <= size
}

/// 取角标 RGBA 颜色（标签色 → RGBA；缺失回退橙）
fn tag_color_rgba(overlay_hwnd: HWND) -> [u8; 4] {
    let target_hwnd = get_target_hwnd(overlay_hwnd);
    if target_hwnd == 0 {
        return FALLBACK_DOT_RGBA;
    }
    let Some(store) = TAG_STORE_INNER.get() else {
        return FALLBACK_DOT_RGBA;
    };
    let Ok(store) = store.lock() else {
        return FALLBACK_DOT_RGBA;
    };
    let Some(tag) = store.get(&target_hwnd) else {
        return FALLBACK_DOT_RGBA;
    };
    tag.color.as_rgba()
}

/// 取角标描边色（依当前主题：暗色用浅描边、亮色用深描边）
fn badge_stroke_rgba() -> [u8; 4] {
    crate::ui::theme::theme_colors()
        .map(|c| {
            // 取主题 border 色 BGR → RGBA
            let r = (c.border.0 >> 16) & 0xFF;
            let g = (c.border.0 >> 8) & 0xFF;
            let b = c.border.0 & 0xFF;
            [r as u8, g as u8, b as u8, 255]
        })
        .unwrap_or([60, 60, 60, 255])
}

/// 渲染角标并经 `UpdateLayeredWindow` 提交到覆盖层窗口（逐像素 alpha 抗锯齿）
fn update_layered_badge(hwnd: HWND) {
    let side = BADGE_WIN_SIZE;
    let fill = tag_color_rgba(hwnd);
    let stroke = badge_stroke_rgba();
    // 先渲染纯三角形缓冲（腰长 BADGE_SIZE，贴左上角），再拷贝进全窗口缓冲，
    // 使三角形相对窗口左上角偏移 BADGE_OFFSET 像素、四周留透明边。
    let tri = render_badge(BadgeParams {
        size: BADGE_SIZE,
        fill,
        stroke,
    });
    let mut rgba = vec![0u8; (side * side) as usize * 4];
    for y in 0..BADGE_SIZE {
        let src = (y * BADGE_SIZE) as usize * 4;
        let dst = (((y + BADGE_OFFSET) * side + BADGE_OFFSET) as usize) * 4;
        let len = BADGE_SIZE as usize * 4;
        rgba[dst..dst + len].copy_from_slice(&tri[src..src + len]);
    }

    // SAFETY: CreateBitmap 创建 32bpp DIB，数据由 rgba 提供；返回 HBITMAP。
    // rgba 在调用期间存活，CreateBitmap 内部完成像素拷贝。
    let hbmp = unsafe {
        CreateBitmap(
            side,
            side,
            1,
            32,
            Some(rgba.as_ptr() as *const std::ffi::c_void),
        )
    };
    if hbmp.is_invalid() {
        return;
    }

    // SAFETY: CreateCompatibleDC 创建内存 DC；SelectObject 选入 HBITMAP；
    // 用完后删除（先 SelectObject 恢复原对象）。均在本函数内成对完成。
    let mem_dc = unsafe { CreateCompatibleDC(None) };
    if mem_dc.is_invalid() {
        // SAFETY: hbmp 刚创建，无效 DC 路径下仅删 bitmap。
        unsafe {
            let _ = DeleteObject(hbmp);
        }
        return;
    }
    let old_bmp = unsafe { SelectObject(mem_dc, hbmp) };

    // 目标位置/尺寸由 create/sync_position 设定，此处不传 pt_dst（保持窗口原位置），
    // 避免 UpdateLayeredWindow 将窗口重定位到 `pt_dst` 所指"屏幕坐标"从而挪走窗口。
    let sz = SIZE { cx: side, cy: side };
    let pt_src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 1, // AC_SRC_ALPHA（预乘）
    };

    // SAFETY: hwnd 为覆盖层窗口存活句柄；UpdateLayeredWindow 提交内存 DC 内容
    // 为窗口的分层像素，ULW_ALPHA 启用逐像素 alpha 混合；失败静默忽略
    // （下次 WM_PAINT 会重试）。mem_dc 为 CreateCompatibleDC 返回的 HDC。
    let _ = unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&sz),
            mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    };

    // 清理：恢复原对象 → 删 bitmap → 删内存 DC
    // SAFETY: 成对清理，无跨消息生命周期。
    unsafe {
        let _ = SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(mem_dc);
    }
}

extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            // UpdateLayeredWindow 模式下背景由 WM_PAINT 的 UpdateLayeredWindow 整面覆盖，
            // 无需 GDI 擦除；返回 1 阻止默认擦除。
            LRESULT(1)
        }
        WM_PAINT => {
            // 渲染贴角圆边三角形 → 32bpp 预乘 RGBA → UpdateLayeredWindow 提交
            // （决策记录 D11：替代原色键透明 FillRect 方块，获得逐像素 alpha 抗锯齿）
            update_layered_badge(hwnd);
            // 通知系统 WM_PAINT 已处理（分层窗口经 UpdateLayeredWindow 绘制，
            // 不走 BeginPaint/EndPaint 路径，但仍需返回 0 告知已处理）
            // SAFETY: ValidateRect 标记整个客户区为有效，阻止重复 WM_PAINT。
            unsafe {
                let _ = ValidateRect(hwnd, None);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            // 命中测试：仅角标三角形内部可点击（HTCLIENT），其余区域穿透（HTTRANSPARENT）
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut wr = RECT::default();
            // SAFETY: hwnd 由系统传入且有效；GetWindowRect 为只读查询。
            unsafe {
                let _ = GetWindowRect(hwnd, &mut wr);
            }
            let cx = x - wr.left - BADGE_OFFSET;
            let cy = y - wr.top - BADGE_OFFSET;
            if point_in_badge_triangle(cx, cy, BADGE_SIZE) {
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
    // 分层排版：标题与备注以 \n 分隔，tooltip_wndproc 分别绘制（标题加粗）
    let text = if note.is_empty() {
        title.clone()
    } else {
        format!("{}\n{}", title, note)
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

        // 宽度自适应 + 高度自适应：DrawTextW 预量，宽度上限 360px（解决问题 4 截断）
        // SAFETY: tooltip_hwnd 刚创建且存活；GetDC/ReleaseDC 成对调用；
        // DrawTextW 测量文本，SetWindowPos 调整尺寸，失败均忽略。
        unsafe {
            let hdc = GetDC(tooltip_hwnd);
            // 先按 340px 内容宽（360 减四边 10px 边距）测量高度
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: 340,
                bottom: 0,
            };
            let _ = DrawTextW(hdc, &mut wide, &mut rc, DT_WORDBREAK);
            let content_w = (rc.right - rc.left).max(120);
            let content_h = (rc.bottom - rc.top).max(20);
            let _ = ReleaseDC(tooltip_hwnd, hdc);
            // 实际窗口尺寸 = 内容宽高 + 四边 10px 内边距 + 1px 边框
            let win_w = content_w + 20 + 2;
            let win_h = content_h + 20 + 2;
            let _ = SetWindowPos(
                tooltip_hwnd,
                HWND_TOPMOST,
                pt.x - 10,
                pt.y + 16,
                win_w,
                win_h,
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
            // 圆角矩形 + 1px 边框 + 标题/备注分层（决策记录 D11，解决问题 4 + 观感）
            // SAFETY: hwnd 由系统传入且有效；BeginPaint/EndPaint 成对；GDI 对象及时删除。
            unsafe {
                let mut ps = Default::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                // 取注入的 tooltip 配色（Mutex 可热更新）：已注入用注入色，
                // 未注入/锁中毒回退默认白底黑字
                let (bg, fg) = TOOLTIP_THEME
                    .get()
                    .and_then(|m| m.lock().ok().map(|g| *g))
                    .unwrap_or((COLORREF(0x00FFFFFF), COLORREF(0x00000000)));
                // 描边色取主题 border（未注入时用中灰）
                let border = crate::ui::theme::theme_colors()
                    .map(|c| c.border)
                    .unwrap_or(COLORREF(0x00808080));

                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);

                // —— 圆角背景 + 1px 边框 ——
                let fill = CreateSolidBrush(bg);
                let pen = CreatePen(PS_SOLID, 1, border);
                let old_pen = SelectObject(hdc, pen);
                let old_brush = SelectObject(hdc, fill);
                let radius = 6;
                let _ = RoundRect(
                    hdc,
                    rc.left,
                    rc.top,
                    rc.right - 1,
                    rc.bottom - 1,
                    radius,
                    radius,
                );
                let _ = SelectObject(hdc, old_brush);
                let _ = SelectObject(hdc, old_pen);
                let _ = DeleteObject(fill);
                let _ = DeleteObject(pen);

                // —— 文字分层：标题（粗体）+ 备注（常规），以 \n 分隔 ——
                let mut buf = [0u16; 512];
                let len = GetWindowTextW(hwnd, &mut buf) as usize;
                if len > 0 {
                    let text = String::from_utf16_lossy(&buf[..len]);
                    let mut parts = text.splitn(2, '\n');
                    let title = parts.next().unwrap_or("");
                    let note = parts.next().unwrap_or("");

                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, fg);

                    let margin = 10;
                    let mut y = margin;
                    let content_right = rc.right - margin;

                    // 标题（粗体字体）
                    let bold = crate::ui::theme::message_font_bold();
                    let old_font = if !bold.is_invalid() {
                        SelectObject(hdc, bold)
                    } else {
                        HGDIOBJ(std::ptr::null_mut())
                    };
                    if !title.is_empty() {
                        let mut title_wide: Vec<u16> =
                            title.encode_utf16().chain(std::iter::once(0)).collect();
                        let mut tr = RECT {
                            left: margin,
                            top: y,
                            right: content_right,
                            bottom: rc.bottom - margin,
                        };
                        let _ = DrawTextW(hdc, &mut title_wide, &mut tr, DT_WORDBREAK);
                        y = tr.bottom + 4;
                    }
                    if !old_font.is_invalid() {
                        let _ = SelectObject(hdc, old_font);
                    }

                    // 备注（常规字体）
                    if !note.is_empty() {
                        let msg_font = crate::ui::theme::message_font();
                        let old_font2 = if !msg_font.is_invalid() {
                            SelectObject(hdc, msg_font)
                        } else {
                            HGDIOBJ(std::ptr::null_mut())
                        };
                        let mut note_wide: Vec<u16> =
                            note.encode_utf16().chain(std::iter::once(0)).collect();
                        let mut nr = RECT {
                            left: margin,
                            top: y,
                            right: content_right,
                            bottom: rc.bottom - margin,
                        };
                        let _ = DrawTextW(hdc, &mut note_wide, &mut nr, DT_WORDBREAK);
                        if !old_font2.is_invalid() {
                            let _ = SelectObject(hdc, old_font2);
                        }
                    }
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
