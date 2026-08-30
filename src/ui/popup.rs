use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, DrawFocusRect, FillRect, GetMonitorInfoW,
    InvalidateRect, MonitorFromPoint, RoundRect, SelectObject, SetBkColor, SetTextColor, HDC,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, PS_SOLID,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, EM_SETSEL, ODS_FOCUS, ODT_BUTTON};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, SetFocus, VK_A, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClassLongPtrW, GetClientRect, GetCursorPos,
    GetDlgCtrlID, GetDlgItem, GetParent, GetWindowTextLengthW, GetWindowTextW, IsWindow,
    PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, BS_OWNERDRAW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, GCLP_WNDPROC, GWLP_WNDPROC, HMENU, HWND_TOPMOST, SWP_NOACTIVATE,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_KEYDOWN, WNDCLASSW, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_OVERLAPPEDWINDOW, WS_VISIBLE, WS_VSCROLL,
};

use crate::common::{self, get_userdata, set_userdata, widestring};
use crate::core::matcher;
use crate::core::tag::{Tag, TagColor, TagStore};
use crate::ui::button::{self, ButtonStyle};
use crate::ui::layout::dp;
use crate::ui::theme::apply_font_to_children;

const IDC_TITLE_EDIT: i32 = 101;
const IDC_NOTE_EDIT: i32 = 103;
const IDC_OK_BUTTON: i32 = 104;
const IDC_CANCEL_BUTTON: i32 = 105;
/// 颜色选择色块起始 ID（依次为 [`COLOR_CHOICES`] 各变体，R16）
const IDC_COLOR_BASE: i32 = 110;

/// Tab 焦点循环顺序（控件 ID）：标题 → 备注 → 确认 → 取消 → 回到标题（R7）
const FOCUS_ORDER: [i32; 4] = [
    IDC_TITLE_EDIT,
    IDC_NOTE_EDIT,
    IDC_OK_BUTTON,
    IDC_CANCEL_BUTTON,
];

/// 颜色选择行的候选色（顺序即色块 ID 顺序，R16）
const COLOR_CHOICES: [TagColor; 5] = [
    TagColor::Orange,
    TagColor::Blue,
    TagColor::Green,
    TagColor::Red,
    TagColor::Purple,
];

/// 设计像素常量（96 DPI 基准），运行时经 [`dp`] 缩放
const MARGIN: i32 = 12;
const LABEL_W: i32 = 52;
const CTRL_H: i32 = 26;
const BTN_W: i32 = 88;
const BTN_H: i32 = 30;
const BTN_GAP: i32 = 8;
const INFO_H: i32 = 20;
const TITLE_ROW_Y: i32 = 44;
/// 颜色选择行 Y 坐标（标题行下方，R16）
const COLOR_ROW_Y: i32 = 78;
const NOTE_ROW_Y: i32 = 112;
/// 颜色色块尺寸与间距（R16）
const CHIP_W: i32 = 40;
const CHIP_H: i32 = 16;
const CHIP_GAP: i32 = 8;
const WIN_W: i32 = 420;
const WIN_H: i32 = 320;

/// 弹窗窗口的用户数据（随 `lpCreateParams` 传入，`WM_DESTROY` 时释放）
struct PopupData {
    tag_store: Arc<Mutex<TagStore>>,
    target_hwnd: isize,
    window_title: String,
    process_name: String,
    hidden_hwnd: isize,
    /// 当前选中的标签颜色（R16：色块行选择，保存时写入 Tag）
    selected_color: TagColor,
}

/// 当前打开的标记弹窗注册表：(目标窗口句柄, 弹窗句柄)
///
/// 同一时间至多一个弹窗：同目标重复请求（如多次点击同一角标）复用并置前，
/// 异目标请求销毁旧弹窗后新建。仅主线程读写。
static ACTIVE_POPUP: Mutex<Option<(isize, isize)>> = Mutex::new(None);

/// 创建“标记窗口”弹窗
///
/// 在主线程中为指定目标窗口弹出标签编辑弹窗，用于输入标签标题与备注。
/// 弹窗数据（[`PopupData`]）通过 `lpCreateParams` 传入，窗口销毁（`WM_DESTROY`）时释放；
/// 确认按钮触发标签写入成功后，才发送 `WM_CREATE_OVERLAY` 请求覆盖层创建。
/// 同一时间仅存在一个弹窗（R18）：同目标请求复用置前，异目标请求替换；
/// 弹窗出现在鼠标附近（R19），越界时钳制回显示器工作区。
///
/// # 参数
///
/// - `store`：全局标签存储（共享引用）
/// - `target_hwnd`：目标窗口句柄（以 `isize` 表示）
/// - `window_title`：目标窗口标题，标签标题为空时作为回退值
/// - `process_name`：目标窗口进程名
/// - `hidden_hwnd`：主线程隐藏窗口句柄，用于发送覆盖层创建消息
pub fn create_popup(
    store: Arc<Mutex<TagStore>>,
    target_hwnd: isize,
    window_title: &str,
    process_name: &str,
    hidden_hwnd: isize,
) {
    // 单例复用（R18）：已有弹窗时——同目标置前复用（不再叠加新弹窗），
    // 异目标销毁旧弹窗（WM_DESTROY 释放数据并清理注册表）后新建
    if let Ok(mut active) = ACTIVE_POPUP.lock() {
        if let Some((old_target, old_hwnd)) = *active {
            // SAFETY: IsWindow 为只读查询，句柄失效返回 FALSE。
            if unsafe { IsWindow(HWND(old_hwnd as *mut std::ffi::c_void)) }.as_bool() {
                if old_target == target_hwnd {
                    // 复用：置前并聚焦标题框，直接返回
                    let popup = HWND(old_hwnd as *mut std::ffi::c_void);
                    // SAFETY: popup 为存活弹窗句柄；ShowWindow/SetForegroundWindow/
                    // GetDlgItem/SetFocus 失败均仅返回错误值，忽略。
                    unsafe {
                        let _ = ShowWindow(popup, SW_SHOW);
                        let _ = SetForegroundWindow(popup);
                        if let Ok(title_edit) = GetDlgItem(popup, IDC_TITLE_EDIT) {
                            let _ = SetFocus(title_edit);
                        }
                    }
                    return;
                }
                // SAFETY: old_hwnd 为本进程存活弹窗，DestroyWindow 触发
                // WM_DESTROY 释放其用户数据（主线程内调用合法）。
                unsafe {
                    let _ = DestroyWindow(HWND(old_hwnd as *mut std::ffi::c_void));
                }
            }
            *active = None;
        }
    }

    // 预选颜色：已有标签沿用其颜色，否则默认橙色（R16）
    let selected_color = store
        .lock()
        .ok()
        .and_then(|s| s.get(&target_hwnd).map(|t| t.color))
        .unwrap_or(TagColor::Orange);

    let data = Box::new(PopupData {
        tag_store: store,
        target_hwnd,
        window_title: window_title.to_string(),
        process_name: process_name.to_string(),
        hidden_hwnd,
        selected_color,
    });
    // SAFETY: data 的所有权转交给弹窗窗口（作为 lpCreateParams 传入），窗口 WM_DESTROY 时
    // 通过 Box::from_raw 归还；若 CreateWindowExW 失败则在本函数内归还释放，均只释放一次。
    let data_ptr = Box::into_raw(data);

    let class_name = widestring("WinTagPopup");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(popup_wndproc),
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

    // 窗口样式修正（问题 10 + 9.6）：
    // - 恢复 WS_SYSMENU（保留关闭按钮）；去掉 WS_MINIMIZEBOX/WS_MAXIMIZEBOX
    //   （原代码去掉 SYSMENU 却留着 MIN/MAX，标题栏无关闭按钮却有最小/最大化，外观怪异）；
    // - 保留 WS_THICKFRAME（WS_OVERLAPPEDWINDOW 的可缩放边框）但通过
    //   WM_GETMINMAXINFO 固定尺寸，防止控件位置错乱（9.6 原本靠布局自适应，
    //   但固定尺寸是最简且不撕裂的方案）。
    // - 不带 WS_VISIBLE：先定位到光标附近再显示，避免默认位置闪现。
    let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 & !((WS_MINIMIZEBOX | WS_MAXIMIZEBOX).0));
    let ex_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0);

    // 光标附近初始位置（R19）；创建后按 dp 实际尺寸精确钳制
    let mut origin = POINT::default();
    // SAFETY: GetCursorPos 无前置条件，失败时 origin 保持 (0,0)。
    unsafe {
        let _ = GetCursorPos(&mut origin);
    }

    // SAFETY: CreateWindowExW 为线程安全标准 API；失败时归还 data_ptr 所有权并打印错误，
    // 提前返回，避免 Box 泄漏。
    match unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("标记窗口"),
            style,
            origin.x + 16,
            origin.y + 16,
            WIN_W,
            WIN_H,
            None,
            None,
            None,
            Some(data_ptr as *const std::ffi::c_void),
        )
    } {
        Ok(hwnd) => {
            // 登记活动弹窗（单例复用依据，R18）
            if let Ok(mut active) = ACTIVE_POPUP.lock() {
                *active = Some((target_hwnd, hwnd.0 as isize));
            }

            // 尺寸对齐 DPI（WM_GETMINMAXINFO 锁定的就是 dp 尺寸，创建尺寸需一致）
            // 并钳制到光标附近 + 显示器工作区内，然后一次性显示（无默认位置闪现）。
            let w = dp(hwnd, WIN_W);
            let h = dp(hwnd, WIN_H);
            let (x, y) = clamp_to_work(origin.x + 16, origin.y + 16, w, h);
            // SAFETY: hwnd 为刚创建成功的有效窗口句柄；SetWindowPos 定位定尺寸。
            unsafe {
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            // 弹窗已显示：激活窗口（问题 5.1）。WM_CREATE 中的 SetFocus 在窗口未激活时
            // 无效（键盘焦点仍留在后台目标窗口），需在窗口可见后再激活并聚焦。
            // SAFETY: 本函数由主线程响应热键消息调用，进程刚收到输入，SetForegroundWindow
            // 允许激活弹窗；返回 BOOL 非错误码，失败（如被系统拦截）时静默忽略。
            unsafe {
                let _ = SetForegroundWindow(hwnd);
            }
            // 聚焦标题编辑框：键盘焦点落到编辑框后用户可直接输入标题（问题 5.1）。
            // SAFETY: GetDlgItem 按子控件 ID 查询弹窗子控件，失败返回 Err 被忽略。
            if let Ok(title_edit) = unsafe { GetDlgItem(hwnd, IDC_TITLE_EDIT) } {
                // SAFETY: title_edit 为弹窗子控件句柄，窗口存活期间有效；SetFocus 失败
                // 仅返回 Err，忽略即可（WM_CREATE 内仍有 SetFocus 兜底）。
                unsafe {
                    let _ = SetFocus(title_edit);
                }
            }
        }
        Err(e) => {
            // SAFETY: CreateWindowExW 失败时窗口未接管 data_ptr，所有权仍在本函数；
            // 重建 Box 释放内存，防止泄漏。
            unsafe {
                drop(Box::from_raw(data_ptr));
            }
            eprintln!("创建弹窗失败: {e}");
        }
    }
}

/// 将弹窗位置钳制到光标附近 + 所在显示器工作区内（R19）
///
/// 首选光标右下偏移 (16, 16)；越出工作区右/下边界时向左/上收回，
/// 负坐标时贴工作区左/上边缘。
fn clamp_to_work(x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    // SAFETY: MonitorFromPoint 为只读查询，取不到时返回 NULL 由下方回退处理。
    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    if hmon.is_invalid() {
        return (x, y);
    }
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi 为栈上缓冲且 cbSize 已填，GetMonitorInfoW 只读填充。
    if !unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
        return (x, y);
    }
    let work = mi.rcWork;
    let mut nx = x;
    let mut ny = y;
    if nx + w > work.right {
        nx = work.right - w;
    }
    if ny + h > work.bottom {
        ny = work.bottom - h;
    }
    if nx < work.left {
        nx = work.left;
    }
    if ny < work.top {
        ny = work.top;
    }
    (nx, ny)
}

extern "system" fn popup_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            // SAFETY: lParam 指向 WM_CREATE 的 CREATESTRUCTW，其 lpCreateParams 由
            // create_popup 传入 Box<PopupData> 原始指针，窗口销毁（WM_DESTROY）前始终有效。
            let data = unsafe {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                cs.lpCreateParams as *mut PopupData
            };
            // SAFETY: data 在窗口生命周期内有效（见上）；set_userdata 由 common 封装，
            // 仅在主线程消息循环内调用。
            unsafe {
                set_userdata(hwnd, data as *mut std::ffi::c_void);
            }

            // 统一主题管理器（D17）：解析全局设置 → 写入全局调色板 → 取得暗色判定。
            // 需在子控件创建前完成：控件首次绘制即发 WM_CTLCOLOR* 请求配色，须先写入
            // 全局调色板保证画刷取色有值（DWM/控件主题变体在子控件创建后应用）。
            let theme_ctx = crate::ui::theme::sync_window_theme();
            let dark = theme_ctx.dark;
            // SAFETY: hwnd 为正在创建的弹窗窗口（WM_CREATE 期间有效）；
            // DWM 属性调用失败（如 Win10 不支持圆角属性）时静默忽略返回值。
            let _ = crate::ui::theme::apply_dark_mode(hwnd, dark);
            let _ = crate::ui::theme::apply_corner_preference(hwnd, theme_ctx.corner);

            // SAFETY: data 指针有效（见上），借用弹窗数据创建子控件与预填编辑框。
            let pd = unsafe { &*data };

            let instance = HINSTANCE::default();
            let child_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0);

            // 已有标签预填：查询目标窗口已存在的标签，有则用其 title/note 初始化编辑框
            // （title 为空时回退窗口标题）
            let (title_default, note_default) = match pd
                .tag_store
                .lock()
                .ok()
                .and_then(|s| s.get(&pd.target_hwnd).cloned())
            {
                Some(t) => {
                    let title = if t.title.is_empty() {
                        pd.window_title.clone()
                    } else {
                        t.title
                    };
                    (title, t.note)
                }
                None => (pd.window_title.clone(), String::new()),
            };

            // —— DPI 缩放后的布局坐标 ——
            // 窗口自身宽高同样经 dp 缩放（WM_GETMINMAXINFO 锁定的就是
            // dp(WIN_W)×dp(WIN_H)），子控件坐标必须与之同基准，否则非 100%
            // DPI 下控件会溢出窗口右缘/底缘。
            let win_w = dp(hwnd, WIN_W);
            let m = dp(hwnd, MARGIN);
            let label_w = dp(hwnd, LABEL_W);
            let ctrl_h = dp(hwnd, CTRL_H);
            let btn_w = dp(hwnd, BTN_W);
            let btn_h = dp(hwnd, BTN_H);
            let btn_gap = dp(hwnd, BTN_GAP);
            let info_h = dp(hwnd, INFO_H);
            let title_row_y = dp(hwnd, TITLE_ROW_Y);
            let color_row_y = dp(hwnd, COLOR_ROW_Y);
            let note_row_y = dp(hwnd, NOTE_ROW_Y);
            let chip_w = dp(hwnd, CHIP_W);
            let chip_h = dp(hwnd, CHIP_H);
            let chip_gap = dp(hwnd, CHIP_GAP);
            let edit_x = m + label_w;
            let edit_w = win_w - m - edit_x;
            let client_h = dp(hwnd, WIN_H) - dp(hwnd, 30); // 减去标题栏近似高度
            let btn_row_y = client_h - m - btn_h;
            // 备注编辑框顶 = 备注标签行下方（标签行高 + 4px 间距），底 = 按钮行
            // 上方（留 btn_gap 间距）。原实现漏减标签行高，编辑框下探遮挡按钮行。
            let note_top = note_row_y + ctrl_h + dp(hwnd, 4);
            let note_h = btn_row_y - btn_gap - note_top;

            // —— 信息行：窗口 + 进程合并为一行 muted 小字（问题 10）——
            let info = format!("窗口：{} · 进程：{}", pd.window_title, pd.process_name);
            let info_wide = widestring(&info);
            // SAFETY: info_wide 为 NUL 结尾宽字符串且存活于调用期间；CreateWindowExW 为
            // 线程安全标准 API，返回值忽略（静态文本控件创建失败不影响弹窗功能）。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    PCWSTR(info_wide.as_ptr()),
                    child_style,
                    m,
                    m,
                    win_w - 2 * m,
                    info_h,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }

            // —— 标题行：标签 + 编辑框同排（问题 10：原"挤成纵排"修正）——
            // SAFETY: 静态标签创建失败忽略。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("标题："),
                    child_style,
                    m,
                    title_row_y,
                    label_w,
                    ctrl_h,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }

            // 标题编辑框（预填已有标签标题，为空则回退窗口标题）
            let title_ws = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | ES_AUTOHSCROLL as u32);
            let title_wide = widestring(&title_default);
            // SAFETY: title_wide 为 NUL 结尾宽字符串且存活于调用期间；失败返回 Err 由调用方处理。
            let title_edit = unsafe {
                CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    PCWSTR(title_wide.as_ptr()),
                    title_ws,
                    edit_x,
                    title_row_y,
                    edit_w,
                    ctrl_h,
                    hwnd,
                    HMENU(IDC_TITLE_EDIT as *mut std::ffi::c_void),
                    instance,
                    None,
                )
            };
            if let Ok(title_edit) = title_edit {
                // 子类化标题编辑框：回车/ESC 键转发给弹窗父窗口处理（问题 5.2）
                // SAFETY: title_edit 为刚创建成功的有效子控件句柄，子类化仅替换
                // 实例窗口过程，无额外内存操作（由安全函数内部封装）。
                subclass_edit_for_keys(title_edit);
                // SAFETY: title_edit 为刚创建成功的有效子控件句柄；SetFocus 在窗口未
                // 激活时无效（create_popup 已补 SetForegroundWindow + SetFocus 兜底），
                // 失败仅返回 Err，忽略即可（不影响弹窗功能）。
                unsafe {
                    let _ = SetFocus(title_edit);
                }
            }

            // —— 颜色行：标签 + 5 个色块（R16），编辑已有标签时预选原颜色 ——
            // SAFETY: 静态标签创建失败忽略。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("颜色："),
                    child_style,
                    m,
                    color_row_y,
                    label_w,
                    ctrl_h,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }
            // 色块：BS_OWNERDRAW 按钮纯色绘制（WM_DRAWITEM 拦截自绘），
            // 点击经标准 WM_COMMAND/BN_CLICKED 路由更新选中色
            for (i, _) in COLOR_CHOICES.iter().enumerate() {
                let chip_x = edit_x + (chip_w + chip_gap) * i as i32;
                // SAFETY: 色块为标准子控件创建，失败忽略（仅少了该色块选项）。
                unsafe {
                    let _ = CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        windows::core::w!("BUTTON"),
                        windows::core::w!(""),
                        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | BS_OWNERDRAW as u32),
                        chip_x,
                        color_row_y + (ctrl_h - chip_h) / 2,
                        chip_w,
                        chip_h,
                        hwnd,
                        HMENU((IDC_COLOR_BASE + i as i32) as *mut std::ffi::c_void),
                        instance,
                        None,
                    );
                }
            }

            // —— 备注行：标签 + 多行编辑框占主体 ——
            // SAFETY: 静态标签创建失败忽略。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("备注："),
                    child_style,
                    m,
                    note_row_y,
                    label_w,
                    ctrl_h,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }

            // 备注编辑框（预填已有标签备注）
            let note_ws = WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_VSCROLL.0
                    | ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32,
            );
            let note_wide = widestring(&note_default);
            // SAFETY: note_wide 为 NUL 结尾宽字符串且存活于调用期间；备注控件创建失败忽略。
            let note_edit = unsafe {
                CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    PCWSTR(note_wide.as_ptr()),
                    note_ws,
                    m,
                    note_top,
                    win_w - 2 * m,
                    note_h,
                    hwnd,
                    HMENU(IDC_NOTE_EDIT as *mut std::ffi::c_void),
                    instance,
                    None,
                )
            };
            if let Ok(note_edit) = note_edit {
                // 子类化备注编辑框：ESC 键转发给弹窗父窗口处理（问题 5.2）；
                // 回车键留给多行编辑框自身插入换行，不转发。
                // SAFETY: note_edit 为刚创建成功的有效子控件句柄，子类化仅替换
                // 实例窗口过程，无额外内存操作（由安全函数内部封装）。
                subclass_edit_for_keys(note_edit);
            }

            // —— 按钮行：右下 确认（accent）/取消（次要），自绘（9.4/9.8）——
            let btn_row_x = win_w - m - (btn_w * 2 + btn_gap);
            // SAFETY: create_button 内部注册状态并子类化；失败返回 Err，忽略即可
            // （按钮不可用不影响弹窗其余功能，WM_COMMAND 仍走原 ID 路由）。
            let _ = button::create_button(
                hwnd,
                IDC_OK_BUTTON,
                "确认",
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

            // 全局消息字体注入所有子控件（EDIT/STATIC；按钮由 BS_OWNERDRAW
            // 自绘时经 message_font() 选择字体）
            apply_font_to_children(hwnd);

            // 子控件 comctl32 主题变体（D17）：备注框滚动条随暗色主题渲染
            crate::ui::theme::apply_control_theme(hwnd, dark);

            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            // 固定弹窗尺寸（问题 9.6）：禁止缩放导致控件错乱
            // SAFETY: lParam 指向 MINMAXINFO，WM_GETMINMAXINFO 期间有效；
            // 覆盖 ptMaxSize 与 ptMinTrackSize/ptMaxTrackSize 锁定尺寸。
            use windows::Win32::UI::WindowsAndMessaging::MINMAXINFO;
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
            // 颜色色块自绘（R16）：纯色圆角矩形，选中项加描边环；
            // 其余自绘按钮（确认/取消）委托 ui::button 处理
            // SAFETY: WM_DRAWITEM 的 lParam 指向 DRAWITEMSTRUCT，生命周期覆盖消息处理。
            let dis = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            let chip_index = dis.CtlID as i32 - IDC_COLOR_BASE;
            if dis.CtlType == ODT_BUTTON && (0..COLOR_CHOICES.len() as i32).contains(&chip_index) {
                // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
                let data = unsafe { get_userdata::<PopupData>(hwnd) };
                let selected = !data.is_null()
                    && (unsafe { (*data).selected_color }) == COLOR_CHOICES[chip_index as usize];
                draw_color_chip(dis, selected);
                LRESULT(1)
            } else if button::handle_draw_item(lparam) {
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
            let data = unsafe { get_userdata::<PopupData>(hwnd) };
            if data.is_null() {
                // SAFETY: DefWindowProcW 为默认窗口过程，其余消息原样透传。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }

            match id {
                IDC_OK_BUTTON => save_and_close(hwnd),
                IDC_CANCEL_BUTTON => cancel_and_close(hwnd, data),
                // 颜色色块点击（R16）：更新选中色并整窗重绘色块行
                id if (IDC_COLOR_BASE..IDC_COLOR_BASE + COLOR_CHOICES.len() as i32)
                    .contains(&id) =>
                {
                    // SAFETY: data 已校验非空；写入弹窗用户数据字段。
                    unsafe {
                        (*data).selected_color = COLOR_CHOICES[(id - IDC_COLOR_BASE) as usize];
                    }
                    // SAFETY: InvalidateRect 标记整窗重绘（色块行随 WM_DRAWITEM 刷新）。
                    unsafe {
                        let _ = InvalidateRect(hwnd, None, FALSE);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            // 键盘按键（问题 5.2）：焦点在弹窗本体时按键直接到达本分支；
            // 焦点在标题/备注编辑框内时，由子类化过程（见 subclass_edit_for_keys）
            // 将回车/ESC 转发为 WM_KEYDOWN 送达本分支，行为与按钮点击等价。
            let key = (wparam.0 & 0xFFFF) as u16;
            // 虚拟键码常量（结构体字段访问不能直接作 match 模式，先绑定为常量）
            const VK_RETURN_CODE: u16 = VK_RETURN.0;
            const VK_ESCAPE_CODE: u16 = VK_ESCAPE.0;
            const VK_TAB_CODE: u16 = VK_TAB.0;
            match key {
                // 回车：与点击“确认”按钮等价的保存并关闭
                VK_RETURN_CODE => save_and_close(hwnd),
                // ESC：与点击“取消”按钮等价的取消并关闭
                VK_ESCAPE_CODE => {
                    // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
                    let data = unsafe { get_userdata::<PopupData>(hwnd) };
                    if !data.is_null() {
                        cancel_and_close(hwnd, data);
                    }
                }
                // Tab / Shift+Tab：在标题/备注/确认/取消间循环切换键盘焦点（R7）。
                // 焦点在编辑框内时由子类化过程转发到达，焦点在按钮上时由
                // button.rs 的子类化过程转发到达。
                VK_TAB_CODE => {
                    // SAFETY: GetKeyState 查询虚拟键状态，无失败路径。
                    let shift_down = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                    focus_next_control(hwnd, !shift_down);
                }
                _ => {}
            }
            LRESULT(0)
        }
        // WM_ERASEBKGND：窗口自身客户区背景擦除
        //
        // WM_CTLCOLOR* 只处理子控件（编辑框/静态文本/按钮）的配色，窗口客户区的
        // 背景由 WM_ERASEBKGND 决定；默认 DefWindowProc 用窗口类白色画刷擦除，
        // 导致暗色主题下弹窗主体（标题/备注文本以外的空白区域）仍为白色。
        // 此处按主题背景色填充客户区并返回 1（表示背景已擦除），阻止系统默认
        // 白色填充（任务 T8）。
        WM_ERASEBKGND => {
            // 主题状态未初始化（从未调用 set_theme）时回退系统默认绘制
            let Some(colors) = crate::ui::theme::theme_colors() else {
                // SAFETY: DefWindowProcW 将未处理的 WM_ERASEBKGND 原样透传给系统
                // 默认窗口过程，参数与消息上下文一致，无额外内存操作。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            };
            // SAFETY: GetClientRect 写入栈上 RECT（调用期间存活），hwnd 为本窗口
            // 有效句柄；失败（理论不可达）时回退 DefWindowProcW 走系统默认绘制。
            let mut rc = RECT::default();
            if unsafe { GetClientRect(hwnd, &mut rc) }.is_err() {
                // SAFETY: 同上方 DefWindowProcW 回退，参数与消息上下文一致。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            // SAFETY: wParam 携带窗口客户区 HDC，仅在消息处理期间有效；画刷经
            // get_brush 进程级缓存持有、进程生命周期内不销毁，FillRect 同步填充
            // 后立即返回，无跨消息生命周期。
            let hdc = HDC(wparam.0 as *mut std::ffi::c_void);
            unsafe {
                FillRect(hdc, &rc, crate::ui::theme::get_brush(colors.bg));
            }
            // 返回 1 表示背景已擦除，系统不再用默认画刷填充
            LRESULT(1)
        }
        // WM_CTLCOLOR*：子控件（标题/备注编辑框 EDIT / 静态文本 / 按钮）重绘前
        // 向父窗口请求配色，统一按当前主题调色板着色（任务 T7）。
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            handle_ctlcolor(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            // 覆盖层创建已移至确认分支（标签写入成功后），取消/关闭时无需销毁覆盖层；
            // 直接关闭弹窗，数据释放由 WM_DESTROY 分支负责。
            // SAFETY: hwnd 为本窗口有效句柄，DestroyWindow 触发 WM_DESTROY 释放数据。
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // 清理活动弹窗注册表（本弹窗销毁，单例复用依据失效）
            if let Ok(mut active) = ACTIVE_POPUP.lock() {
                if active.is_some_and(|(_, h)| h == hwnd.0 as isize) {
                    *active = None;
                }
            }
            // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
            let data = unsafe { get_userdata::<PopupData>(hwnd) };
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

/// 执行“确认”保存并关闭弹窗（确认按钮与回车键共用，问题 5.2）
///
/// 读取标题/备注编辑框内容构造 [`Tag`]（颜色固定 `TagColor::Orange`，颜色选择
/// 不在本任务范围），写入全局标签存储；写入成功后向隐藏窗口发送
/// `WM_CREATE_OVERLAY` 请求创建覆盖层，最后销毁弹窗（`WM_DESTROY` 释放数据）。
fn save_and_close(hwnd: HWND) {
    // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用；
    // 返回指针在 WM_DESTROY 释放前有效。
    let data = unsafe { get_userdata::<PopupData>(hwnd) };
    if data.is_null() {
        return;
    }

    // SAFETY: GetDlgItem 查询本窗口子控件，失败返回 Err 由调用方处理。
    let title_hwnd = unsafe { GetDlgItem(hwnd, IDC_TITLE_EDIT) };
    if let Ok(th) = title_hwnd {
        // 动态长度读取（原固定 256/1024 u16 栈缓冲会静默丢弃超长内容）
        // SAFETY: th 为本弹窗存活子控件句柄。
        let title = unsafe { read_window_text(th) }.trim().to_string();

        // SAFETY: GetDlgItem 查询本窗口子控件，失败返回 Err 由调用方处理。
        let note_hwnd = unsafe { GetDlgItem(hwnd, IDC_NOTE_EDIT) };
        let note = if let Ok(nh) = note_hwnd {
            // SAFETY: nh 为本弹窗存活子控件句柄。
            unsafe { read_window_text(nh) }.trim().to_string()
        } else {
            String::new()
        };

        let tag = Tag {
            title: if title.is_empty() {
                // SAFETY: data 指针有效（见函数开头），字段为 String 可克隆。
                unsafe { (*data).window_title.clone() }
            } else {
                title
            },
            note,
            // 选中颜色（R16 色块行），不再固定橙色
            color: unsafe { (*data).selected_color },
            window_title: unsafe { (*data).window_title.clone() },
            process_name: unsafe { (*data).process_name.clone() },
        };

        // SAFETY: data 指针有效；tag_store 为 Arc<Mutex>，lock() 失败
        // （毒锁）时跳过写入并标记未保存，避免向主线程请求创建覆盖层。
        let mut saved = false;
        if let Ok(mut store) = unsafe { (*data).tag_store.lock() } {
            matcher::upsert_tag(&mut store, unsafe { (*data).target_hwnd }, tag);
            saved = true;
        }
        if saved {
            // SAFETY: hidden_hwnd 为主线程创建的隐藏窗口句柄，PostMessageW 为
            // 线程安全标准 API；标签写入成功后请求主线程为目标窗口创建覆盖层。
            unsafe {
                let _ = PostMessageW(
                    HWND((*data).hidden_hwnd as *mut std::ffi::c_void),
                    common::WM_CREATE_OVERLAY,
                    WPARAM((*data).target_hwnd as usize),
                    LPARAM(0),
                );
                // SAFETY: 同上；广播标签数据变更，主线程转发给概览面板刷新树形列表。
                let _ = PostMessageW(
                    HWND((*data).hidden_hwnd as *mut std::ffi::c_void),
                    common::WM_APP_TAGS_CHANGED,
                    WPARAM((*data).target_hwnd as usize),
                    LPARAM(0),
                );
            }
            println!("已标记窗口：{}", unsafe { &(*data).window_title });
        } else {
            eprintln!("标签存储锁中毒，标记未保存");
        }
    }
    // SAFETY: hwnd 为本窗口有效句柄，DestroyWindow 触发 WM_DESTROY 释放数据。
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// 执行“取消”关闭弹窗（取消按钮与 ESC 键共用，问题 5.2）
///
/// 打印取消日志后走 `WM_CLOSE` 单一路径关闭弹窗（覆盖层无需销毁，见 WM_CLOSE）。
///
/// # 参数
///
/// - `hwnd`：弹窗窗口句柄
/// - `data`：弹窗用户数据指针（由调用方保证非空且在窗口生命周期内有效）
fn cancel_and_close(hwnd: HWND, data: *mut PopupData) {
    // SAFETY: data 由调用方保证非空且在窗口生命周期内有效（WM_DESTROY 前），
    // 此处仅读取 window_title 字段。
    println!("取消标记窗口：{}", unsafe { &(*data).window_title });
    // SAFETY: 取消统一走 WM_CLOSE 单一路径关闭弹窗（覆盖层无需销毁，见 WM_CLOSE）。
    unsafe {
        let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// 在弹窗子控件间循环切换键盘焦点（Tab 正向 / Shift+Tab 反向，R7）
///
/// 按 [`FOCUS_ORDER`] 顺序从当前焦点控件取下一个；焦点不在已知控件上
/// （异常路径）时落到第一个控件（标题框）。
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

/// 将 EDIT 子控件子类化：把回车/ESC/TAB 键转发给父弹窗处理（问题 5.2/R7/R14）
///
/// 键盘消息（`WM_KEYDOWN`）只会投递给拥有键盘焦点的窗口——即编辑框本身；
/// 若不子类化，父弹窗收不到回车/ESC/TAB，`WM_KEYDOWN` 分支无法触发保存/取消/
/// 焦点切换。子类化过程仅做按键转发与判定，其余消息经 `GetClassLongPtrW`
/// (GCLP_WNDPROC) 取回的 EDIT 类原始窗口过程透传，不影响编辑框其他行为。
///
/// # 参数
///
/// - `edit_hwnd`：刚创建成功的 EDIT 子控件句柄（由调用方保证有效）
fn subclass_edit_for_keys(edit_hwnd: HWND) {
    // SAFETY: edit_hwnd 为刚创建成功的子控件；SetWindowLongPtrW 仅替换实例窗口
    // 过程（返回的原过程值无需保留，透传统一走类过程 GCLP_WNDPROC），
    // 无跨线程访问，失败（理论不可达）时静默忽略。函数指针先经 `as *const ()`
    // 再转 isize，避免 function_casts_as_integer 警告。
    unsafe {
        let _ = SetWindowLongPtrW(
            edit_hwnd,
            GWLP_WNDPROC,
            edit_subclass_proc as *const () as isize,
        );
    }
}

/// 弹窗 EDIT 子控件的子类化窗口过程：转发确认/取消/切焦点键给父弹窗，其余透传
///
/// - 标题编辑框（`IDC_TITLE_EDIT`）任意回车 → 转发父弹窗保存；
/// - 备注编辑框（多行）：裸回车 → 转发父弹窗保存；Shift+回车透传插入换行；
/// - 任一编辑框 ESC → 转发父弹窗取消，TAB → 转发父弹窗循环切换焦点；
/// - 其余消息调用 EDIT 类原始窗口过程处理。
unsafe extern "system" fn edit_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        let key = (wparam.0 & 0xFFFF) as u16;
        // Ctrl+A：标准 Win32 EDIT 控件默认不支持 Ctrl+A 全选（只支持
        // Shift+方向键等选择方式），故在此子类化过程中拦截实现（任务 T8）。
        if key == VK_A.0 {
            // SAFETY: GetKeyState 查询虚拟键状态，返回 i16 最高位为 1 表示按下
            //（即负值）；无失败路径，可在任意线程调用。
            let ctrl_down = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            if ctrl_down {
                // SAFETY: hwnd 为本编辑框有效句柄；EM_SETSEL 全选：wParam=起始
                // 位置 0，lParam=-1 表示选中到文本末尾；SendMessageW 为线程安全
                // 标准 API，仅修改本控件选区状态。
                unsafe {
                    let _ = SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
                }
                // 拦截该按键，不再透传给 EDIT 原始窗口过程（防止输入字符 'a'）
                return LRESULT(0);
            }
        }
        // SAFETY: GetDlgCtrlID 查询子控件 ID（创建时经 HMENU 传入）。
        let is_title = unsafe { GetDlgCtrlID(hwnd) } == IDC_TITLE_EDIT;
        // 转发判定：
        // - 标题框回车 → 保存；备注框裸回车 → 保存（Shift+回车透传换行）；
        // - ESC 取消 / TAB 切换焦点：两框均转发父弹窗统一处理。
        let forward = if key == VK_RETURN.0 {
            if is_title {
                true
            } else {
                // SAFETY: GetKeyState 查询虚拟键状态，最高位为 1（负值）表示按下。
                (unsafe { GetKeyState(VK_SHIFT.0 as i32) }) >= 0
            }
        } else {
            key == VK_ESCAPE.0 || key == VK_TAB.0
        };
        if forward {
            // SAFETY: GetParent 返回创建时指定的弹窗父窗口；PostMessageW 为线程安全
            // 标准 API，异步投递避免在子控件窗口过程内同步销毁窗口（WM_DESTROY 链）。
            if let Ok(parent) = unsafe { GetParent(hwnd) } {
                unsafe {
                    let _ = PostMessageW(parent, WM_KEYDOWN, wparam, lparam);
                }
            }
            return LRESULT(0);
        }
    }
    // 其余消息透传 EDIT 类原始窗口过程（子类化仅替换实例过程，类过程不变）
    // SAFETY: GetClassLongPtrW(GCLP_WNDPROC) 返回 EDIT 类默认窗口过程，签名与
    // 窗口过程一致；transmute 为函数指针↔整数往返转换，Windows ABI 下良定义。
    let orig = unsafe { GetClassLongPtrW(hwnd, GCLP_WNDPROC) };
    let orig_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        unsafe { std::mem::transmute(orig) };
    orig_proc(hwnd, msg, wparam, lparam)
}

/// 处理 `WM_CTLCOLOR*` 消息：按主题调色板为子控件设置文字色与背景色（任务 T7）
///
/// - `WM_CTLCOLOREDIT`：标题/备注编辑框（EDIT），使用编辑框专用前景/背景色；
/// - `WM_CTLCOLORSTATIC`：静态文本，使用窗口前景/背景色；
/// - `WM_CTLCOLORBTN`：按钮，使用窗口前景/背景色。
///
/// 颜色取自 [`crate::ui::theme::theme_colors`]；主题状态未初始化（返回 `None`）时
/// 回退 [`DefWindowProcW`] 走系统默认配色。
fn handle_ctlcolor(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 主题状态未初始化（从未调用 set_theme）时回退系统默认绘制
    let Some(colors) = crate::ui::theme::theme_colors() else {
        // SAFETY: DefWindowProcW 将未处理的 WM_CTLCOLOR* 原样透传给系统默认
        // 窗口过程，参数与消息上下文一致，无额外内存操作。
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };

    let (fg, bg) = match msg {
        WM_CTLCOLOREDIT => (colors.edit_fg, colors.edit_bg),
        // WM_CTLCOLORSTATIC / WM_CTLCOLORBTN：静态文本与按钮按窗口前景/背景着色
        _ => (colors.fg, colors.bg),
    };

    // SAFETY: wParam 携带子控件本次绘制使用的 HDC，仅在消息处理期间有效；
    // SetTextColor/SetBkColor 只修改该 DC 的当前文本/背景状态，无跨消息生命周期。
    let hdc = HDC(wparam.0 as *mut std::ffi::c_void);
    unsafe {
        let _ = SetTextColor(hdc, fg);
        let _ = SetBkColor(hdc, bg);
    }
    // 返回背景色画刷句柄：控件以此绘制客户区背景。画刷经 get_brush 进程级
    // 缓存持有、进程生命周期内不销毁，可安全作为 LRESULT 返回。
    let brush = crate::ui::theme::get_brush(bg);
    LRESULT(brush.0 as isize)
}

/// 读取窗口文本为 String（按 `GetWindowTextLengthW` 动态分配，避免静默截断）
///
/// 取代原先 256/1024 定长栈缓冲——超长标题/备注会被无声丢弃，用户毫无感知。
///
/// # Safety
///
/// `hwnd` 须为存活窗口句柄（本弹窗子控件，消息处理期间有效）。
unsafe fn read_window_text(hwnd: HWND) -> String {
    // SAFETY: hwnd 由调用方保证存活；GetWindowTextLengthW 为只读查询。
    let len = unsafe { GetWindowTextLengthW(hwnd) } as usize;
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len + 1];
    // SAFETY: buf 长度 len+1 覆盖文本与 NUL 终止符，无越界风险。
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// 绘制颜色色块（R16）：填充圆角矩形；选中项加主题前景色描边环，聚焦画焦点框
fn draw_color_chip(dis: &DRAWITEMSTRUCT, selected: bool) {
    let color = COLOR_CHOICES[(dis.CtlID as i32 - IDC_COLOR_BASE) as usize];
    let [r, g, b, _] = color.as_rgba();
    let theme = crate::ui::theme::theme_colors();
    // 选中：2px 主题前景色描边环；未选中：1px 主题描边色弱框
    let pen_color = if selected {
        theme.map(|c| c.fg).unwrap_or(COLORREF(0x00FFFFFF))
    } else {
        theme.map(|c| c.border).unwrap_or(COLORREF(0x00808080))
    };
    let pen_width = if selected { 2 } else { 1 };

    // SAFETY: DRAWITEMSTRUCT.hDC 由系统在绘制期间提供，仅本函数内使用；
    // 画刷/画笔在本函数内成对创建、恢复、删除。
    unsafe {
        let brush = CreateSolidBrush(COLORREF((r as u32) | (g as u32) << 8 | (b as u32) << 16));
        let pen = CreatePen(PS_SOLID, pen_width, pen_color);
        let old_pen = SelectObject(dis.hDC, pen);
        let old_brush = SelectObject(dis.hDC, brush);
        let rc = dis.rcItem;
        let _ = RoundRect(dis.hDC, rc.left, rc.top, rc.right, rc.bottom, 6, 6);
        let _ = SelectObject(dis.hDC, old_brush);
        let _ = SelectObject(dis.hDC, old_pen);
        let _ = DeleteObject(brush);
        let _ = DeleteObject(pen);
        if (dis.itemState.0 & ODS_FOCUS.0) != 0 {
            let focus = RECT {
                left: rc.left + 2,
                top: rc.top + 2,
                right: rc.right - 2,
                bottom: rc.bottom - 2,
            };
            let _ = DrawFocusRect(dis.hDC, &focus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 颜色候选：5 个变体的 RGBA 互不相同（R16 色块行选择依据）
    #[test]
    fn color_choices_are_distinct() {
        assert_eq!(COLOR_CHOICES.len(), 5);
        let mut rgba: Vec<_> = COLOR_CHOICES.iter().map(|c| c.as_rgba()).collect();
        rgba.sort();
        rgba.dedup();
        assert_eq!(rgba.len(), 5);
    }

    /// 色块 ID 区间与候选数量一致（WM_COMMAND 路由的边界）
    #[test]
    fn color_chip_id_range_matches_choices() {
        let last = IDC_COLOR_BASE + COLOR_CHOICES.len() as i32 - 1;
        assert!(
            (IDC_COLOR_BASE..=last).contains(&(IDC_COLOR_BASE + 4)),
            "第 5 个色块 ID 应落在路由区间内"
        );
        assert!(!(IDC_COLOR_BASE..=last).contains(&(last + 1)));
    }
}
