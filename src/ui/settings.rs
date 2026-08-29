//! 设置窗口模块（任务 T3）
//!
//! 原生 Win32 设置窗口：允许用户选择主题模式（跟随系统/浅色/深色）与
//! 窗口圆角偏好（默认/圆角/小圆角），保存到配置文件，并通过自定义消息
//! `WM_APP_THEME_CHANGED` 向主线程广播主题变更。
//!
//! 窗口生命周期范式与 [`super::panel`] 一致：`create_settings` 注册窗口类 →
//! `lpCreateParams` 传 `Box<SettingsData>` → `WM_CREATE` 写入 `GWLP_USERDATA` →
//! `WM_DESTROY` 时 `Box::from_raw` 回收；`WM_CLOSE` 仅隐藏不销毁。

use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, SetBkColor, SetTextColor, HDC};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, PostMessageW, RegisterClassW, SendMessageW,
    SetForegroundWindow, ShowWindow, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HMENU, MINMAXINFO, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};

use crate::common::{get_userdata, set_userdata, widestring, WM_APP_THEME_CHANGED};
use crate::core::settings::{global_settings, CornerPreference, Settings, ThemeMode};
use crate::ui::button::{self, ButtonStyle};
use crate::ui::layout::dp;
use crate::ui::theme::{
    apply_corner_preference, apply_dark_mode, apply_font_to_children, detect_system_dark,
    get_brush, light_colors, theme_colors,
};

/// 控件 ID 常量（子控件消息路由用）
const IDC_THEME_COMBO: i32 = 201;
const IDC_CORNER_COMBO: i32 = 202;
const IDC_SAVE_BUTTON: i32 = 205;
const IDC_CANCEL_BUTTON: i32 = 206;

/// 设计像素常量（96 DPI 基准）
const MARGIN: i32 = 20;
const LABEL_W: i32 = 80;
const CTRL_H: i32 = 26;
const ROW_GAP: i32 = 48;
const BTN_W: i32 = 88;
const BTN_H: i32 = 30;
const BTN_GAP: i32 = 8;
const WIN_W: i32 = 460;
const WIN_H: i32 = 280;

/// 设置窗口的用户数据：设置存储引用、隐藏窗口句柄与可见状态
///
/// 通过 `GWLP_USERDATA` 关联到设置窗口，供设置窗口 WndProc 及辅助函数在
/// 消息处理期间取回。窗口销毁（`WM_DESTROY`）时由 `Box::from_raw` 回收。
/// 子控件句柄在 `WM_CREATE` 创建完成后写入，供下拉框读写复用。
pub struct SettingsData {
    /// 全局共享的设置存储（与 `global_settings()` 指向同一实例）
    pub settings: Arc<Mutex<Settings>>,
    /// 主线程隐藏窗口句柄（用于广播 `WM_APP_THEME_CHANGED` 主题变更消息）
    pub hidden_hwnd: isize,
    /// 设置窗口当前是否可见
    pub visible: bool,
    /// 主题模式下拉框句柄（`WM_CREATE` 后有效）
    pub theme_combo: HWND,
    /// 窗口圆角下拉框句柄（`WM_CREATE` 后有效）
    pub corner_combo: HWND,
    /// 主题编辑框句柄（预留：当前版本使用 `CBS_DROPDOWNLIST`，无独立编辑框）
    pub theme_edit: HWND,
    /// 圆角编辑框句柄（预留：当前版本使用 `CBS_DROPDOWNLIST`，无独立编辑框）
    pub corner_edit: HWND,
}

/// 创建设置窗口（初始隐藏）
///
/// 注册 `WinTagSettings` 窗口类并调用 `CreateWindowExW` 创建设置窗口，
/// 设置数据（[`SettingsData`]）通过 `lpCreateParams` 传递，由 `WM_CREATE`
/// 写入窗口用户数据。窗口初始隐藏，由 [`toggle_settings`] 控制显隐。
/// 窗口句柄同时写入内部 `SETTINGS_HWND`，供 [`settings_hwnd`] 查询。
///
/// # 参数
///
/// - `data`：设置窗口数据（含设置存储引用与隐藏窗口句柄）
///
/// # 返回值
///
/// 成功时返回设置窗口句柄；创建失败时打印错误信息并返回默认（NULL）句柄，
/// 调用方应先检查返回值再使用。
pub fn create_settings(data: SettingsData) -> HWND {
    let data = Box::new(data);
    let data_ptr = Box::into_raw(data);

    let class_name = widestring("WinTagSettings");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(settings_wndproc),
        hInstance: HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    // SAFETY: RegisterClassW 注册窗口类，返回已有或新注册结果均被忽略；
    // 窗口类名固定为 "WinTagSettings"，WndProc 与类名一一对应，重复注册幂等。
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0); // 初始隐藏，由 toggle_settings 控制
    let result = unsafe {
        // SAFETY: class_name 与窗口标题为本地 NUL 结尾宽字符串，调用期间有效；
        // data_ptr 所有权随 lpCreateParams 转移给 WM_CREATE（写入 GWLP_USERDATA），
        // 创建失败时由下方 Err 分支回收，避免 Box 泄漏。
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("WinTag - 设置"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIN_W,
            WIN_H,
            None,
            None,
            None,
            Some(data_ptr as *const c_void),
        )
    };

    match result {
        Ok(hwnd) => {
            // SAFETY: hwnd 为刚创建成功的有效窗口句柄，句柄原始值写入
            // 进程级 SETTINGS_HWND 供 settings_hwnd 查询；重复设置被忽略。
            let _ = SETTINGS_HWND.set(hwnd.0 as isize);
            hwnd
        }
        Err(e) => {
            eprintln!("创建设置窗口失败: {e}");
            // SAFETY: data_ptr 由 Box::into_raw 产生且窗口创建失败，所有权未转移
            // 给任何 WndProc（WM_CREATE 未执行），在此回收防止内存泄漏。
            unsafe {
                drop(Box::from_raw(data_ptr));
            }
            HWND::default()
        }
    }
}

/// 已创建设置窗口的句柄（未创建时返回 None）
///
/// 供主线程（T4 接线）在收到主题变更广播后重新应用主题到各窗口时查询。
pub fn settings_hwnd() -> Option<isize> {
    SETTINGS_HWND.get().copied()
}

/// 设置窗口句柄单例：进程生命周期内仅创建一次
static SETTINGS_HWND: OnceLock<isize> = OnceLock::new();

/// 切换设置窗口的显示 / 隐藏状态
///
/// 窗口当前可见则隐藏；不可见则刷新下拉框选中项、显示窗口并将其置前到前台。
/// 同时将调用方最新传入的设置存储与隐藏窗口句柄写入用户数据（保持同步）。
///
/// # 参数
///
/// - `hwnd`：由 [`create_settings`] 创建设置窗口句柄
/// - `hidden_hwnd`：主线程隐藏窗口句柄（主题变更消息的接收方）
/// - `settings`：全局共享的设置存储
pub fn toggle_settings(hwnd: HWND, hidden_hwnd: isize, settings: Arc<Mutex<Settings>>) {
    // SAFETY: hwnd 由 create_settings 返回，窗口存活期间 SettingsData 有效。
    let data = unsafe { get_userdata::<SettingsData>(hwnd) };
    if data.is_null() {
        return;
    }

    // SAFETY: data 已校验非空，窗口仍存活。
    let visible = unsafe { (*data).visible };
    if !visible {
        // SAFETY: data 已校验非空；刷新调用方传入的最新设置引用与隐藏窗口句柄。
        unsafe {
            (*data).settings = settings;
            (*data).hidden_hwnd = hidden_hwnd;
            (*data).visible = true;
        }
        refresh_selections(hwnd);
        // SAFETY: 显示设置窗口并置前到前台，由用户显式触发的窗口操作。
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    } else {
        // SAFETY: data 已校验非空，隐藏设置窗口。
        unsafe {
            (*data).visible = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

extern "system" fn settings_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            // SAFETY: WM_CREATE 的 lParam 指向 CREATESTRUCTW，其生命周期覆盖整个
            // 消息处理过程；lpCreateParams 为 create_settings 传入的 SettingsData 指针。
            let data = unsafe {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                cs.lpCreateParams as *mut SettingsData
            };
            // SAFETY: data 指针来自 create_settings，窗口生命周期内始终有效；
            // 写入 GWLP_USERDATA 供后续消息取回，WM_DESTROY 时统一回收。
            unsafe {
                set_userdata(hwnd, data as *mut c_void);
            }

            let instance = HINSTANCE::default();
            let child_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0);

            // —— DPI 缩放后的布局坐标 ——
            let m = dp(hwnd, MARGIN);
            let label_w = dp(hwnd, LABEL_W);
            let ctrl_h = dp(hwnd, CTRL_H);
            let row1_y = m + dp(hwnd, 8);
            let row2_y = row1_y + ctrl_h + dp(hwnd, ROW_GAP);
            let combo_x = m + label_w;
            let combo_w = WIN_W - m - combo_x;
            let client_h = WIN_H - dp(hwnd, 30);
            let btn_w = dp(hwnd, BTN_W);
            let btn_h = dp(hwnd, BTN_H);
            let btn_gap = dp(hwnd, BTN_GAP);
            let btn_row_y = client_h - m - btn_h;
            let btn_row_x = WIN_W - m - (btn_w * 2 + btn_gap);

            // —— 主题模式标签 ——
            // SAFETY: 静态标签创建失败忽略，不影响其余子控件。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("主题模式"),
                    child_style,
                    m,
                    row1_y,
                    label_w,
                    ctrl_h,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }

            // —— 主题模式下拉框（只读下拉列表，高度含展开后的列表区域）——
            let combo_style = WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
            );
            // SAFETY: 创建 COMBOBOX 子控件（ID = IDC_THEME_COMBO），样式为只读下拉列表；
            // 失败时返回默认句柄并打印告警。
            let theme_combo = match unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("COMBOBOX"),
                    windows::core::w!(""),
                    combo_style,
                    combo_x,
                    row1_y,
                    combo_w,
                    dp(hwnd, 200),
                    hwnd,
                    HMENU(IDC_THEME_COMBO as *mut c_void),
                    instance,
                    None,
                )
            } {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("创建主题下拉框失败: {e}");
                    HWND::default()
                }
            };
            // 填充主题下拉项（顺序与 ThemeMode 枚举变体索引一致：System/Light/Dark）
            for mode in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
                let label = widestring(mode.label_cn());
                // SAFETY: label 为 NUL 结尾宽字符串且存活于调用期间，
                // CB_ADDSTRING 在消息返回前完成文本拷贝。
                unsafe {
                    let _ = SendMessageW(
                        theme_combo,
                        CB_ADDSTRING,
                        WPARAM(0),
                        LPARAM(label.as_ptr() as isize),
                    );
                }
            }

            // —— 窗口圆角标签 ——
            // SAFETY: 静态标签创建失败忽略，不影响其余子控件。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("窗口圆角"),
                    child_style,
                    m,
                    row2_y,
                    label_w,
                    ctrl_h,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }

            // —— 窗口圆角下拉框 ——
            // SAFETY: 创建 COMBOBOX 子控件（ID = IDC_CORNER_COMBO），样式同主题下拉框；
            // 失败时返回默认句柄并打印告警。
            let corner_combo = match unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("COMBOBOX"),
                    windows::core::w!(""),
                    combo_style,
                    combo_x,
                    row2_y,
                    combo_w,
                    dp(hwnd, 200),
                    hwnd,
                    HMENU(IDC_CORNER_COMBO as *mut c_void),
                    instance,
                    None,
                )
            } {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("创建圆角下拉框失败: {e}");
                    HWND::default()
                }
            };
            // 填充圆角下拉项（顺序与 CornerPreference 枚举变体索引一致：
            // Default/Round/SmallRound）
            for corner in [
                CornerPreference::Default,
                CornerPreference::Round,
                CornerPreference::SmallRound,
            ] {
                let label = widestring(corner.label_cn());
                // SAFETY: label 为 NUL 结尾宽字符串且存活于调用期间，
                // CB_ADDSTRING 在消息返回前完成文本拷贝。
                unsafe {
                    let _ = SendMessageW(
                        corner_combo,
                        CB_ADDSTRING,
                        WPARAM(0),
                        LPARAM(label.as_ptr() as isize),
                    );
                }
            }

            // —— 按钮行：右下 保存（accent）/取消（次要），自绘（9.4/9.8）——
            // SAFETY: create_button 内部注册状态并子类化；失败返回 Err，忽略。
            let _ = button::create_button(
                hwnd,
                IDC_SAVE_BUTTON,
                "保存",
                btn_row_x,
                btn_row_y,
                btn_w,
                btn_h,
                ButtonStyle::Accent,
            );
            let _ = button::create_button(
                hwnd,
                IDC_CANCEL_BUTTON,
                "取消",
                btn_row_x + btn_w + btn_gap,
                btn_row_y,
                btn_w,
                btn_h,
                ButtonStyle::Secondary,
            );

            // 保存子控件句柄到用户数据（供 toggle/WM_COMMAND 复用）
            // SAFETY: data 指针有效（见上方校验），仅覆盖下拉框句柄；
            // theme_edit/corner_edit 为预留字段，保持默认句柄。
            unsafe {
                (*data).theme_combo = theme_combo;
                (*data).corner_combo = corner_combo;
            }

            // 全局消息字体注入所有子控件（STATIC/COMBOBOX；按钮自绘时选用 message_font）
            apply_font_to_children(hwnd);

            // 应用当前主题与圆角偏好（从全局设置读取，未注入时用默认值）
            let current = current_settings();
            let dark = match current.theme {
                ThemeMode::Dark => true,
                ThemeMode::System => detect_system_dark(),
                ThemeMode::Light => false,
            };
            // SAFETY: hwnd 为正在创建中的有效窗口句柄；apply_* 内部已处理 DWM 调用失败。
            let _ = apply_dark_mode(hwnd, dark);
            let _ = apply_corner_preference(hwnd, current.corner);

            // 按当前全局设置初始化下拉框选中项
            refresh_selections(hwnd);

            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
            // 静态文本 / 下拉列表 / 按钮：窗口背景色 + 前景文本色
            let c = theme_colors().unwrap_or_else(light_colors);
            ctlcolor_brush(lparam, c.bg, c.fg)
        }
        WM_CTLCOLOREDIT => {
            // 编辑框区域（预留的 theme_edit/corner_edit 命中时用编辑框配色，
            // 其余回退窗口配色）；ComboBox 的下拉列表部分由系统管理，
            // 父窗口通过 WM_CTLCOLORLISTBOX 处理编辑框区域。
            // SAFETY: 设置窗口存活期间 SettingsData 有效（WM_DESTROY 才回收）。
            let data = unsafe { get_userdata::<SettingsData>(hwnd) };
            let ctl = HWND(lparam.0 as *mut c_void);
            let is_themed_edit = !data.is_null()
                // SAFETY: data 已校验非空；比较子控件句柄是否命中预留编辑框。
                && unsafe { ctl == (*data).theme_edit || ctl == (*data).corner_edit };
            let c = theme_colors().unwrap_or_else(light_colors);
            let (bg, fg) = if is_themed_edit {
                (c.edit_bg, c.edit_fg)
            } else {
                (c.bg, c.fg)
            };
            ctlcolor_brush(lparam, bg, fg)
        }
        // WM_ERASEBKGND：窗口自身客户区背景。WM_CTLCOLOR* 只处理子控件配色，
        // 客户区背景由 WM_ERASEBKGND 决定；默认 DefWindowProc 用白色类画刷擦除
        // 背景（暗色主题下表现为白色窗口底色），此处按主题色填充并返回 1
        // 告知系统背景已擦除，阻止默认白色填充。
        WM_GETMINMAXINFO => {
            // 固定设置窗口尺寸（问题 9.6）：禁止缩放导致控件错乱
            // SAFETY: lParam 指向 MINMAXINFO，WM_GETMINMAXINFO 期间有效；
            // 覆盖 ptMaxSize 与 ptMin/MaxTrackSize 锁定尺寸。
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
            // 自绘按钮绘制请求（问题 9.4/9.8）：委托 ui::button 处理
            if button::handle_draw_item(lparam) {
                LRESULT(1)
            } else {
                // SAFETY: 非按钮的 WM_DRAWITEM 透传默认过程。
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_ERASEBKGND => {
            // 主题状态未初始化（从未调用 set_theme）时回退系统默认擦除
            let Some(colors) = theme_colors() else {
                // SAFETY: DefWindowProcW 将未处理的 WM_ERASEBKGND 原样透传给系统
                // 默认窗口过程，参数与消息上下文一致，无额外内存操作。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            };
            // SAFETY: wParam 携带客户区 HDC，仅在消息处理期间有效；
            // FillRect 只填充本次消息对应的客户区矩形，无跨消息生命周期。
            let hdc = HDC(wparam.0 as *mut c_void);
            // SAFETY: GetClientRect 将客户区矩形写入栈上 RECT，调用期间有效。
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            // SAFETY: rc 为栈上局部值，画刷句柄由 get_brush 进程级缓存持有，均有效。
            unsafe {
                let _ = FillRect(hdc, &rc, get_brush(colors.bg));
            }
            // 返回 1 表示背景已擦除，阻止 DefWindowProc 用白色类画刷填充
            LRESULT(1)
        }
        WM_COMMAND => {
            // WM_COMMAND 的 wParam 低 16 位为控件 ID，高 16 位为通知码
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                IDC_SAVE_BUTTON => save_and_hide(hwnd),
                IDC_CANCEL_BUTTON => {
                    // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
                    let data = unsafe { get_userdata::<SettingsData>(hwnd) };
                    if !data.is_null() {
                        // SAFETY: data 已校验非空，标记为隐藏。
                        unsafe {
                            (*data).visible = false;
                        }
                    }
                    // SAFETY: 取消仅隐藏设置窗口，不保存；参数均为栈上局部值。
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
                // 下拉框的通知（如 CBN_SELCHANGE）无需处理
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
            let data = unsafe { get_userdata::<SettingsData>(hwnd) };
            if data.is_null() {
                // SAFETY: DefWindowProcW 将未处理消息原样透传给系统默认窗口过程。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            // wParam 低 16 位为虚拟键码（VIRTUAL_KEY 包装 u16，此处还原为 usize 比较）
            if wparam.0 == VK_ESCAPE.0 as usize {
                // ESC：隐藏设置窗口，不保存
                // SAFETY: data 已校验非空，隐藏窗口（不销毁，镜像 panel 行为）。
                unsafe {
                    (*data).visible = false;
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                LRESULT(0)
            } else if wparam.0 == VK_RETURN.0 as usize {
                // 回车：与保存按钮语义一致
                save_and_hide(hwnd);
                LRESULT(0)
            } else {
                // SAFETY: DefWindowProcW 将未处理消息原样透传给系统默认窗口过程。
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_CLOSE => {
            // SAFETY: 设置窗口存活期间 SettingsData 有效（WM_DESTROY 才回收）。
            let data = unsafe { get_userdata::<SettingsData>(hwnd) };
            if !data.is_null() {
                // SAFETY: data 已校验非空，窗口仍存活。
                unsafe {
                    (*data).visible = false;
                }
            }
            // SAFETY: WM_CLOSE 仅隐藏设置窗口，不销毁窗口，保持 SettingsData 存活。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: WM_DESTROY 表示窗口即将销毁，回收此前 Box::into_raw 转移的
            // SettingsData，防止内存泄漏。
            let data = unsafe { get_userdata::<SettingsData>(hwnd) };
            if !data.is_null() {
                // SAFETY: data 由 Box::into_raw 产生，此处为唯一所有权释放点。
                unsafe {
                    drop(Box::from_raw(data));
                }
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: DefWindowProcW 将未处理消息原样透传给系统默认窗口过程，
            // 参数与消息上下文一致，无额外内存操作。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// 按主题调色板为控件设置文本/背景色，返回背景画刷（`WM_CTLCOLOR*` 消息返回值）
///
/// `lParam` 为消息携带的控件 DC 句柄；`bg`/`fg` 为 BGR 格式的 `COLORREF`。
/// 画刷来自 [`get_brush`] 的进程级缓存（永不删除），可直接作为返回值。
fn ctlcolor_brush(lparam: LPARAM, bg: COLORREF, fg: COLORREF) -> LRESULT {
    let hdc = HDC(lparam.0 as *mut c_void);
    // SAFETY: lParam 在 WM_CTLCOLOR* 消息中为控件 DC 句柄，消息处理期间有效；
    // SetTextColor/SetBkColor 为线程安全标准 API，失败时仅影响该次绘制的配色。
    unsafe {
        let _ = SetTextColor(hdc, fg);
        let _ = SetBkColor(hdc, bg);
    }
    LRESULT(get_brush(bg).0 as isize)
}

/// 刷新两个下拉框的选中项为当前全局设置
///
/// 从 [`global_settings`] 读取当前主题/圆角（未注入或锁中毒时回退默认值），
/// 通过 `CB_SETCURSEL` 同步到下拉框，供窗口创建与显示时调用。
fn refresh_selections(hwnd: HWND) {
    // SAFETY: 设置窗口由 create_settings 创建，窗口存活期间 SettingsData 有效。
    let data = unsafe { get_userdata::<SettingsData>(hwnd) };
    if data.is_null() {
        return;
    }
    let current = current_settings();
    // SAFETY: data 已校验非空；theme_combo/corner_combo 在 WM_CREATE 时创建，
    // CB_SETCURSEL 以枚举变体索引（System=0/Light=1/Dark=2 等）作 wParam。
    unsafe {
        let _ = SendMessageW(
            (*data).theme_combo,
            CB_SETCURSEL,
            WPARAM(current.theme as usize),
            LPARAM(0),
        );
        let _ = SendMessageW(
            (*data).corner_combo,
            CB_SETCURSEL,
            WPARAM(current.corner as usize),
            LPARAM(0),
        );
    }
}

/// 读取当前全局设置（未注入或锁中毒时回退默认值）
fn current_settings() -> Settings {
    global_settings()
        .and_then(|g| g.lock().ok().map(|guard| *guard))
        .unwrap_or_default()
}

/// 执行保存逻辑：读取下拉框选中项 → 更新全局设置并写盘 → 广播主题变更 → 隐藏窗口
///
/// 供保存按钮（`WM_COMMAND`）与回车键（`WM_KEYDOWN` VK_RETURN）共用。
fn save_and_hide(hwnd: HWND) {
    // SAFETY: 设置窗口由 create_settings 创建，窗口存活期间 SettingsData 有效。
    let data = unsafe { get_userdata::<SettingsData>(hwnd) };
    if data.is_null() {
        return;
    }
    // SAFETY: data 已校验非空，字段访问安全。
    let pd = unsafe { &*data };

    // 读取两个下拉框当前选中索引（未选中时为 -1，映射函数回退默认值）
    // SAFETY: theme_combo/corner_combo 在 WM_CREATE 时创建，均为有效子控件句柄。
    let theme_idx = unsafe { SendMessageW(pd.theme_combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)) }.0;
    let corner_idx = unsafe { SendMessageW(pd.corner_combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)) }.0;
    let new_settings = Settings {
        theme: theme_from_index(theme_idx as i32),
        corner: corner_from_index(corner_idx as i32),
    };

    // 更新内存中的设置并写盘（锁中毒时跳过，避免 panic 传播）
    if let Ok(mut settings) = pd.settings.lock() {
        *settings = new_settings;
        if let Err(e) = settings.save() {
            eprintln!("保存设置失败: {e}");
        }
    }

    // 广播主题变更消息到主线程隐藏窗口（由主线程重新应用主题到各窗口）
    // SAFETY: hidden_hwnd 为主线程隐藏窗口句柄；PostMessageW 为线程安全标准 API。
    unsafe {
        let _ = PostMessageW(
            HWND(pd.hidden_hwnd as *mut c_void),
            WM_APP_THEME_CHANGED,
            WPARAM(0),
            LPARAM(0),
        );
    }

    // SAFETY: data 已校验非空，标记为隐藏并 SW_HIDE（不销毁，镜像 panel 行为）。
    unsafe {
        (*data).visible = false;
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// 将主题下拉框选中索引映射为主题模式（越界/未选中回退跟随系统）
///
/// 索引顺序与 [`WM_CREATE`] 中填充下拉项的枚举顺序一致：
/// 0 = 跟随系统、1 = 浅色、2 = 深色。
fn theme_from_index(idx: i32) -> ThemeMode {
    match idx {
        1 => ThemeMode::Light,
        2 => ThemeMode::Dark,
        _ => ThemeMode::System,
    }
}

/// 将圆角下拉框选中索引映射为圆角偏好（越界/未选中回退默认圆角）
///
/// 索引顺序与 [`WM_CREATE`] 中填充下拉项的枚举顺序一致：
/// 0 = 默认、1 = 圆角、2 = 小圆角。
fn corner_from_index(idx: i32) -> CornerPreference {
    match idx {
        1 => CornerPreference::Round,
        2 => CornerPreference::SmallRound,
        _ => CornerPreference::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 下拉框索引 → 主题模式映射（与 WM_CREATE 填充顺序一致）
    #[test]
    fn theme_index_mapping() {
        assert_eq!(theme_from_index(0), ThemeMode::System);
        assert_eq!(theme_from_index(1), ThemeMode::Light);
        assert_eq!(theme_from_index(2), ThemeMode::Dark);
        // 越界（-1 = 未选中）与任意非法值回退默认
        assert_eq!(theme_from_index(-1), ThemeMode::System);
        assert_eq!(theme_from_index(99), ThemeMode::System);
    }

    /// 下拉框索引 → 圆角偏好映射（与 WM_CREATE 填充顺序一致）
    #[test]
    fn corner_index_mapping() {
        assert_eq!(corner_from_index(0), CornerPreference::Default);
        assert_eq!(corner_from_index(1), CornerPreference::Round);
        assert_eq!(corner_from_index(2), CornerPreference::SmallRound);
        // 越界（-1 = 未选中）与任意非法值回退默认
        assert_eq!(corner_from_index(-1), CornerPreference::Default);
        assert_eq!(corner_from_index(99), CornerPreference::Default);
    }
}
