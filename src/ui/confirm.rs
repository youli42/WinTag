//! 定制主题的"退出确认"弹窗（自绘暗色/圆角 + 自绘圆角按钮 + 键盘语义）
//!
//! 以 [`crate::ui::popup`] 的自绘小窗模板为蓝本，仿制出一个更小的确认窗：
//! 注册 `WinTagConfirm` 窗口类（含箭头光标），`ConfirmData` 经 `lpCreateParams`
//! 传入、`WM_DESTROY` 时 `Box::from_raw` 回收；主题色一律经 [`crate::ui::theme`]
//! 设施读取（禁用硬编码色值），按钮复用 [`crate::ui::button`] 的 `BS_OWNERDRAW`
//! 自绘方案（Accent"退出" + Secondary"取消"），`WM_DRAWITEM` 委托
//! `button::handle_draw_item`。
//!
//! 键盘语义：回车=确认退出、Esc=取消、Tab/Shift+Tab 在两按钮间循环焦点
//! （按钮子类会转发 Tab/Esc 给父窗口，故本窗口的 `WM_KEYDOWN` 必须处理）。

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, HDC};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, SetFocus, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetDlgCtrlID, GetDlgItem,
    GetSystemMetrics, PostMessageW, RegisterClassW, SetForegroundWindow, SetWindowPos, ShowWindow,
    CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, MINMAXINFO, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WNDCLASSW, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::common::{self, get_userdata, set_userdata, widestring, WM_APP_EXIT};
use crate::ui::button::{self, ButtonStyle};
use crate::ui::layout::{client_height, dp};
use crate::ui::theme::{apply_font_to_children, theme_colors};

// —— 设计像素常量（96 DPI 基准，运行时经 dp 缩放）——
const WIN_W: i32 = 380;
const WIN_H: i32 = 160;
const MARGIN: i32 = 16;
const BTN_W: i32 = 88;
const BTN_H: i32 = 30;
const BTN_GAP: i32 = 8;

// —— 控件 ID ——
/// "退出"确认按钮
const IDC_OK_BUTTON: i32 = 301;
/// "取消"按钮
const IDC_CANCEL_BUTTON: i32 = 302;

/// STATIC 控件水平居中样式（`SS_CENTER = 0x1`，WinUser.h）
///
/// 项目的 windows crate 未启用 `Win32_System_SystemServices` 特性，无法直接
/// 引用 `SS_CENTER`；此处按 WinUser.h 定义就近声明，语义与官方常量等价。
const SS_CENTER: u32 = 0x1;

/// Tab 焦点循环顺序（控件 ID）：退出 → 取消 → 回到退出
const FOCUS_ORDER: [i32; 2] = [IDC_OK_BUTTON, IDC_CANCEL_BUTTON];

/// 确认窗口的用户数据（随 `lpCreateParams` 传入，`WM_DESTROY` 时释放）
struct ConfirmData {
    /// 主线程隐藏窗口句柄（用于发送 `WM_APP_EXIT` 请求退出）
    hidden_hwnd: isize,
    /// 要显示的提示文本（如"确定退出？将丢弃 N 个标签/便签"）
    message: String,
    /// "退出"按钮句柄
    ok_btn: HWND,
    /// "取消"按钮句柄
    cancel_btn: HWND,
}

/// 创建"退出确认"弹窗
///
/// 注册 `WinTagConfirm` 窗口类（含箭头光标），创建一个小窗：
///
/// - 设计像素 `WIN_W`×`WIN_H`（经 [`dp`] 缩放）；
/// - `WS_OVERLAPPEDWINDOW` 去掉 MIN/MAX 按钮，`WS_EX_TOPMOST | WS_EX_TOOLWINDOW`；
/// - 根据 [`GetSystemMetrics`] 屏幕尺寸计算居中坐标；
/// - 顶部 `STATIC` 显示 [`message`]（主题字色/背景经 `WM_CTLCOLORSTATIC` 着色，
///   `SS_CENTER` 水平居中、超长自动换行），底部右下 Accent"退出" + Secondary"取消"。
///
/// 窗口创建成功后置顶显示并聚焦"退出"按钮（默认动作），使回车键自然触发确认。
/// 返回窗口句柄；创建失败时归还 `Box` 所有权并返回 `HWND::default()`（`Err` 语义）。
///
/// # 参数
///
/// - `message`：要显示的提示文本
/// - `hidden_hwnd`：主线程隐藏窗口句柄，用于发送 `WM_APP_EXIT` 请求退出
pub fn create_confirm(message: &str, hidden_hwnd: isize) -> HWND {
    let data = Box::new(ConfirmData {
        hidden_hwnd,
        message: message.to_string(),
        ok_btn: HWND::default(),
        cancel_btn: HWND::default(),
    });
    // SAFETY: data 的所有权转交给窗口（作为 lpCreateParams 传入），窗口 WM_DESTROY 时
    // 经 Box::from_raw 归还；若 CreateWindowExW 失败则在本函数内归还释放，均只释放一次。
    let data_ptr = Box::into_raw(data);

    let class_name = widestring("WinTagConfirm");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(confirm_wndproc),
        hInstance: HINSTANCE::default(),
        // 类光标必须非 NULL（NULL 会让 DefWindowProc 的 WM_SETCURSOR 隐藏光标）
        hCursor: common::arrow_cursor(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    // SAFETY: RegisterClassW 注册窗口类；类已存在时返回失败，忽略即可（幂等）。
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    // 窗口样式：WS_OVERLAPPEDWINDOW 去掉 MIN/MAX（与 popup 一致），保留标题栏关闭按钮；
    // 不带 WS_VISIBLE，先定位到屏幕居中再显示，避免默认位置闪现。
    let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 & !((WS_MINIMIZEBOX | WS_MAXIMIZEBOX).0));
    let ex_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0);

    // SAFETY: CreateWindowExW 为线程安全标准 API；失败时归还 data_ptr 所有权并打印错误，
    // 提前返回，避免 Box 泄漏。
    match unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("退出确认"),
            style,
            0,
            0,
            WIN_W,
            WIN_H,
            None,
            None,
            None,
            Some(data_ptr as *const c_void),
        )
    } {
        Ok(hwnd) => {
            // 屏幕居中：GetSystemMetrics(SM_CXSCREEN/SM_CYSCREEN) 取主屏物理像素尺寸；
            // 窗口宽高与 WM_GETMINMAXINFO 锁定的 dp 尺寸保持一致，否则居中偏移错误。
            let w = dp(hwnd, WIN_W);
            let h = dp(hwnd, WIN_H);
            // SAFETY: GetSystemMetrics 为线程安全标准 API，无失败路径，返回主屏尺寸。
            let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
            let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
            let x = (screen_w - w) / 2;
            let y = (screen_h - h) / 2;

            // SAFETY: hwnd 为刚创建成功的有效窗口句柄；SetWindowPos 定位定尺寸。
            unsafe {
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            // 激活并聚焦"退出"按钮：默认动作走回车/空格即触发确认（WM_COMMAND 分支）。
            // SAFETY: 本函数由主线程响应请求调用，进程刚收到输入，SetForegroundWindow
            // 允许激活弹窗；失败（如被系统拦截）时静默忽略。
            unsafe {
                let _ = SetForegroundWindow(hwnd);
                if let Ok(ok_btn) = GetDlgItem(hwnd, IDC_OK_BUTTON) {
                    let _ = SetFocus(ok_btn);
                }
            }
            hwnd
        }
        Err(_) => {
            // SAFETY: CreateWindowExW 失败时窗口未接管 data_ptr，所有权仍在本函数；
            // 重建 Box 释放内存，防止泄漏。
            unsafe {
                drop(Box::from_raw(data_ptr));
            }
            HWND::default()
        }
    }
}

extern "system" fn confirm_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            // SAFETY: lParam 指向 WM_CREATE 的 CREATESTRUCTW，其 lpCreateParams 由
            // create_confirm 传入 Box<ConfirmData> 原始指针，窗口销毁（WM_DESTROY）前始终有效。
            let data = unsafe {
                let cs =
                    &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
                cs.lpCreateParams as *mut ConfirmData
            };
            // SAFETY: data 在窗口生命周期内有效；set_userdata 由 common 封装，
            // 仅在主线程消息循环内调用。
            unsafe {
                set_userdata(hwnd, data as *mut c_void);
            }

            // 统一主题管理器（D17）：解析全局设置 → 写入全局调色板 → 取得暗色判定。
            // 需在子控件创建前完成：控件首次绘制即发 WM_CTLCOLOR* 请求配色。
            let theme_ctx = crate::ui::theme::sync_window_theme();
            let dark = theme_ctx.dark;
            // SAFETY: hwnd 为正在创建的窗口（WM_CREATE 期间有效）；
            // DWM 属性调用失败时静默忽略返回值。
            let _ = crate::ui::theme::apply_dark_mode(hwnd, dark);
            let _ = crate::ui::theme::apply_corner_preference(hwnd, theme_ctx.corner);

            // SAFETY: data 指针有效，借用弹窗数据创建子控件；以可变引用
            // 借用以回写按钮句柄（ok_btn/cancel_btn）。
            let pd = unsafe { &mut *data };

            // —— DPI 缩放后的布局坐标 ——
            let win_w = dp(hwnd, WIN_W);
            let m = dp(hwnd, MARGIN);
            let btn_w = dp(hwnd, BTN_W);
            let btn_h = dp(hwnd, BTN_H);
            let btn_gap = dp(hwnd, BTN_GAP);
            // 客户区高度：窗口外高（win_h）抵扣标题栏+边框后剩余可用高度。
            // 按钮行必须以客户区高度为基准定位，否则会溢出客户区底部被裁切
            //（历史 bug：退出确认窗"退出/取消"按钮被剪切，见 layout::TITLEBAR_H）。
            let client_h = client_height(hwnd, WIN_H);

            let instance = HINSTANCE::default();
            let child_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0);

            // —— 顶部消息区：STATIC 主题文本（SS_CENTER 水平居中、超长自动换行）——
            // 宽度撑满左右边距；高 = 按钮行上沿 - 间距 - 上边距（剩余空间留给文本区）。
            let btn_row_y = client_h - m - btn_h;
            let msg_h = btn_row_y - btn_gap - m;
            let msg_wide = widestring(&pd.message);
            // SAFETY: msg_wide 为 NUL 结尾宽字符串且存活于调用期间；静态文本控件创建
            // 失败不影响弹窗功能（窗口背景仍由 WM_ERASEBKGND 填充）。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    PCWSTR(msg_wide.as_ptr()),
                    WINDOW_STYLE(child_style.0 | SS_CENTER),
                    m,
                    m,
                    win_w - 2 * m,
                    msg_h,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }

            // —— 底部按钮：右下 Accent"退出"（确认）+ Secondary"取消"，自绘（BS_OWNERDRAW）——
            let btn_row_x = win_w - m - (btn_w * 2 + btn_gap);
            // SAFETY: create_button 内部注册状态并子类化；失败返回 Err，忽略即可
            // （按钮不可用不影响弹窗其余功能，WM_COMMAND 仍走原 ID 路由）。
            let ok_btn = button::create_button(
                hwnd,
                IDC_OK_BUTTON,
                "退出",
                btn_row_x,
                btn_row_y,
                btn_w,
                btn_h,
                ButtonStyle::Accent,
            );
            let cancel_btn = button::create_button(
                hwnd,
                IDC_CANCEL_BUTTON,
                "取消",
                btn_row_x + btn_w + btn_gap,
                btn_row_y,
                btn_w,
                btn_h,
                ButtonStyle::Secondary,
            );
            // 按钮句柄回写用户数据（供 Tab 焦点切换与键盘语义定位子控件）
            // SAFETY: pd 为 Box<ConfirmData> 的可变借用指针，窗口生命周期内有效。
            if let Ok(b) = ok_btn {
                pd.ok_btn = b;
            }
            if let Ok(b) = cancel_btn {
                pd.cancel_btn = b;
            }

            // 全局消息字体注入所有子控件（STATIC；按钮由 BS_OWNERDRAW 自绘时经
            // message_font() 选择字体）
            apply_font_to_children(hwnd);
            // 子控件 comctl32 主题变体（D17）
            crate::ui::theme::apply_control_theme(hwnd, dark);

            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            // 固定窗口尺寸：禁止缩放导致控件错乱（与 popup 同一策略）。
            // SAFETY: lParam 指向 MINMAXINFO，WM_GETMINMAXINFO 期间有效；
            // 覆盖 ptMaxSize 与 ptMinTrackSize/ptMaxTrackSize 锁定尺寸。
            let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let w = dp(hwnd, WIN_W);
            let h = dp(hwnd, WIN_H);
            mmi.ptMaxSize.x = w;
            mmi.ptMaxSize.y = h;
            mmi.ptMinTrackSize.x = w;
            mmi.ptMinTrackSize.y = h;
            mmi.ptMaxTrackSize.x = w;
            mmi.ptMaxTrackSize.y = h;
            LRESULT(0)
        }
        WM_DRAWITEM => {
            // 自绘圆角按钮（Accent/Secondary）委托 ui::button 处理
            // SAFETY: WM_DRAWITEM 的 lParam 指向 DRAWITEMSTRUCT，生命周期覆盖消息处理。
            if button::handle_draw_item(lparam) {
                LRESULT(1)
            } else {
                // SAFETY: 非按钮的 WM_DRAWITEM 透传默认过程。
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用；
            // 返回指针在 WM_DESTROY 释放前有效。
            let data = unsafe { get_userdata::<ConfirmData>(hwnd) };
            if data.is_null() {
                // SAFETY: DefWindowProcW 为默认窗口过程，其余消息原样透传。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            match id {
                // 确认退出：请求主线程退出（wParam=1 已确认）并销毁窗口
                IDC_OK_BUTTON => {
                    // SAFETY: data 已校验非空；PostMessageW 为线程安全标准 API，
                    // 异步投递避免在子控件窗口过程内同步销毁窗口。
                    unsafe {
                        let _ = PostMessageW(
                            HWND((*data).hidden_hwnd as *mut c_void),
                            WM_APP_EXIT,
                            WPARAM(1),
                            LPARAM(0),
                        );
                    }
                    // SAFETY: hwnd 为本窗口有效句柄，DestroyWindow 触发 WM_DESTROY 释放数据。
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                // 取消：直接销毁窗口（无退出请求）
                IDC_CANCEL_BUTTON => {
                    // SAFETY: hwnd 为本窗口有效句柄，DestroyWindow 触发 WM_DESTROY 释放数据。
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            // 键盘语义：焦点在本窗或转发的按键到达本分支（按钮子类已转发 Tab/Esc）。
            let key = (wparam.0 & 0xFFFF) as u16;
            const VK_RETURN_CODE: u16 = VK_RETURN.0;
            const VK_ESCAPE_CODE: u16 = VK_ESCAPE.0;
            const VK_TAB_CODE: u16 = VK_TAB.0;
            match key {
                // 回车：与点击"退出"确认等价
                VK_RETURN_CODE => {
                    // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
                    let data = unsafe { get_userdata::<ConfirmData>(hwnd) };
                    if !data.is_null() {
                        // SAFETY: data 已校验非空；PostMessageW 为线程安全标准 API。
                        unsafe {
                            let _ = PostMessageW(
                                HWND((*data).hidden_hwnd as *mut c_void),
                                WM_APP_EXIT,
                                WPARAM(1),
                                LPARAM(0),
                            );
                        }
                    }
                    // SAFETY: hwnd 为本窗口有效句柄，DestroyWindow 触发 WM_DESTROY 释放数据。
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                // Esc：与点击"取消"等价
                VK_ESCAPE_CODE => {
                    // SAFETY: hwnd 为本窗口有效句柄，DestroyWindow 触发 WM_DESTROY 释放数据。
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                // Tab / Shift+Tab：在两按钮间循环切换键盘焦点
                VK_TAB_CODE => {
                    // SAFETY: GetKeyState 查询虚拟键状态，无失败路径。
                    let shift_down = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                    focus_next_control(hwnd, !shift_down);
                }
                _ => {}
            }
            LRESULT(0)
        }
        // WM_ERASEBKGND：窗口自身客户区背景按主题背景色填充（防系统默认白色填充）
        WM_ERASEBKGND => {
            let Some(colors) = theme_colors() else {
                // SAFETY: DefWindowProcW 将未处理的 WM_ERASEBKGND 原样透传给系统
                // 默认窗口过程，参数与消息上下文一致，无额外内存操作。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            };
            // SAFETY: GetClientRect 写入栈上 RECT（调用期间存活），hwnd 为本窗口有效句柄。
            let mut rc = RECT::default();
            if unsafe { GetClientRect(hwnd, &mut rc) }.is_err() {
                // SAFETY: 同上方 DefWindowProcW 回退。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            // SAFETY: wParam 携带窗口客户区 HDC，仅在消息处理期间有效；画刷经
            // get_brush 进程级缓存持有、进程生命周期内不销毁。
            let hdc = HDC(wparam.0 as *mut c_void);
            unsafe {
                FillRect(hdc, &rc, crate::ui::theme::get_brush(colors.bg));
            }
            LRESULT(1)
        }
        // WM_CTLCOLOR*：子控件（STATIC 消息文本 / 按钮）重绘前请求配色，
        // 统一按当前主题调色板着色。
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => handle_ctlcolor(hwnd, msg, wparam, lparam),
        WM_CLOSE => {
            // SAFETY: hwnd 为本窗口有效句柄，DestroyWindow 触发 WM_DESTROY 释放数据。
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
            let data = unsafe { get_userdata::<ConfirmData>(hwnd) };
            if !data.is_null() {
                // SAFETY: data 由 Box::into_raw 产生，且仅在此释放一次（WM_DESTROY 后
                // 窗口销毁不再访问），重建 Box 归还所有权以释放内存。
                unsafe {
                    drop(Box::from_raw(data));
                }
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: DefWindowProcW 为默认窗口过程，其余消息原样透传。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// 处理 `WM_CTLCOLOR*`：按主题调色板为子控件设置文字色与背景色
///
/// - `WM_CTLCOLORSTATIC`：消息文本（STATIC），使用窗口前景/背景色；
/// - `WM_CTLCOLORBTN`：按钮（owner-draw 时实际不发送，保留以防标准按钮路径）。
///
/// 主题未初始化（`theme_colors` 返回 `None`）时回退 [`DefWindowProcW`] 走系统默认配色。
fn handle_ctlcolor(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(colors) = theme_colors() else {
        // SAFETY: DefWindowProcW 将未处理的 WM_CTLCOLOR* 原样透传给系统默认窗口过程，
        // 参数与消息上下文一致，无额外内存操作。
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    // SAFETY: wParam 携带子控件本次绘制使用的 HDC，仅在消息处理期间有效。
    let hdc = HDC(wparam.0 as *mut c_void);
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::SetTextColor(hdc, colors.fg);
        let _ = windows::Win32::Graphics::Gdi::SetBkColor(hdc, colors.bg);
    }
    // 返回背景色画刷句柄：控件以此绘制客户区背景。画刷经 get_brush 进程级
    // 缓存持有、进程生命周期内不销毁，可安全作为 LRESULT 返回。
    let brush = crate::ui::theme::get_brush(colors.bg);
    LRESULT(brush.0 as isize)
}

/// 在两按钮间循环切换键盘焦点（Tab 正向 / Shift+Tab 反向）
///
/// 按 [`FOCUS_ORDER`] 顺序从当前焦点控件取下一个；焦点不在已知控件上
/// （异常路径）时落到第一个控件（"退出"按钮）。
fn focus_next_control(hwnd: HWND, forward: bool) {
    // SAFETY: GetFocus 查询调用线程当前焦点窗口，无失败路径（无焦点返回 NULL）。
    let current = unsafe { GetFocus() };
    // SAFETY: current 为本线程焦点窗口句柄（可能为 NULL，查询返回 0 无副作用）。
    let cur_id = unsafe { GetDlgCtrlID(current) };
    let next_idx = match FOCUS_ORDER.iter().position(|&id| id == cur_id) {
        Some(i) => {
            let n = FOCUS_ORDER.len();
            let delta = if forward { 1 } else { n - 1 };
            (i + delta) % n
        }
        None => 0,
    };
    // SAFETY: GetDlgItem 按 ID 查询本窗口子控件，失败返回 Err 被忽略。
    if let Ok(next) = unsafe { GetDlgItem(hwnd, FOCUS_ORDER[next_idx]) } {
        // SAFETY: next 为存活子控件句柄；SetFocus 失败仅返回 Err，忽略。
        unsafe {
            let _ = SetFocus(next);
        }
    }
}
