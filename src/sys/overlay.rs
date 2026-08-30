use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, FALSE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, GetDC, GetMonitorInfoW, GetTextExtentPoint32W,
    InvalidateRect, MonitorFromPoint, PatBlt, ReleaseDC, RoundRect, SelectObject, SetBkMode,
    SetTextColor, UpdateWindow, ValidateRect, BITMAPINFO, BITMAPINFOHEADER, BLACKNESS,
    BLENDFUNCTION, DIB_RGB_COLORS, DT_CALCRECT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, HDC,
    HFONT, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic,
    IsWindowVisible, PostMessageW, RegisterClassW, SetWindowPos, ShowWindow, UpdateLayeredWindow,
    CS_HREDRAW, CS_VREDRAW, GWL_EXSTYLE, GW_HWNDPREV, HTCLIENT, HTTRANSPARENT, HWND_NOTOPMOST,
    HWND_TOP, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOW, ULW_ALPHA,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCHITTEST,
    WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, WS_VISIBLE,
};

use super::badge::{render_badge, render_rounded_rect, truncate_title, BadgeParams};
use super::TagStore;
use crate::common::{get_userdata, set_userdata, widestring};

/// 角标逻辑像素边长（贴窗口左上角的等腰直角三角形腰长）
const BADGE_SIZE: i32 = 18;
/// 角标在覆盖层窗口中的偏移（距窗口左上角）
const BADGE_OFFSET: i32 = 2;
/// 覆盖层角标区边长 = 三角形腰长 + 左右/上下各留 [`BADGE_OFFSET`] 透明边
///
/// 覆盖层窗口只覆盖角标（及可选的标题条）这一小块、贴目标窗口左上角，
/// 而非铺满整块目标窗口，从而避免铺满窗口时吞掉目标窗口的点击
/// （`HTTRANSPARENT` 无法穿透其他进程窗口）。
const BADGE_WIN_SIZE: i32 = BADGE_SIZE + BADGE_OFFSET * 2;
/// 标题条最大显示字符数（超出以省略号截断，需求：默认显示 5 个字）
const TITLE_MAX_CHARS: usize = 5;
/// 标题条高度（覆盖层高度 [`BADGE_WIN_SIZE`] 内垂直居中）
const TITLE_H: i32 = 16;
/// 标题条纵向偏移
const TITLE_Y: i32 = (BADGE_WIN_SIZE - TITLE_H) / 2;
/// 标题条左右内边距
const TITLE_PAD_X: i32 = 7;
/// 圆点回退颜色（橙色，RGB [255, 183, 77]）
const FALLBACK_DOT_RGBA: [u8; 4] = [255, 183, 77, 255];
/// WM_MOUSELEAVE：TrackMouseEvent(TME_LEAVE) 触发的鼠标离开消息
const WM_MOUSELEAVE: u32 = 0x02A3;

/// 角标标题条显示开关（R6 设置项 `show_badge_title` 的 sys 层注入镜像）
///
/// 依赖方向约束（ui → core → sys）不允许 sys 层读取 `core::settings`，
/// 因此由主线程经 [`set_show_title`] 注入（启动时 + 设置保存广播后），
/// 镜像 [`set_tooltip_theme`] 的注入模式。未注入时默认显示。
static SHOW_TITLE: AtomicBool = AtomicBool::new(true);

/// 注入标题条显示开关（R6：设置页"角标显示标题"）
pub fn set_show_title(enabled: bool) {
    SHOW_TITLE.store(enabled, Ordering::Relaxed);
}

/// 角标始终置顶开关（R19 设置项 `badge_always_top` 的 sys 层注入镜像）
///
/// 开启（默认）：覆盖层带 `WS_EX_TOPMOST` 且每次同步重申 `HWND_TOPMOST`，
/// 浮在所有窗口之上（含被其他窗口盖住的目标窗口）。
/// 关闭：覆盖层跟随目标窗口 z 序——`sync_position` 改用
/// "插到目标窗口正上方一格"（insert-after）的方式重排，被其他窗口盖住时
/// 随目标一起被遮挡，不再悬浮在最上层。
/// 由主线程经 [`set_badge_always_top`] 注入（启动时 + 设置保存广播后），
/// 与 [`SHOW_TITLE`] 相同的依赖方向约束（ui → core → sys）。
static BADGE_ALWAYS_TOP: AtomicBool = AtomicBool::new(true);

/// 注入角标始终置顶开关（R19：设置页"角标始终置顶"）
pub fn set_badge_always_top(enabled: bool) {
    BADGE_ALWAYS_TOP.store(enabled, Ordering::Relaxed);
}

/// 返回角标是否始终置顶（供 tooltip 创建与同步逻辑共用）
fn badge_always_top() -> bool {
    BADGE_ALWAYS_TOP.load(Ordering::Relaxed)
}

/// 注入的隐藏窗口句柄（消息中转目标）
///
/// 依赖方向约束（ui → core → sys）不允许 sys 层反向调用 ui 层打开编辑弹窗，
/// 因此角标/标题条单击（R5）只经 [`set_message_target`] 注入的隐藏窗口发送
/// `WM_APP_EDIT_TAG`，由主线程隐藏窗口 WndProc 统一分发（镜像 [`set_tag_store`]
/// 的注入模式）。未注入时单击静默（不发送消息）。
static MESSAGE_TARGET: OnceLock<isize> = OnceLock::new();

/// 注入隐藏窗口句柄（`WM_APP_EDIT_TAG` 等请求消息的接收方）
///
/// 必须在任何覆盖层创建之前调用（程序启动时主线程注入一次）。
pub fn set_message_target(hidden_hwnd: isize) {
    let _ = MESSAGE_TARGET.set(hidden_hwnd);
}

/// 向隐藏窗口发送编辑标签请求（R5：角标/标题条单击打开编辑弹窗）
fn request_edit_tag(target_hwnd: isize) {
    let Some(&hidden) = MESSAGE_TARGET.get() else {
        return;
    };
    // SAFETY: hidden 为主线程注入的存活隐藏窗口句柄；PostMessageW 为线程安全
    // 标准 API，失败（窗口已销毁）静默忽略。
    unsafe {
        let _ = PostMessageW(
            HWND(hidden as *mut std::ffi::c_void),
            crate::common::WM_APP_EDIT_TAG,
            WPARAM(target_hwnd as usize),
            LPARAM(0),
        );
    }
}

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
/// 作为"已标记"指示圆点；鼠标悬停时弹出便签 tooltip。R6 起角标右侧可
/// 附带一个圆角标题条（[`SHOW_TITLE`] 开关控制，显示标签标题，超长省略号
/// 截断），悬停标题条同样显示完整标题与备注。
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
            // 类光标必须非 NULL：NULL 会让 DefWindowProc 的 WM_SETCURSOR 隐藏光标
            //（表现为悬停角标/标题条时鼠标"消失"）
            hCursor: crate::common::arrow_cursor(),
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

        // R19：置顶开关闭时不带 WS_EX_TOPMOST（后续 sync_position 按目标窗口
        // 重排 z 序；创建时开关可能刚被主线程注入，此处以当前值拼接即可）
        let topmost = if badge_always_top() {
            WS_EX_TOPMOST.0
        } else {
            0
        };
        let ex_style =
            WINDOW_EX_STYLE(WS_EX_LAYERED.0 | topmost | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0);

        // 覆盖层窗口只占角标区大小、贴在目标窗口左上角，而非铺满整个目标窗口。
        // 铺满目标窗口会吞掉其点击（`HTTRANSPARENT` 无法穿透其他进程窗口）；
        // 标题条（R6）在首次绘制时经 UpdateLayeredWindow 自适应加宽。
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

        // SAFETY: hwnd 为本函数刚创建且存活的窗口；ShowWindow 显示窗口后仅
        // InvalidateRect 标记重绘，首绘延迟到消息循环空闲时触发（WM_PAINT →
        // update_layered_badge）。刻意不在创建栈内 UpdateWindow 同步绘制：
        // 实测创建瞬间的同步 UpdateLayeredWindow 内容可能未生效（角标不显示，
        // 需事后重绘才出现），异步首绘与设置保存后 refresh() 的恢复路径一致。
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = InvalidateRect(hwnd, None, FALSE);
        }

        Ok(Overlay {
            hwnd,
            target_hwnd: target,
            running: AtomicBool::new(true),
            visible: AtomicBool::new(true),
        })
    }

    /// 将覆盖层位置同步到目标窗口当前可见区域的左上角（窗口移动/缩放时调用）
    ///
    /// 覆盖层只跟随目标窗口左上角位置；窗口尺寸由 [`update_layered_badge`]
    /// 绘制时自适应（角标区 + 可选标题条宽度），此处携带 [`SWP_NOSIZE`]
    /// 仅调整位置，避免用固定尺寸覆盖掉绘制结果。
    ///
    /// 优先使用 DWM 扩展帧边界（[`DWMWA_EXTENDED_FRAME_BOUNDS`]）获取目标窗口的
    /// **可见区域**：该边界不含隐形 resize 边框，与用户视觉感知一致；在 Per-Monitor
    /// v2 DPI 感知下与物理像素一致，不受系统 DPI 缩放调整。获取失败时回退到
    /// `GetWindowRect`（含隐形边框的窗口矩形）。
    ///
    /// 可见性守卫：目标窗口最小化（`IsIconic`）或不可见（`!IsWindowVisible`）时
    /// **自动隐藏覆盖层**——这是 500ms 兜底轮询与事件丢失场景下的可见性校正路径，
    /// 确保目标最小化/隐藏后覆盖层不会悬浮残留；目标恢复可见后由 [`show`] 恢复。
    /// 每次同步都执行 `SetWindowPos`（不按位置去重）并重排 z 序：
    /// 始终置顶模式重申 `HWND_TOPMOST`，跟随目标模式插到目标窗口正上方一格，
    /// 保证覆盖层 z 序永远压在目标窗口之上。
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

        // 覆盖层窗口仅跟随目标窗口左上角位置；尺寸由绘制自适应（SWP_NOSIZE）。
        // 每次同步都执行 SetWindowPos：若按位置去重早退，目标窗口被激活压住
        // 覆盖层（同属 topmost 带时后者在前）后 z 序永远无法恢复，角标就此
        // 被遮挡不再显示。事件合并（win_event.rs）+ 500ms 轮询已节制调用频率，
        // 单次 SetWindowPos 开销可忽略。
        // z 序（R19）：
        // - 始终置顶（默认）：insertAfter = HWND_TOPMOST，每次同步重申置顶；
        // - 跟随目标：与目标窗口同一 z 带、插到目标**正前方一格**。
        //
        // 注意 SetWindowPos 的 hWndInsertAfter 语义是「被移动窗口跟在它之后（背后）」，
        // 所以「插到目标正上方」必须把 hWndInsertAfter 设为目标的前邻窗口
        // （GW_HWNDPREV），而非目标本身——否则会把覆盖层排到目标背后、被目标自身挡住。
        // 目标前邻为 NULL（目标处于其 z 带顶端）时用 HWND_TOP（== NULL，置于带顶）。
        //
        // 带位一致性：创建时的 WS_EX_TOPMOST 仅按当时开关拼接，置顶开关来回切换不会
        // 改动已有覆盖层的样式；这里按「目标是否 topmost」对齐覆盖层带位，既保证
        // 「跟随目标」语义（目标被其他窗口盖住时角标随之被遮挡），也避免表头遗留的
        // topmost 样式让角标在取消置顶后仍悬浮最上层。
        let target_topmost = unsafe {
            (GetWindowLongPtrW(self.target_hwnd, GWL_EXSTYLE) as isize & WS_EX_TOPMOST.0 as isize)
                != 0
        };
        let want_topmost = badge_always_top() || target_topmost;
        let overlay_topmost = unsafe {
            (GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as isize & WS_EX_TOPMOST.0 as isize) != 0
        };
        if want_topmost != overlay_topmost {
            // 切换 z 带（不改位置/尺寸）：HWND_TOPMOST / HWND_NOTOPMOST 自动置/清
            // WS_EX_TOPMOST 并在带内排序。失败忽略，下次同步会重试。
            // SAFETY: self.hwnd 由本 Overlay 独占持有且存活；SWP_NOMOVE|SWP_NOSIZE 仅动 z 序。
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    if want_topmost {
                        HWND_TOPMOST
                    } else {
                        HWND_NOTOPMOST
                    },
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
        let insert_after = if badge_always_top() {
            HWND_TOPMOST
        } else {
            // SAFETY: self.target_hwnd 由本 Overlay 持有且窗口存活；GetWindow 为只读
            // z 序查询，失败（Err/返回 NULL）时用 HWND_TOP 兜底（置于其带顶）。
            unsafe { GetWindow(self.target_hwnd, GW_HWNDPREV) }
                .ok()
                .filter(|h| !h.0.is_null())
                .unwrap_or(HWND_TOP)
        };
        // SAFETY: self.hwnd 与 self.target_hwnd 由本 Overlay 持有且窗口存活；
        // SetWindowPos 仅调整位置并重排 z 序（SWP_NOSIZE 保留绘制自适应尺寸），
        // 失败忽略（下次同步会重试）。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                insert_after,
                rect.left,
                rect.top,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE,
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

    /// 强制覆盖层立即重绘（标签内容/主题配色/显示开关变化后调用）
    ///
    /// 触发一次同步 `WM_PAINT`：`update_layered_badge` 会按当前 TagStore、
    /// 主题调色板与 `show_badge_title` 开关重新渲染并自适应窗口尺寸。
    pub fn refresh(&self) {
        // SAFETY: self.hwnd 由本 Overlay 独占持有，Drop 前始终存活；
        // InvalidateRect 标记重绘，UpdateWindow 同步触发 WM_PAINT。
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, FALSE);
            let _ = UpdateWindow(self.hwnd);
        }
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

/// `COLORREF`（0x00BBGGRR）→ RGB 字节序（描边等不透明色转换）
fn colorref_to_rgb(c: COLORREF) -> [u8; 3] {
    [
        (c.0 & 0xFF) as u8,
        ((c.0 >> 8) & 0xFF) as u8,
        ((c.0 >> 16) & 0xFF) as u8,
    ]
}

/// 读取标题条显示文本（R6：角标旁显示标题，超长截断为省略号）
///
/// - 开关关闭（[`set_show_title`]）或未注入标签存储 → `None`（只画角标）；
/// - 标签标题为空时回退窗口原始标题（`tag.window_title`）；
/// - 均为空 → `None`。
fn title_text(target_hwnd: isize) -> Option<String> {
    if !SHOW_TITLE.load(Ordering::Relaxed) {
        return None;
    }
    let store = TAG_STORE_INNER.get()?;
    let Ok(store) = store.lock() else {
        return None;
    };
    let tag = store.get(&target_hwnd)?;
    let raw = if tag.title.is_empty() {
        tag.window_title.as_str()
    } else {
        tag.title.as_str()
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_title(trimmed, TITLE_MAX_CHARS))
}

/// 在指定 DC 上量测文本的绘制尺寸（`DT_CALCRECT`，只量测不实际绘制）
///
/// 返回 (宽, 高)。量测宽度上限 340px（与 tooltip 创建时的换行宽度一致）。
/// 标题（粗体）与备注（常规）行高不同，须用各自字体分别量测后累加——
/// 此前用窗口默认字体对全文整体量测，行高偏小导致 tooltip 高度不足、
/// 备注行被裁掉不可见。
fn measure_text_size(hdc: HDC, text: &str, font: HFONT) -> (i32, i32) {
    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: 340,
        bottom: 0,
    };
    let old = if !font.is_invalid() {
        // SAFETY: 切换量测字体，结束后恢复原对象。
        unsafe { SelectObject(hdc, font) }
    } else {
        HGDIOBJ::default()
    };
    // SAFETY: wide 为 NUL 结尾宽字符串切片，rc 为栈上矩形，调用期间存活；
    // DT_CALCRECT 仅量测不绘制，DC 无副作用。
    unsafe {
        let _ = DrawTextW(hdc, &mut wide, &mut rc, DT_WORDBREAK | DT_CALCRECT);
    }
    if !old.is_invalid() {
        // SAFETY: 恢复 SelectObject 保存的原对象。
        unsafe {
            let _ = SelectObject(hdc, old);
        }
    }
    (rc.right - rc.left, rc.bottom - rc.top)
}

/// 量测标题文本像素宽度（系统消息字体；量测失败回退每字符 12px 估算）
fn measure_title_width(text: &str) -> i32 {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let count = (wide.len() as i32 - 1).max(0);
    if count == 0 {
        return 0;
    }
    // SAFETY: GetDC(None) 取屏幕 DC 仅作字体量测，用后 ReleaseDC 归还。
    let hdc = unsafe { GetDC(None) };
    let mut width = 0i32;
    if !hdc.is_invalid() {
        let font = crate::ui::theme::message_font();
        let old = if !font.is_invalid() {
            // SAFETY: SelectObject 切换量测字体，结束后恢复原对象。
            unsafe { SelectObject(hdc, font) }
        } else {
            HGDIOBJ::default()
        };
        let mut size = SIZE::default();
        // SAFETY: wide 为 NUL 结尾宽字符串切片，size 为栈上缓冲，调用期间存活。
        let ok = unsafe { GetTextExtentPoint32W(hdc, &wide[..count as usize], &mut size) };
        if ok.as_bool() {
            width = size.cx;
        }
        if !font.is_invalid() {
            // SAFETY: 恢复 SelectObject 保存的原对象。
            unsafe {
                let _ = SelectObject(hdc, old);
            }
        }
        // SAFETY: 与 GetDC(None) 成对归还屏幕 DC。
        unsafe {
            let _ = ReleaseDC(None, hdc);
        }
    }
    if width <= 0 {
        // 回退估算：每字符约 12px（中文略偏小，仅量测彻底失败时兜底）
        width = count * 12;
    }
    width
}

/// 将标题文字按蒙版方式合成进预乘 RGBA 缓冲（GDI 黑底白字 → 亮度作 alpha）
///
/// `rgba` 为整窗预乘缓冲，`win` 为整窗 (宽, 高)；文字绘制在 `rect`
/// = (x0, y0, 宽, 高) 的标题条内部。以 `CreateDIBSection` 建立
/// 32bpp 顶朝下 DIB，GDI 以白字写黑底后读回：R 通道亮度即文字覆盖率，
/// 以 `fg` 色经 alpha-over 合成到标题条底色上。
fn overlay_text_into(
    rgba: &mut [u8],
    win: (i32, i32),
    rect: (i32, i32, i32, i32),
    text: &str,
    fg: [u8; 3],
) {
    let (win_w, win_h) = win;
    let (x0, y0, w, h) = rect;
    if w <= 0 || h <= 0 {
        return;
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // 负高度 = 顶朝下（行序与 rgba 缓冲一致）
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: bmi 为栈上有效结构；bits 出参由系统写入；DIB 尺寸小（≤ 数十字节宽）。
    let hbmp = unsafe { CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) };
    let hbmp = match hbmp {
        Ok(h) if !bits.is_null() => h,
        Ok(h) => {
            // SAFETY: DIB 创建成功但像素指针为空（异常路径），删除后退出。
            unsafe {
                let _ = DeleteObject(h);
            }
            return;
        }
        Err(_) => return,
    };
    // SAFETY: CreateCompatibleDC 创建内存 DC，SelectObject 选入 DIB，
    // 用毕成对恢复/删除（与本函数内其余 GDI 调用同作用域完成）。
    let mem_dc = unsafe { CreateCompatibleDC(None) };
    if mem_dc.is_invalid() {
        // SAFETY: hbmp 刚创建，无效 DC 路径下仅删 bitmap。
        unsafe {
            let _ = DeleteObject(hbmp);
        }
        return;
    }
    let old_bmp = unsafe { SelectObject(mem_dc, hbmp) };

    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: mem_dc 存活；PatBlt 将 DIB 整面涂黑作文字蒙版底色。
    unsafe {
        let _ = PatBlt(mem_dc, 0, 0, w, h, BLACKNESS);
        let _ = SetBkMode(mem_dc, TRANSPARENT);
        // 白字：R 通道亮度即文字覆盖率
        let _ = SetTextColor(mem_dc, COLORREF(0x00FFFFFF));
    }
    let font = crate::ui::theme::message_font();
    let old_font = if !font.is_invalid() {
        // SAFETY: 字体为进程级缓存句柄（永不删除），选入后恢复。
        unsafe { SelectObject(mem_dc, font) }
    } else {
        HGDIOBJ::default()
    };
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    // SAFETY: wide 为 NUL 结尾缓冲，rc 为栈上矩形，均在调用期间存活。
    unsafe {
        let _ = DrawTextW(mem_dc, &mut wide, &mut rc, DT_SINGLELINE | DT_VCENTER);
    }
    if !old_font.is_invalid() {
        // SAFETY: 恢复原字体对象。
        unsafe {
            let _ = SelectObject(mem_dc, old_font);
        }
    }

    // 读回蒙版像素（32bpp 内存序 B,G,R,unused），合成到整窗缓冲
    // SAFETY: bits 指向 hbmp 的像素内存（w*h*4 字节），DIB 与 mem_dc 存活期间有效。
    let mask = unsafe { std::slice::from_raw_parts(bits as *const u8, (w * h) as usize * 4) };
    for y in 0..h {
        let wy = y0 + y;
        if wy < 0 || wy >= win_h {
            continue;
        }
        for x in 0..w {
            let wx = x0 + x;
            if wx < 0 || wx >= win_w {
                continue;
            }
            let cov = mask[((y * w + x) * 4 + 2) as usize] as f32 / 255.0;
            if cov <= 0.0 {
                continue;
            }
            let di = ((wy * win_w + wx) * 4) as usize;
            let da = rgba[di + 3] as f32 / 255.0;
            // premultiplied alpha-over：out = fg*cov + dst*(1-cov)
            let inv = 1.0 - cov;
            // 缓冲字节序为 BGRA（同 render_badge）：fg 为 [R,G,B]，故 di 写蓝通道
            rgba[di] = (fg[2] as f32 * cov + rgba[di] as f32 * inv).round() as u8;
            rgba[di + 1] = (fg[1] as f32 * cov + rgba[di + 1] as f32 * inv).round() as u8;
            rgba[di + 2] = (fg[0] as f32 * cov + rgba[di + 2] as f32 * inv).round() as u8;
            rgba[di + 3] = (255.0 * cov + 255.0 * da * inv).round() as u8;
        }
    }

    // 清理：恢复原对象 → 删 DIB → 删内存 DC
    // SAFETY: 成对清理，无跨消息生命周期。
    unsafe {
        let _ = SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(mem_dc);
    }
}

/// 渲染角标 + 标题条并经 `UpdateLayeredWindow` 提交到覆盖层窗口（逐像素 alpha 抗锯齿）
///
/// - 角标：贴左上角的圆边三角形（[`render_badge`]），色取标签色；
/// - 标题条（R6）：角标右侧圆角胶囊（[`render_rounded_rect`]），底色/文字色
///   取注入的 tooltip 主题配色（主题热更新即时生效），文字为标签标题
///   （超长省略号截断）。无标签或开关关闭时只画角标，窗口退回角标区大小。
/// - 窗口尺寸随内容自适应：`UpdateLayeredWindow` 以 `psize` 同步改窗口大小。
fn update_layered_badge(hwnd: HWND) {
    let h = BADGE_WIN_SIZE;
    let target_hwnd = get_target_hwnd(hwnd);
    let fill = tag_color_rgba(hwnd);
    let stroke = badge_stroke_rgba();

    // 标题条几何与配色（tooltip 主题配色 + border 描边，与悬停 tooltip 一致）
    let title = if target_hwnd != 0 {
        title_text(target_hwnd)
    } else {
        None
    };
    let (pill_fill, pill_stroke, text_fg) = {
        let theme = TOOLTIP_THEME
            .get()
            .and_then(|m| m.lock().ok().map(|g| *g))
            .unwrap_or((COLORREF(0x00FFFFFF), COLORREF(0x00000000)));
        let border = crate::ui::theme::theme_colors()
            .map(|c| c.border)
            .unwrap_or(COLORREF(0x00808080));
        (
            colorref_to_rgb(theme.0),
            colorref_to_rgb(border),
            colorref_to_rgb(theme.1),
        )
    };

    let (win_w, pill_x, pill_w, text) = match &title {
        Some(t) => {
            let pw = measure_title_width(t) + TITLE_PAD_X * 2;
            (BADGE_WIN_SIZE + pw, BADGE_WIN_SIZE, pw, t.clone())
        }
        None => (BADGE_WIN_SIZE, 0, 0, String::new()),
    };

    let mut rgba = vec![0u8; (win_w * h) as usize * 4];

    // 1. 角标三角形（贴左上角，四周留 BADGE_OFFSET 透明边）
    let tri = render_badge(BadgeParams {
        size: BADGE_SIZE,
        fill,
        stroke,
    });
    for y in 0..BADGE_SIZE {
        let src = (y * BADGE_SIZE) as usize * 4;
        let dst = (((y + BADGE_OFFSET) * win_w + BADGE_OFFSET) as usize) * 4;
        let len = BADGE_SIZE as usize * 4;
        rgba[dst..dst + len].copy_from_slice(&tri[src..src + len]);
    }

    // 2. 标题条圆角胶囊（贴角标区右侧）+ 3. 文字蒙版合成
    if pill_w > 0 {
        let pill = render_rounded_rect(
            pill_w,
            TITLE_H,
            TITLE_H / 2,
            [pill_fill[0], pill_fill[1], pill_fill[2], 255],
            [pill_stroke[0], pill_stroke[1], pill_stroke[2], 255],
        );
        for y in 0..TITLE_H {
            let src = (y * pill_w) as usize * 4;
            let dst = ((TITLE_Y + y) * win_w + pill_x) as usize * 4;
            let len = pill_w as usize * 4;
            rgba[dst..dst + len].copy_from_slice(&pill[src..src + len]);
        }
        overlay_text_into(
            &mut rgba,
            (win_w, h),
            (
                pill_x + TITLE_PAD_X,
                TITLE_Y,
                pill_w - TITLE_PAD_X * 2,
                TITLE_H,
            ),
            &text,
            text_fg,
        );
    }

    // SAFETY: CreateBitmap 创建 32bpp DIB，数据由 rgba 提供；返回 HBITMAP。
    // rgba 在调用期间存活，CreateBitmap 内部完成像素拷贝。
    let hbmp = unsafe {
        CreateBitmap(
            win_w,
            h,
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

    // 目标位置由 create/sync_position 设定，此处不传 pt_dst（保持窗口原位置），
    // 避免 UpdateLayeredWindow 将窗口重定位到 `pt_dst` 所指"屏幕坐标"从而挪走窗口；
    // psize 携带内容自适应尺寸（角标区 + 可选标题条宽），分层窗口尺寸随之更新。
    let sz = SIZE { cx: win_w, cy: h };
    let pt_src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 1, // AC_SRC_ALPHA（预乘）
    };

    // SAFETY: hwnd 为覆盖层窗口存活句柄；UpdateLayeredWindow 提交内存 DC 内容
    // 为窗口的分层像素，ULW_ALPHA 启用逐像素 alpha 混合。mem_dc 为
    // CreateCompatibleDC 返回的 HDC。
    if let Err(e) = unsafe {
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
    } {
        // 失败不再静默：记录错误，便于定位“角标不显示”类问题
        //（失败时下次 WM_PAINT 会重试）。
        eprintln!("[覆盖层] UpdateLayeredWindow 失败: {e}，角标内容未提交");
    }

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
            // 命中测试：角标三角形内部与标题条区域可交互（HTCLIENT），
            // 其余区域穿透（HTTRANSPARENT）。窗口宽 = 角标区 + 可选标题条，
            // 标题条区域即 [BADGE_WIN_SIZE, win_w) × [TITLE_Y, TITLE_Y+TITLE_H)。
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut wr = RECT::default();
            // SAFETY: hwnd 由系统传入且有效；GetWindowRect 为只读查询。
            unsafe {
                let _ = GetWindowRect(hwnd, &mut wr);
            }
            let win_w = wr.right - wr.left;
            let cx = x - wr.left;
            let cy = y - wr.top;
            let in_badge =
                point_in_badge_triangle(cx - BADGE_OFFSET, cy - BADGE_OFFSET, BADGE_SIZE);
            let in_title = win_w > BADGE_WIN_SIZE
                && cx >= BADGE_WIN_SIZE
                && cx < win_w
                && (TITLE_Y..TITLE_Y + TITLE_H).contains(&cy);
            if in_badge || in_title {
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
        WM_LBUTTONDOWN => {
            // 角标/标题条单击（R5）：请求主线程为对应目标窗口打开编辑弹窗。
            // 命中区域已由 WM_NCHITTEST 过滤（仅角标三角形与标题条返回 HTCLIENT），
            // 能收到本消息即命中可交互区。
            let target_hwnd = get_target_hwnd(hwnd);
            if target_hwnd != 0 {
                request_edit_tag(target_hwnd);
            }
            LRESULT(0)
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
        // 类光标必须非 NULL：NULL 会让 DefWindowProc 的 WM_SETCURSOR 隐藏光标
        hCursor: crate::common::arrow_cursor(),
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

    // R19：tooltip 的 z 序跟随角标开关——置顶模式保持 TOPMOST（悬停信息
    // 永远可见）；跟随目标模式不带 TOPMOST，插到角标窗口正上方一格，
    // 保证至少盖住角标本体，同时随目标窗口一起被其他窗口遮挡。
    let topmost_ex = if badge_always_top() {
        WS_EX_TOPMOST.0
    } else {
        0
    };
    let ex_style = WINDOW_EX_STYLE(topmost_ex | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0);
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

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
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

        // 宽度自适应 + 高度自适应：与 tooltip_wndproc(WM_PAINT) 的排版一致，
        // 标题（粗体）与备注（常规）用各自字体分别 DT_CALCRECT 量测后累加，
        // 宽度上限 340px 内容宽（360 减四边 10px 边距）。此前用窗口默认字体
        // 对全文整体量测（且未加 DT_CALCRECT，实际在屏幕上画了一次），
        // 行高偏小导致窗口高度不足，备注行被裁掉不可见。
        // SAFETY: tooltip_hwnd 刚创建且存活；GetDC/ReleaseDC 成对调用；
        // SetWindowPos 调整尺寸，失败均忽略。
        unsafe {
            let hdc = GetDC(tooltip_hwnd);
            let (title_w, title_h) = if title.is_empty() {
                (0, 0)
            } else {
                measure_text_size(hdc, &title, crate::ui::theme::message_font_bold())
            };
            let (note_w, note_h) = if note.is_empty() {
                (0, 0)
            } else {
                measure_text_size(hdc, &note, crate::ui::theme::message_font())
            };
            let _ = ReleaseDC(tooltip_hwnd, hdc);
            let content_w = title_w.max(note_w).max(120);
            // 标题与备注之间的 4px 间距与 WM_PAINT 的 y = title_bottom + 4 对齐
            let line_gap = if !title.is_empty() && !note.is_empty() {
                4
            } else {
                0
            };
            let content_h = (title_h + line_gap + note_h).max(20);
            // 实际窗口尺寸 = 内容宽高 + 四边 10px 内边距 + 1px 边框
            let win_w = content_w + 20 + 2;
            let win_h = content_h + 20 + 2;
            // 定位钳制（D17 修复悬停备注显示不完整）：默认光标右下 (-10,+16)；
            // 超出光标所在显示器工作区底部时翻到光标上方，仍放不下贴顶边；
            // 横向越界收回右边缘。
            let (mut tx, mut ty) = (pt.x - 10, pt.y + 16);
            // SAFETY: MonitorFromPoint 只读查询，失败（NULL）时跳过钳制。
            let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            if !hmon.is_invalid() {
                let mut mi = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                // SAFETY: mi 为栈上缓冲且 cbSize 已填，GetMonitorInfoW 只读填充。
                if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                    let work = mi.rcWork;
                    if ty + win_h > work.bottom {
                        ty = pt.y - win_h - 8;
                    }
                    if ty < work.top {
                        ty = work.top;
                    }
                    if tx + win_w > work.right {
                        tx = work.right - win_w;
                    }
                    if tx < work.left {
                        tx = work.left;
                    }
                }
            }
            // z 序与创建时 ex_style 一致（R19）：置顶 → TOPMOST；跟随目标 →
            // 工具提示应盖住角标本体（而非被它挡在背后），故「插到角标窗口
            // 正上方」须以角标前邻窗口（GW_HWNDPREV）为插入点；并把 tooltip 的
            // z 带对齐到角标窗口（跟随模式下角标可能随顶部目标位于 topmost 带，
            // 非 topmost 的 tooltip 进不了同带）。与 sync_position 的带位对齐逻辑一致。
            let overlay_topmost = {
                (GetWindowLongPtrW(overlay_hwnd, GWL_EXSTYLE) as isize & WS_EX_TOPMOST.0 as isize)
                    != 0
            };
            let want_topmost = badge_always_top() || overlay_topmost;
            let tooltip_topmost = {
                (GetWindowLongPtrW(tooltip_hwnd, GWL_EXSTYLE) as isize & WS_EX_TOPMOST.0 as isize)
                    != 0
            };
            if want_topmost != tooltip_topmost {
                // 切换 tooltip z 带（仅 z 序，不动位置/尺寸）。
                // tooltip_hwnd 为刚创建且存活的窗口；SWP_NOMOVE|SWP_NOSIZE
                // 仅调整带位，失败忽略（悬停为临时窗口，下次悬停会重建）。
                let _ = SetWindowPos(
                    tooltip_hwnd,
                    if want_topmost {
                        HWND_TOPMOST
                    } else {
                        HWND_NOTOPMOST
                    },
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            let insert_after = if badge_always_top() {
                HWND_TOPMOST
            } else {
                GetWindow(overlay_hwnd, GW_HWNDPREV)
                    .ok()
                    .filter(|h| !h.0.is_null())
                    .unwrap_or(HWND_TOP)
            };
            let _ = SetWindowPos(
                tooltip_hwnd,
                insert_after,
                tx,
                ty,
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
                // 动态长度读取（定长 512 缓冲会静默丢弃超长备注）
                let text_len = GetWindowTextLengthW(hwnd) as usize;
                if text_len > 0 {
                    let mut buf = vec![0u16; text_len + 1];
                    let len = GetWindowTextW(hwnd, &mut buf) as usize;
                    let text = String::from_utf16_lossy(&buf[..len]);
                    let mut parts = text.splitn(2, '\n');
                    // 标题尾部可能残留 EDIT 换行符的 CR，剥掉避免渲染成方框
                    let title = parts.next().unwrap_or("").trim_end_matches('\r');
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
                        // DrawTextW 返回值为文本实际绘制高度（不带 DT_CALCRECT 时
                        // 不会回写 tr.bottom，不能用它推下一行位置）
                        let title_h = DrawTextW(hdc, &mut title_wide, &mut tr, DT_WORDBREAK);
                        y += title_h + 4;
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
