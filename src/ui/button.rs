//! 自绘现代按钮（决策记录 D11，解决问题 9.4 / 9.8）
//!
//! `WM_CTLCOLORBTN` 无法改变标准按钮的灰色面子（Win32 已知限制），故改用
//! `BS_OWNERDRAW` 自绘：扁平圆角矩形（`RoundRect`），两档样式——
//!
//! - [`ButtonStyle::Accent`]：accent 底 + 白字（"确认/保存"等默认动作）；
//! - [`ButtonStyle::Secondary`]：表面色底 + 1px 边框（"取消"等次要动作）。
//!
//! 交互态：悬停提亮、按压压暗（经 [`crate::ui::theme::blend`] 派生），
//! 键盘焦点保留系统虚线焦点框（`DrawFocusRect`），禁用态转次要灰。
//!
//! 用法：各窗口 WM_CREATE 经 [`create_button`] 创建按钮（内部注册状态并
//! 子类化跟踪鼠标），父窗口过程增加 `WM_DRAWITEM => button::handle_draw_item(...)`
//! 分支即可；点击仍走原 `WM_COMMAND`/`BN_CLICKED` 路由，无需改动。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, DrawFocusRect, DrawTextW, GetStockObject,
    InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor, BACKGROUND_MODE, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, HGDIOBJ, NULL_PEN, PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODT_BUTTON};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetClassLongPtrW, GetParent, GetWindowTextW, PostMessageW, SetWindowLongPtrW,
    BS_OWNERDRAW, GCLP_WNDPROC, GWLP_WNDPROC, HMENU, WINDOW_EX_STYLE, WINDOW_STYLE, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_SETFOCUS, WS_CHILD,
    WS_TABSTOP, WS_VISIBLE,
};

use crate::ui::theme::{blend, message_font, theme_colors};

/// `WM_MOUSELEAVE`：TrackMouseEvent(TME_LEAVE) 触发的鼠标离开消息
const WM_MOUSELEAVE: u32 = 0x02A3;

/// 按钮样式档位
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// 默认动作按钮：accent 底 + 白字（如"确认/保存"）
    Accent,
    /// 次要动作按钮：表面色底 + 1px 边框（如"取消"）
    Secondary,
}

/// 单个按钮的运行时状态（仅主线程消息循环内读写）
struct ButtonState {
    style: ButtonStyle,
    /// 鼠标是否悬停（用于提亮）
    hot: bool,
    /// 左键是否按下（用于压暗）
    pressed: bool,
}

/// 按钮状态注册表：按钮 HWND → 状态
///
/// `create_button` 注册、按钮 `WM_NCDESTROY` 注销；仅主线程访问，
/// 包 `Mutex` 仅为满足 static 语义（与 BRUSH_CACHE 同一模式）。
static BUTTON_STATES: OnceLock<Mutex<HashMap<usize, ButtonState>>> = OnceLock::new();

fn with_states<R>(f: impl FnOnce(&mut HashMap<usize, ButtonState>) -> R) -> Option<R> {
    let map = BUTTON_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    match map.lock() {
        Ok(mut guard) => Some(f(&mut guard)),
        Err(_) => None, // 锁中毒：放弃状态更新，按钮退化为静态外观
    }
}

/// 修改按钮状态并按需重绘（状态变化时返回 true）
fn update_state(hwnd: HWND, f: impl FnOnce(&mut ButtonState)) -> bool {
    let key = hwnd.0 as usize;
    with_states(|map| {
        if let Some(state) = map.get_mut(&key) {
            let before = (state.hot, state.pressed);
            f(state);
            let after = (state.hot, state.pressed);
            if before != after {
                // SAFETY: hwnd 为存活按钮窗口；InvalidateRect 仅标记重绘，无副作用。
                unsafe {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                true
            } else {
                false
            }
        } else {
            false
        }
    })
    .unwrap_or(false)
}

/// 创建自绘按钮并注册状态
///
/// 按钮使用 `BS_OWNERDRAW`，由父窗口的 `WM_DRAWITEM` 分支调用
/// [`handle_draw_item`] 绘制；点击仍以 `WM_COMMAND`（`BN_CLICKED`）上报，
/// 控件 ID 经 `id` 指定。创建成功返回窗口句柄，失败返回 Err。
///
/// # 参数
///
/// - `parent`：父窗口（接收 WM_DRAWITEM / WM_COMMAND）
/// - `id`：控件 ID
/// - `text`：按钮文字
/// - `x/y/w/h`：位置与尺寸（调用方负责 DPI 缩放）
/// - `style`：[`ButtonStyle`] 样式档位
#[allow(clippy::too_many_arguments)]
pub fn create_button(
    parent: HWND,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    style: ButtonStyle,
) -> windows::core::Result<HWND> {
    let style_ws = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32);
    let wide = crate::common::widestring(text);
    // SAFETY: 按标准子控件创建；wide 为 NUL 结尾宽字符串且存活于调用期间；
    // 失败返回 Err 由调用方处理。
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("BUTTON"),
            PCWSTR(wide.as_ptr()),
            style_ws,
            x,
            y,
            w,
            h,
            parent,
            HMENU(id as *mut c_void),
            windows::Win32::Foundation::HINSTANCE::default(),
            None,
        )
    }?;

    // 注册状态并子类化跟踪鼠标
    with_states(|map| {
        map.insert(
            hwnd.0 as usize,
            ButtonState {
                style,
                hot: false,
                pressed: false,
            },
        );
    });
    // SAFETY: hwnd 为刚创建成功的有效子控件；子类化仅替换实例窗口过程
    // （透传统一走类过程 GCLP_WNDPROC），无跨线程访问，失败静默忽略。
    unsafe {
        let _ = SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            button_subclass_proc as *const () as isize,
        );
    }
    Ok(hwnd)
}

/// 处理父窗口的 `WM_DRAWITEM`：绘制自绘按钮
///
/// 返回 `true` 表示本次消息属于本模块的按钮且已绘制（父窗口返回
/// `LRESULT(1)`），`false` 表示与按钮无关（父窗口走 `DefWindowProcW`）。
pub fn handle_draw_item(lparam: LPARAM) -> bool {
    // SAFETY: WM_DRAWITEM 的 lParam 指向 DRAWITEMSTRUCT，生命周期覆盖消息
    // 处理过程；先校验 CtlType 为按钮再使用。
    let dis = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
    if dis.CtlType != ODT_BUTTON {
        return false;
    }
    draw_button(dis);
    true
}

/// 绘制单个按钮（圆角矩形 + 居中文字 + 焦点框）
fn draw_button(dis: &DRAWITEMSTRUCT) {
    let colors = theme_colors().unwrap_or_else(crate::ui::theme::light_colors);
    let state = with_states(|map| {
        map.get(&(dis.hwndItem.0 as usize))
            .map(|s| (s.style, s.hot, s.pressed))
    })
    .flatten();
    let Some((style, hot, pressed)) = state else {
        return; // 未注册（异常路径）：交给系统默认绘制
    };

    // SAFETY: DRAWITEMSTRUCT.hDC 由系统在绘制期间提供，仅本函数内使用。
    let hdc = dis.hDC;
    let rc = dis.rcItem;
    let disabled = (dis.itemState.0 & ODS_DISABLED.0) != 0;

    // —— 底色 / 文字色 / 边框色 派生 ——
    let black = COLORREF(0x00000000);
    let white = COLORREF(0x00FFFFFF);
    let (bg, fg, border) = match style {
        ButtonStyle::Accent => {
            let bg = if disabled {
                blend(colors.accent, colors.bg, 0.5)
            } else if pressed {
                blend(colors.accent, black, 0.18)
            } else if hot {
                blend(colors.accent, white, 0.14)
            } else {
                colors.accent
            };
            (bg, white, None)
        }
        ButtonStyle::Secondary => {
            let base = colors.edit_bg;
            let bg = if disabled {
                blend(base, colors.bg, 0.5)
            } else if pressed {
                blend(base, colors.fg, 0.14)
            } else if hot {
                blend(base, colors.fg, 0.06)
            } else {
                base
            };
            (bg, colors.fg, Some(colors.border))
        }
    };
    let fg = if disabled { colors.muted } else { fg };

    // —— 圆角矩形（内缩 1px 容纳边框）——
    // SAFETY: GDI 对象创建/选择/删除均在本函数内成对完成；绘制同步完成，
    // 无跨消息生命周期。
    unsafe {
        let old_bk = SetBkMode(hdc, TRANSPARENT);
        let fill = CreateSolidBrush(bg);
        let (pen, old_pen) = match border {
            Some(c) => {
                let pen = CreatePen(PS_SOLID, 1, c);
                // SAFETY: pen 刚创建成功；SelectObject 返回原对象供恢复。
                (Some(pen), SelectObject(hdc, pen))
            }
            None => {
                // 无边框：空画笔（accent 按钮不加描边）
                // SAFETY: NULL_PEN 为库存对象，无需删除。
                (None, SelectObject(hdc, GetStockObject(NULL_PEN)))
            }
        };
        let old_brush = SelectObject(hdc, fill);
        let inset = RECT {
            left: rc.left + 1,
            top: rc.top + 1,
            right: rc.right - 1,
            bottom: rc.bottom - 1,
        };
        let radius = (inset.bottom - inset.top).min(12) / 3;
        let _ = RoundRect(
            hdc,
            inset.left,
            inset.top,
            inset.right,
            inset.bottom,
            radius,
            radius,
        );

        // —— 居中文字（全局消息字体）——
        let _ = SetTextColor(hdc, fg);
        let font = message_font();
        let old_font = if font.0 as usize != 0 {
            SelectObject(hdc, font)
        } else {
            HGDIOBJ(std::ptr::null_mut())
        };
        let mut buf = [0u16; 64];
        let len = GetWindowTextW(dis.hwndItem, &mut buf) as usize;
        if len > 0 {
            let mut tr = RECT {
                left: rc.left + 2,
                top: rc.top + 2,
                right: rc.right - 2,
                bottom: rc.bottom - 2,
            };
            let _ = DrawTextW(
                hdc,
                &mut buf[..len],
                &mut tr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        if !old_font.is_invalid() {
            let _ = SelectObject(hdc, old_font);
        }
        let _ = SelectObject(hdc, old_brush);
        if let Some(p) = pen {
            let _ = SelectObject(hdc, old_pen);
            let _ = DeleteObject(p);
        } else {
            let _ = SelectObject(hdc, old_pen);
        }
        let _ = DeleteObject(fill);
        let _ = SetBkMode(hdc, BACKGROUND_MODE(old_bk as u32));

        // —— 键盘焦点框（虚线矩形，保留键盘可达性）——
        if (dis.itemState.0 & ODS_FOCUS.0) != 0 && !disabled {
            let focus = RECT {
                left: rc.left + 4,
                top: rc.top + 4,
                right: rc.right - 4,
                bottom: rc.bottom - 4,
            };
            let _ = DrawFocusRect(hdc, &focus);
        }
    }
}

/// 按钮子类化窗口过程：跟踪悬停/按压状态，其余消息透传 BUTTON 类原过程
///
/// - `WM_MOUSEMOVE`：置位悬停并在冷→热翻转时臂定 `TME_LEAVE`；
/// - `WM_MOUSELEAVE`：复位悬停；
/// - `WM_LBUTTONDOWN/UP`：跟踪按压态；
/// - `WM_SETFOCUS/KILLFOCUS`：重绘以显示/隐藏焦点框；
/// - `WM_NCDESTROY`：注销状态注册表条目。
unsafe extern "system" fn button_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let became_hot = update_state(hwnd, |s| s.hot = true);
            if became_hot {
                // 冷→热翻转：臂定 TME_LEAVE，鼠标离开时收 WM_MOUSELEAVE
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: tme 为栈上结构，hwndTrack 为存活按钮；失败仅代表
                // 本次未臂定，后续 MOUSEMOVE 会重试。
                let _ = unsafe { TrackMouseEvent(&mut tme) };
            }
        }
        msg if msg == WM_MOUSELEAVE => {
            update_state(hwnd, |s| s.hot = false);
        }
        WM_LBUTTONDOWN => {
            update_state(hwnd, |s| s.pressed = true);
        }
        WM_LBUTTONUP => {
            update_state(hwnd, |s| s.pressed = false);
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            // 焦点变化影响焦点框显示，触发重绘
            // SAFETY: InvalidateRect 仅标记重绘区域。
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
        }
        WM_KEYDOWN => {
            // Tab / Esc 转发父窗口（R7）：焦点落在按钮上时 Tab 继续流转、
            // Esc 触发取消（父窗口 WM_KEYDOWN 统一处理）。标准 BUTTON 类
            // 过程会吞掉这两个键，非对话框窗口下不转发则焦点卡死在按钮上。
            let key = (wparam.0 & 0xFFFF) as u16;
            if key == VK_TAB.0 || key == VK_ESCAPE.0 {
                // SAFETY: GetParent 返回创建时指定的父窗口；PostMessageW 为线程
                // 安全标准 API，异步投递避免在子控件窗口过程内连锁处理消息。
                if let Ok(parent) = unsafe { GetParent(hwnd) } {
                    unsafe {
                        let _ = PostMessageW(parent, WM_KEYDOWN, wparam, lparam);
                    }
                }
                return LRESULT(0);
            }
        }
        WM_NCDESTROY => {
            // 窗口销毁：注销状态，防止注册表随进程膨胀
            let key = hwnd.0 as usize;
            with_states(|map| {
                map.remove(&key);
            });
        }
        _ => {}
    }
    // SAFETY: WM_KEYDOWN 等未消费消息与默认路径统一透传 BUTTON 类原始
    // 窗口过程（子类化仅替换实例过程，类过程不变）；GetClassLongPtrW 返回
    // 的过程指针经 transmute 还原为函数指针，Windows ABI 下往返转换良定义。
    let orig = unsafe { GetClassLongPtrW(hwnd, GCLP_WNDPROC) };
    let orig_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        unsafe { std::mem::transmute(orig) };
    orig_proc(hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态注册表：键值插入与移除（纯 HashMap 行为，不涉及窗口）
    #[test]
    fn state_registry_insert_remove() {
        with_states(|map| {
            map.insert(
                0xDEAD_BEEF,
                ButtonState {
                    style: ButtonStyle::Accent,
                    hot: false,
                    pressed: false,
                },
            );
        });
        let found =
            with_states(|map| map.get(&0xDEAD_BEEF).map(|s| (s.style, s.hot, s.pressed))).flatten();
        assert_eq!(found, Some((ButtonStyle::Accent, false, false)));
        with_states(|map| {
            map.remove(&0xDEAD_BEEF);
        });
        let gone = with_states(|map| map.get(&0xDEAD_BEEF).is_some()).unwrap_or(true);
        assert!(!gone);
    }
}
