use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, ScreenToClient, SetBkColor, SetTextColor, HDC};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, HTREEITEM, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, NMHDR, NM_CLICK,
    NM_DBLCLK, TVE_EXPAND, TVGN_NEXT, TVGN_PARENT, TVGN_ROOT, TVHITTESTINFO, TVHT_ONITEM,
    TVHT_ONITEMBUTTON, TVIF_HANDLE, TVIF_PARAM, TVIF_STATE, TVIF_TEXT, TVINSERTSTRUCTW,
    TVINSERTSTRUCTW_0, TVIS_EXPANDED, TVITEMW, TVI_LAST, TVI_ROOT, TVM_DELETEITEM, TVM_EXPAND,
    TVM_GETITEMW, TVM_GETNEXTITEM, TVM_HITTEST, TVM_INSERTITEMW, TVM_SETBKCOLOR, TVM_SETLINECOLOR,
    TVM_SETTEXTCOLOR, TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT, TVS_SHOWSELALWAYS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, GetDlgItem, GetWindowTextW,
    IsIconic, IsWindow, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos,
    ShowWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, EN_CHANGE, ES_AUTOHSCROLL, HWND_TOP,
    MINMAXINFO, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_RESTORE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_NOTIFY,
    WM_SIZE, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::common::{get_userdata, set_userdata, widestring, WM_APP_TAGS_CHANGED};
use crate::core::settings::ThemeMode;
use crate::core::tag::TagStore;
use crate::ui::layout::dp;
use crate::ui::theme::{apply_font_to_children, theme_colors};

const IDC_SEARCH_EDIT: i32 = 201;
const IDC_LIST_VIEW: i32 = 202;

/// 设计像素常量（96 DPI 基准），运行时经 [`dp`] 缩放为物理像素
const MARGIN: i32 = 12;
const SEARCH_H: i32 = 28;
const SEARCH_GAP: i32 = 8;
const WIN_W: i32 = 400;
const WIN_H: i32 = 640;
/// 最小宽度：列表宽度随窗口收缩仍可读，允许拖窄到紧凑列表形态
const MIN_W: i32 = 300;
const MIN_H: i32 = 360;

/// 面板窗口的用户数据：标签存储引用与当前可见状态
///
/// 通过 `GWLP_USERDATA` 关联到面板窗口，供面板 WndProc 及辅助函数在
/// 消息处理期间取回。窗口销毁（`WM_DESTROY`）时由 `Box::from_raw` 回收。
pub struct PanelData {
    /// 全局共享的标签存储（窗口句柄 → 标签）
    pub tag_store: Arc<Mutex<TagStore>>,
    /// 面板当前是否可见
    pub visible: bool,
}

/// 创建概览面板窗口（初始隐藏）
///
/// 注册 `WinTagPanel` 窗口类并调用 `CreateWindowExW` 创建面板，
/// 面板数据（[`PanelData`]）通过 `lpCreateParams` 传递，由 `WM_CREATE`
/// 写入窗口用户数据。窗口初始隐藏，由 [`toggle_panel`] 控制显隐。
///
/// # 返回值
///
/// 成功时返回面板窗口句柄；创建失败时打印错误信息并返回默认（NULL）句柄，
/// 调用方应先检查返回值再使用。
pub fn create_panel(data: Arc<Mutex<TagStore>>) -> HWND {
    let panel_data = Box::new(PanelData {
        tag_store: data,
        visible: false,
    });
    let data_ptr = Box::into_raw(panel_data);

    let class_name = widestring("WinTagPanel");

    let wc = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(panel_wndproc),
        hInstance: windows::Win32::Foundation::HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    // SAFETY: RegisterClassW 注册窗口类，返回已有或新注册结果均被忽略；
    // 窗口类名固定为 "WinTagPanel"，WndProc 与类名一一对应，重复注册幂等。
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    // 初始化通用控件
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_TREEVIEW_CLASSES,
    };
    // SAFETY: InitCommonControlsEx 使用栈上 INITCOMMONCONTROLSEX，调用期间有效；
    // 返回值忽略，控件类未加载时后续创建会失败并在对应分支打印告警。
    unsafe {
        let _ = InitCommonControlsEx(&icc);
    }

    let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0); // 初始隐藏，由 toggle_panel 控制
    let panel = unsafe {
        // SAFETY: class_name 与窗口标题为本地 NUL 结尾宽字符串，调用期间有效；
        // data_ptr 所有权随 lpCreateParams 转移给 WM_CREATE（写入 GWLP_USERDATA），
        // 创建失败时由下方 Err 分支回收，避免 Box 泄漏。
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("WinTag - 概览面板"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIN_W,
            WIN_H,
            None,
            None,
            None,
            Some(data_ptr as *const std::ffi::c_void),
        )
    };

    match panel {
        Ok(panel) => panel,
        Err(e) => {
            eprintln!("创建面板窗口失败: {e}");
            // SAFETY: data_ptr 由 Box::into_raw 产生且窗口创建失败，所有权未转移
            // 给任何 WndProc（WM_CREATE 未执行），在此回收防止内存泄漏。
            unsafe {
                drop(Box::from_raw(data_ptr));
            }
            HWND::default()
        }
    }
}

extern "system" fn panel_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            // SAFETY: WM_CREATE 的 lParam 指向 CREATESTRUCTW，其生命周期覆盖整个
            // 消息处理过程；lpCreateParams 为 create_panel 传入的 PanelData 指针。
            let data = unsafe {
                let cs =
                    &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
                cs.lpCreateParams as *mut PanelData
            };
            // SAFETY: data 指针来自 create_panel，窗口生命周期内始终有效；
            // 写入 GWLP_USERDATA 供后续消息取回，WM_DESTROY 时统一回收。
            unsafe {
                set_userdata(hwnd, data as *mut std::ffi::c_void);
            }

            // 读取全局设置并应用暗色主题与圆角偏好
            // global_settings 未注入（如独立测试环境）时回退默认设置（跟随系统 + 默认圆角）
            let settings = crate::core::settings::global_settings()
                .and_then(|s| s.lock().ok().map(|guard| *guard))
                .unwrap_or_default();
            let system_dark = crate::ui::theme::detect_system_dark();
            let colors = crate::ui::theme::resolve_colors(settings.theme, system_dark);
            // 先写入 THEME_STATE，确保后续 WM_CTLCOLOR* 消息能取到当前调色板
            crate::ui::theme::set_theme(colors);
            // 简化暗色判定：显式深色，或跟随系统且系统当前为深色，即视为暗色主题
            let dark = settings.theme == ThemeMode::Dark
                || (settings.theme == ThemeMode::System && system_dark);
            // SAFETY: hwnd 为正在创建的面板窗口（WM_CREATE 期间有效）；
            // DWM 属性调用失败（如 Win10 不支持圆角属性）时静默忽略返回值。
            let _ = crate::ui::theme::apply_dark_mode(hwnd, dark);
            let _ = crate::ui::theme::apply_corner_preference(hwnd, settings.corner);

            let instance = windows::Win32::Foundation::HINSTANCE::default();

            // DPI 缩放后的布局常量
            let m = dp(hwnd, MARGIN);
            let search_h = dp(hwnd, SEARCH_H);
            let list_y = m + search_h + dp(hwnd, SEARCH_GAP);

            // 搜索框
            let search_ws = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | ES_AUTOHSCROLL as u32);
            // SAFETY: 创建 EDIT 子控件（ID = IDC_SEARCH_EDIT）；
            // 失败时忽略，搜索功能不可用但不影响面板主体。
            unsafe {
                let _ = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    windows::core::w!(""),
                    search_ws,
                    m,
                    m,
                    // 宽度占满减左右边距；WM_SIZE 会按实际宽度校正
                    300,
                    search_h,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(
                        IDC_SEARCH_EDIT as *mut std::ffi::c_void,
                    ),
                    instance,
                    None,
                );
            }

            // 可展开树形列表：每个标签一个根项（lParam = 目标窗口句柄），
            // 点击行首 [+] 展开显示备注/窗口/进程详情
            let tree_style = WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | TVS_HASBUTTONS
                    | TVS_HASLINES
                    | TVS_LINESATROOT
                    | TVS_SHOWSELALWAYS,
            );
            let list_view = unsafe {
                // SAFETY: 创建 SysTreeView32 子控件（ID = IDC_LIST_VIEW）；
                // 失败时返回默认句柄并打印告警。
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("SysTreeView32"),
                    windows::core::w!(""),
                    tree_style,
                    m,
                    list_y,
                    // 初始尺寸由 WM_SIZE 校正
                    560,
                    350,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(
                        IDC_LIST_VIEW as *mut std::ffi::c_void,
                    ),
                    instance,
                    None,
                )
            };
            let list_view = match list_view {
                Ok(tv) => tv,
                Err(e) => {
                    eprintln!("创建树形列表失败: {e}");
                    HWND::default()
                }
            };

            // 暗色时设 DarkMode_Explorer 主题（滚动条随之暗化）；亮色恢复 Explorer。
            // 同时按主题调色板设置背景/文字/连线色。需 comctl32 v6 manifest
            // （build.rs 嵌入）才能生效。
            apply_tree_theme(list_view, dark);

            // 首次填充树形列表
            refresh_tree(hwnd);

            // 全局消息字体注入所有子控件（搜索框 + 列表视图）
            apply_font_to_children(hwnd);

            // 首次布局：触发 WM_SIZE 校正搜索框/列表实际尺寸位置
            // SAFETY: GetClientRect 写入栈上 RECT；无副作用。
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            layout_children(hwnd, rc.right - rc.left, rc.bottom - rc.top);

            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            // 限制窗口最小尺寸（问题 9.6），防止缩放到控件不可用
            // SAFETY: lParam 指向 MINMAXINFO，WM_GETMINMAXINFO 期间有效；
            // 仅覆盖 ptMinTrackSize 字段，其余保持系统默认。
            let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            mmi.ptMinTrackSize.x = dp(hwnd, MIN_W);
            mmi.ptMinTrackSize.y = dp(hwnd, MIN_H);
            LRESULT(0)
        }
        WM_SIZE => {
            let width = (lparam.0 & 0xFFFF) as i32;
            let height = ((lparam.0 >> 16) & 0xFFFF) as i32;
            layout_children(hwnd, width, height);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;

            if id == IDC_SEARCH_EDIT && code == EN_CHANGE {
                refresh_tree(hwnd);
            }

            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY 的 lParam 指向通知结构，NM_CLICK/NM_DBLCLK 首字段均为
            // NMHDR，故按 NMHDR 读取 code/idFrom 分流；单击/双击根项 → 置前对应窗口。
            let nm = unsafe { &*(lparam.0 as *const NMHDR) };
            if nm.idFrom == IDC_LIST_VIEW as usize {
                match nm.code {
                    NM_CLICK => handle_tree_click(hwnd),
                    NM_DBLCLK => {
                        handle_tree_click(hwnd);
                        // 返回非 0：抑制树视图双击默认的展开/收起切换
                        return LRESULT(1);
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_APP_TAGS_CHANGED => {
            // 便签弹窗保存标签后广播（经主线程转发）：刷新树形列表，
            // 保留已展开根项的状态，避免自动刷新打断用户的展开浏览
            refresh_tree(hwnd);
            LRESULT(0)
        }
        // WM_CTLCOLOR*：子控件（搜索框 EDIT / SysListView32 列表区 / 静态文本）
        // 重绘前向父窗口请求配色，统一按当前主题调色板着色。
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC => {
            handle_ctlcolor(hwnd, msg, wparam, lparam)
        }
        // WM_ERASEBKGND：窗口自身客户区背景。WM_CTLCOLOR* 只处理子控件配色，
        // 客户区背景由 WM_ERASEBKGND 决定；默认 DefWindowProc 用白色类画刷擦除
        // 背景（暗色主题下表现为白色窗口底色），此处按主题色填充并返回 1
        // 告知系统背景已擦除，阻止默认白色填充。
        WM_ERASEBKGND => {
            // 主题状态未初始化（从未调用 set_theme）时回退系统默认擦除
            let Some(colors) = crate::ui::theme::theme_colors() else {
                // SAFETY: DefWindowProcW 将未处理的 WM_ERASEBKGND 原样透传给系统
                // 默认窗口过程，参数与消息上下文一致，无额外内存操作。
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            };
            // SAFETY: wParam 携带客户区 HDC，仅在消息处理期间有效；
            // FillRect 只填充本次消息对应的客户区矩形，无跨消息生命周期。
            let hdc = HDC(wparam.0 as *mut std::ffi::c_void);
            // SAFETY: GetClientRect 将客户区矩形写入栈上 RECT，调用期间有效。
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            // SAFETY: rc 为栈上局部值，画刷句柄由 get_brush 进程级缓存持有，均有效。
            unsafe {
                let _ = FillRect(hdc, &rc, crate::ui::theme::get_brush(colors.bg));
            }
            // 返回 1 表示背景已擦除，阻止 DefWindowProc 用白色类画刷填充
            LRESULT(1)
        }
        WM_CLOSE => {
            // SAFETY: 面板窗口存活期间 PanelData 有效（WM_DESTROY 才回收）。
            let data = unsafe { get_userdata::<PanelData>(hwnd) };
            if !data.is_null() {
                // SAFETY: data 已校验非空，窗口仍存活。
                unsafe {
                    (*data).visible = false;
                }
            }
            // SAFETY: WM_CLOSE 仅隐藏面板，不销毁窗口，保持 PanelData 存活。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: WM_DESTROY 表示窗口即将销毁，回收此前 Box::into_raw 转移的
            // PanelData，防止内存泄漏。
            let data = unsafe { get_userdata::<PanelData>(hwnd) };
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

/// 处理 `WM_CTLCOLOR*` 消息：按主题调色板为子控件设置文字色与背景色
///
/// - `WM_CTLCOLOREDIT`：搜索框（EDIT）背景；
/// - `WM_CTLCOLORSTATIC`：静态文本背景；
/// - `WM_CTLCOLORLISTBOX`：兼容保留（旧列表视图分支，树形列表改用
///   `TVM_SETBKCOLOR` 着色，不再发送此消息）。
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
        WM_CTLCOLORLISTBOX => (colors.listview_fg, colors.listview_bg),
        // WM_CTLCOLORSTATIC：静态文本按窗口前景/背景着色
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

/// 子控件布局（WM_SIZE / WM_CREATE 末尾统一调用）
///
/// 修正原 WM_SIZE 的 bug（SetWindowPos 缺 SWP_NOMOVE，控件被吸到 (0,0)），
/// 恢复四边 MARGIN 内边距：搜索框顶部对齐、列表占剩余空间。
fn layout_children(hwnd: HWND, width: i32, height: i32) {
    let m = dp(hwnd, MARGIN);
    let search_h = dp(hwnd, SEARCH_H);
    let list_y = m + search_h + dp(hwnd, SEARCH_GAP);
    let content_w = (width - 2 * m).max(1);

    // 搜索框：保持原位置（m, m），仅调整宽度
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    if let Ok(search_edit) = unsafe { GetDlgItem(hwnd, IDC_SEARCH_EDIT) } {
        // SAFETY: SetWindowPos 带 SWP_NOMOVE 保留原位置（m,m），仅改尺寸；
        // 不传 SWP_NOSIZE 即允许改尺寸。SWP_NOZORDER 保留原 Z 序。
        use windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER;
        unsafe {
            let _ = SetWindowPos(
                search_edit,
                HWND_TOP,
                0,
                0,
                content_w,
                search_h,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
            );
        }
    }
    // 列表：占搜索框下方到客户区底边
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    if let Ok(list_view) = unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) } {
        let list_h = (height - list_y - m).max(1);
        // SAFETY: SWP_NOMOVE 保留原位置（m, list_y），仅改尺寸。
        use windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER;
        unsafe {
            let _ = SetWindowPos(
                list_view,
                HWND_TOP,
                0,
                0,
                content_w,
                list_h,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
            );
        }
    }
}

/// 设置树形列表的视觉主题与配色（问题 9.2/9.3/9.5 的树形版）
///
/// 暗色时设 `DarkMode_Explorer`：滚动条与展开按钮随之暗化（Win11 必然生效，
/// Win10 1809+ 生效，更旧降级为纯配色，不撕裂）；亮色恢复 `Explorer`。
/// 同时按主题调色板设置背景 / 文字 / 连线色（替代 ListView 的 NM_CUSTOMDRAW
/// 行级着色：树形列表统一背景 + 选中高亮）。需 comctl32 v6 manifest 才能生效。
fn apply_tree_theme(list_view: HWND, dark: bool) {
    let theme = if dark {
        widestring("DarkMode_Explorer")
    } else {
        widestring("Explorer")
    };
    // SAFETY: SetWindowTheme 为线程安全标准 API；theme 为本函数内 NUL 结尾
    // 宽字符串，调用期间存活；失败（系统不支持）静默忽略，降级为默认外观。
    unsafe {
        let _ = windows::Win32::UI::Controls::SetWindowTheme(
            list_view,
            PCWSTR(theme.as_ptr()),
            PCWSTR::null(),
        );
    }
    let colors = theme_colors().unwrap_or_else(crate::ui::theme::light_colors);
    // SAFETY: 颜色消息参数为值类型，SendMessageW 同步返回后参数生命周期结束。
    unsafe {
        let _ = SendMessageW(
            list_view,
            TVM_SETBKCOLOR,
            WPARAM(0),
            LPARAM(colors.listview_bg.0 as isize),
        );
        let _ = SendMessageW(
            list_view,
            TVM_SETTEXTCOLOR,
            WPARAM(0),
            LPARAM(colors.listview_fg.0 as isize),
        );
        let _ = SendMessageW(
            list_view,
            TVM_SETLINECOLOR,
            WPARAM(0),
            LPARAM(colors.border.0 as isize),
        );
    }
}

/// 处理树形列表单击 / 双击根项：将对应窗口置前（需求：点击置顶对应软件）
///
/// 光标位置经 `TVM_HITTEST` 定位树项；仅根项（无父项，即标签本体）且命中
/// 项本体（图标/文字）时触发置前——命中 [+] 展开按钮时交给树控件处理展开。
fn handle_tree_click(hwnd: HWND) {
    // SAFETY: 面板窗口由 create_panel 创建，窗口存活期间 PanelData 有效。
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() {
        return;
    }
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    let Ok(list_view) = (unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) }) else {
        return;
    };

    // 光标位置 → 树内命中测试（客户端坐标）
    let mut pt = POINT::default();
    // SAFETY: GetCursorPos/ScreenToClient 均为坐标查询，pt 为栈上缓冲。
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = ScreenToClient(list_view, &mut pt);
    }
    let mut ht = TVHITTESTINFO {
        pt,
        ..Default::default()
    };
    // SAFETY: ht 为栈上变量，TVM_HITTEST 返回前完成写入。
    unsafe {
        let _ = SendMessageW(
            list_view,
            TVM_HITTEST,
            WPARAM(0),
            LPARAM(std::ptr::addr_of_mut!(ht) as isize),
        );
    }
    if ht.hItem.0 == 0 {
        return;
    }
    // 命中 [+] 展开按钮：交给树控件切换展开，不触发置前
    if (ht.flags.0 & TVHT_ONITEMBUTTON.0) != 0 {
        return;
    }
    // 仅命中项本体（图标/文字）时触发
    if (ht.flags.0 & TVHT_ONITEM.0) == 0 {
        return;
    }
    // 仅根项（标签本体）触发：有父项的是详情子项
    // SAFETY: hItem 为 TVM_HITTEST 返回的有效句柄，TVM_GETNEXTITEM 只读查询。
    let parent = unsafe {
        SendMessageW(
            list_view,
            TVM_GETNEXTITEM,
            WPARAM(TVGN_PARENT as usize),
            LPARAM(ht.hItem.0),
        )
    };
    if parent.0 != 0 {
        return;
    }

    activate_target(hwnd, data, list_view, ht.hItem);
}

/// 将根项对应的目标窗口置前（临时置顶：提到最前并聚焦，不常驻顶层）
///
/// 从根项 `lParam` 取目标窗口句柄，经 [`IsWindow`] 校验：
/// - 有效：最小化时先恢复（`SW_RESTORE`），再 `SetForegroundWindow` +
///   `SetWindowPos` 置顶；
/// - 无效（窗口已关闭）：从标签存储移除对应条目并刷新列表。
fn activate_target(hwnd: HWND, data: *mut PanelData, list_view: HWND, hitem: HTREEITEM) {
    // 读取根项的 lParam（插入树时写入的是目标窗口句柄）
    let mut item = TVITEMW {
        mask: TVIF_PARAM,
        hItem: hitem,
        ..Default::default()
    };
    // SAFETY: item 为栈上变量，TVM_GETITEMW 返回前完成写入。
    unsafe {
        let _ = SendMessageW(
            list_view,
            TVM_GETITEMW,
            WPARAM(0),
            LPARAM(std::ptr::addr_of_mut!(item) as isize),
        );
    }

    let target = HWND(item.lParam.0 as *mut std::ffi::c_void);

    // SAFETY: IsWindow 为只读查询，校验目标窗口是否仍存活，无副作用。
    if unsafe { IsWindow(target) }.as_bool() {
        // SAFETY: 目标窗口已通过 IsWindow 校验存活；最小化时先恢复窗口，
        // 再 SetForegroundWindow 置前（由用户单击授权），SetWindowPos 置顶
        // 但不抢占输入焦点。临时置前：切到其他软件后不驻留最上层。
        unsafe {
            if IsIconic(target).as_bool() {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            let _ = SetForegroundWindow(target);
            let _ = SetWindowPos(
                target,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    } else {
        // 窗口已关闭：从标签存储移除对应条目后刷新列表
        // SAFETY: data 已校验非空；lock 失败（中毒）时仅刷新列表，不影响主流程。
        if let Some(mut store) = unsafe { (*data).tag_store.lock().ok() } {
            store.remove(&(target.0 as isize));
        }
        refresh_tree(hwnd);
    }
}

/// 按搜索条件重建树形列表
///
/// 每个标签一个根项（文本 = "标题 | 窗口名称"，`lParam` = 目标窗口句柄），
/// 根项下挂备注详情子项（R15）："备注：" 标签行 + 备注完整内容——TreeView
/// 项为单行，多行备注逐行拆为独立子项才能完整显示（备注为空时显示"（无）"）。
/// 重建前记录已展开根项（按目标窗口句柄），重建后恢复展开状态，供标签变更
/// 广播触发的自动刷新不打断浏览。搜索匹配字段与旧列表视图一致：标题 / 备注 /
/// 窗口标题 / 进程名。
fn refresh_tree(hwnd: HWND) {
    // SAFETY: 面板窗口生命周期内 PanelData 有效（create_panel 创建，WM_DESTROY 回收）。
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() {
        return;
    }

    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    let Ok(list_view) = (unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) }) else {
        return;
    };

    // 读取搜索文本
    let mut search_buf = [0u16; 256];
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    let query = if let Ok(search_edit) = unsafe { GetDlgItem(hwnd, IDC_SEARCH_EDIT) } {
        // SAFETY: GetWindowTextW 将文本写入 search_buf，缓冲区由栈上数组保证有效。
        let len = unsafe { GetWindowTextW(search_edit, &mut search_buf) } as usize;
        String::from_utf16_lossy(&search_buf[..len.min(255)])
            .trim()
            .to_lowercase()
    } else {
        String::new()
    };

    // SAFETY: data 已校验非空；锁中毒（lock 返回 Err）时直接返回，避免误清空列表。
    let Ok(store) = (unsafe { (*data).tag_store.lock() }) else {
        return;
    };

    // 重建前记录已展开根项对应的目标窗口句柄（TVM_DELETEITEM 会失效旧项句柄）
    let expanded = collect_expanded_targets(list_view);

    // 清空整棵树（TVI_ROOT 表示根）
    // SAFETY: 向树视图发送清空消息，参数为编译期常量 TVI_ROOT。
    unsafe {
        let _ = SendMessageW(list_view, TVM_DELETEITEM, WPARAM(0), LPARAM(TVI_ROOT.0));
    }

    let mut entries: Vec<_> = store
        .iter()
        .filter(|(_, tag)| {
            query.is_empty()
                || tag.title.to_lowercase().contains(&query)
                || tag.note.to_lowercase().contains(&query)
                || tag.window_title.to_lowercase().contains(&query)
                || tag.process_name.to_lowercase().contains(&query)
        })
        .collect();
    entries.sort_by(|a, b| a.1.title.cmp(&b.1.title));

    for (target_hwnd, tag) in entries {
        // 根项：标题与窗口名称同行（R15）；树控件自动裁剪过长文本，无需截断
        let root_text = format!("{} | {}", tag.title, tag.window_title);
        let title_wide: Vec<u16> = root_text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut root = TVINSERTSTRUCTW {
            hParent: TVI_ROOT,
            hInsertAfter: TVI_LAST,
            Anonymous: TVINSERTSTRUCTW_0 {
                item: TVITEMW {
                    mask: TVIF_TEXT | TVIF_PARAM,
                    pszText: windows::core::PWSTR(title_wide.as_ptr() as *mut _),
                    cchTextMax: title_wide.len() as i32,
                    lParam: LPARAM(*target_hwnd),
                    ..Default::default()
                },
            },
        };

        // SAFETY: root 与 title_wide 在 SendMessageW 调用期间存活，
        // TVM_INSERTITEMW 在消息返回前完成数据拷贝。
        let inserted = unsafe {
            SendMessageW(
                list_view,
                TVM_INSERTITEMW,
                WPARAM(0),
                LPARAM(std::ptr::addr_of_mut!(root) as isize),
            )
        };
        if inserted.0 == 0 {
            continue;
        }
        let parent = HTREEITEM(inserted.0);

        // 详情子项："备注：" 标签行 + 备注完整内容（R15）。TreeView 项为单行，
        // 多行备注必须逐行拆为独立子项才能完整显示；空备注显示占位"（无）"。
        let mut child_texts: Vec<String> = vec!["备注：".to_string()];
        if tag.note.is_empty() {
            child_texts.push("（无）".to_string());
        } else {
            child_texts.extend(tag.note.lines().map(str::to_string));
        }
        for text in child_texts {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let mut child = TVINSERTSTRUCTW {
                hParent: parent,
                hInsertAfter: TVI_LAST,
                Anonymous: TVINSERTSTRUCTW_0 {
                    item: TVITEMW {
                        mask: TVIF_TEXT,
                        pszText: windows::core::PWSTR(wide.as_ptr() as *mut _),
                        cchTextMax: wide.len() as i32,
                        ..Default::default()
                    },
                },
            };
            // SAFETY: child 与 wide 在 SendMessageW 调用期间存活。
            unsafe {
                let _ = SendMessageW(
                    list_view,
                    TVM_INSERTITEMW,
                    WPARAM(0),
                    LPARAM(std::ptr::addr_of_mut!(child) as isize),
                );
            }
        }

        // 恢复重建前该根项的展开状态（按目标窗口句柄匹配）
        if expanded.contains(target_hwnd) {
            // SAFETY: parent 为本次插入返回的有效项句柄，TVM_EXPAND 仅切换展开态。
            unsafe {
                let _ = SendMessageW(
                    list_view,
                    TVM_EXPAND,
                    WPARAM(TVE_EXPAND.0 as usize),
                    LPARAM(parent.0),
                );
            }
        }
    }
}

/// 收集树中所有已展开根项的目标窗口句柄（lParam）
///
/// 从根项起沿兄弟链遍历（`TVGN_ROOT` → `TVGN_NEXT`），逐项读取
/// `TVIS_EXPANDED` 状态，供 [`refresh_tree`] 重建后恢复展开状态。
fn collect_expanded_targets(list_view: HWND) -> HashSet<isize> {
    let mut expanded = HashSet::new();
    // SAFETY: TVM_GETNEXTW/TVGN_* 为只读遍历，参数为常量或消息返回的项句柄；
    // 树控件存活（GetDlgItem 已确认），返回 0 表示遍历结束。
    let mut item = unsafe {
        SendMessageW(
            list_view,
            TVM_GETNEXTITEM,
            WPARAM(TVGN_ROOT as usize),
            LPARAM(0),
        )
    };
    while item.0 != 0 {
        let mut tvi = TVITEMW {
            mask: TVIF_HANDLE | TVIF_STATE | TVIF_PARAM,
            hItem: HTREEITEM(item.0),
            stateMask: TVIS_EXPANDED,
            ..Default::default()
        };
        // SAFETY: tvi 为栈上局部值，TVM_GETITEMW 在消息返回前完成数据拷贝。
        unsafe {
            let _ = SendMessageW(
                list_view,
                TVM_GETITEMW,
                WPARAM(0),
                LPARAM(std::ptr::addr_of_mut!(tvi) as isize),
            );
        }
        if tvi.state.0 & TVIS_EXPANDED.0 != 0 {
            expanded.insert(tvi.lParam.0);
        }
        // SAFETY: 同上，TVGN_NEXT 沿兄弟链推进，返回 0 时退出循环。
        item = unsafe {
            SendMessageW(
                list_view,
                TVM_GETNEXTITEM,
                WPARAM(TVGN_NEXT as usize),
                LPARAM(item.0),
            )
        };
    }
    expanded
}

/// 重新应用主题到面板的树形列表（供 main.rs reapply_theme 调用）
///
/// 主题切换后树形列表的 DarkMode_Explorer 需重新设置才能让滚动条/展开按钮
/// 跟随新主题。取 IDC_LIST_VIEW 子控件并刷新主题与配色。
pub fn reapply_tree_theme(panel_hwnd: HWND, dark: bool) {
    // SAFETY: panel_hwnd 由调用方保证存活；GetDlgItem 按子控件 ID 查询，
    // 失败时返回 Err 被忽略。
    if let Ok(list_view) = unsafe { GetDlgItem(panel_hwnd, IDC_LIST_VIEW) } {
        apply_tree_theme(list_view, dark);
    }
}

/// 切换概览面板的显示 / 隐藏状态
///
/// 面板当前可见则隐藏；不可见则刷新列表、显示面板并将其置前到前台。
/// `hwnd` 必须是由 [`create_panel`] 创建的面板窗口句柄。
pub fn toggle_panel(hwnd: HWND) {
    // SAFETY: hwnd 由 create_panel 返回，窗口存活期间 PanelData 有效。
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() {
        return;
    }

    // SAFETY: data 已校验非空，窗口仍存活。
    let visible = unsafe { (*data).visible };
    if !visible {
        // SAFETY: data 已校验非空。
        unsafe {
            (*data).visible = true;
        }
        refresh_tree(hwnd);
        // SAFETY: 显示面板并置前到前台，由用户显式热键触发的窗口操作。
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    } else {
        // SAFETY: data 已校验非空，隐藏面板。
        unsafe {
            (*data).visible = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}
