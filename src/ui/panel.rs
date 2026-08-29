use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, SetBkColor, SetTextColor, HDC};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, CDDS_ITEMPREPAINT, CDRF_DODEFAULT, CDRF_NEWFONT, ICC_LISTVIEW_CLASSES,
    INITCOMMONCONTROLSEX, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_PARAM, LVIF_TEXT, LVITEMW,
    LVM_DELETEALLITEMS, LVM_GETITEMW, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_EX_HEADERDRAGDROP, LVS_REPORT, NMITEMACTIVATE, NMLVCUSTOMDRAW, NM_CUSTOMDRAW, NM_DBLCLK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetDlgItem, GetWindowTextW, IsWindow,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, EN_CHANGE, ES_AUTOHSCROLL, HWND_TOP, MINMAXINFO, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_NOTIFY, WM_SIZE, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE,
};

use crate::common::{get_userdata, set_userdata, widestring};
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
const WIN_W: i32 = 640;
const WIN_H: i32 = 480;
const MIN_W: i32 = 520;
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
        dwICC: ICC_LISTVIEW_CLASSES,
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

            // 列表视图
            let lv_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | LVS_REPORT);
            let list_view = unsafe {
                // SAFETY: 创建 SysListView32 子控件（ID = IDC_LIST_VIEW），
                // 样式为普通报表视图；失败时返回默认句柄并打印告警。
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("SysListView32"),
                    windows::core::w!(""),
                    lv_style,
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
                Ok(lv) => lv,
                Err(e) => {
                    eprintln!("创建列表视图失败: {e}");
                    HWND::default()
                }
            };

            // 暗色时设 DarkMode_Explorer 主题（Win11：表头 SysHeader32 与滚动条随之暗化）；
            // 亮色恢复 Explorer。需 comctl32 v6 manifest（build.rs 嵌入）才能生效。
            apply_listview_theme(list_view, dark);

            // 整行选择 + 表头拖拽 + 双缓冲（消除拖动闪烁，问题 9.8）
            // SAFETY: 向列表视图发送扩展样式消息，参数为编译期常量，无生命周期问题。
            unsafe {
                let _ = SendMessageW(
                    list_view,
                    LVM_SETEXTENDEDLISTVIEWSTYLE,
                    WPARAM(0),
                    LPARAM(
                        (LVS_EX_FULLROWSELECT | LVS_EX_HEADERDRAGDROP | LVS_EX_DOUBLEBUFFER)
                            as isize,
                    ),
                );
            }

            // 添加列
            let columns = [("标题", 120), ("备注", 160), ("窗口", 160), ("进程", 100)];
            for (i, (name, width)) in columns.iter().enumerate() {
                let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                let mut col = LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH,
                    pszText: windows::core::PWSTR(wide.as_ptr() as *mut _),
                    cx: *width,
                    ..Default::default()
                };
                // SAFETY: col 与 wide 在 SendMessageW 调用期间存活，
                // LVM_INSERTCOLUMNW 在消息返回前完成拷贝。
                unsafe {
                    let _ = SendMessageW(
                        list_view,
                        LVM_INSERTCOLUMNW,
                        WPARAM(i),
                        LPARAM(std::ptr::addr_of_mut!(col) as isize),
                    );
                }
            }

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
                refresh_list(hwnd);
            }

            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY 的 lParam 指向通知结构，所有通知结构首字段均为
            // NMHDR，故先解引用 NMITEMACTIVATE 读取 hdr；按 hdr.code 分流：
            // NM_DBLCLK → 双击跳转；NM_CUSTOMDRAW → 行级着色（问题 9.2/9.3）。
            let nm = unsafe { &*(lparam.0 as *const NMITEMACTIVATE) };
            match nm.hdr.code {
                NM_DBLCLK if nm.hdr.idFrom == IDC_LIST_VIEW as usize => {
                    handle_list_dblclk(hwnd, nm.iItem);
                }
                NM_CUSTOMDRAW if nm.hdr.idFrom == IDC_LIST_VIEW as usize => {
                    return handle_list_customdraw(lparam);
                }
                _ => {}
            }
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
/// - `WM_CTLCOLORLISTBOX`：SysListView32 的列表区背景（ListView 向其父窗口发送）；
/// - `WM_CTLCOLOREDIT`：搜索框（EDIT）背景；
/// - `WM_CTLCOLORSTATIC`：静态文本背景。
///
/// 颜色取自 [`crate::ui::theme::theme_colors`]；主题状态未初始化（返回 `None`）时
/// 回退 [`DefWindowProcW`] 走系统默认配色。
///
/// 已知局限：表头（SysHeader32）的颜色不通过 `WM_CTLCOLOR` 系列消息传递，
/// 无法在此完全控制，接受系统默认外观。
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

/// 设置 ListView 的视觉主题（问题 9.2/9.3/9.5）
///
/// 暗色时设 `DarkMode_Explorer`：SysHeader32 表头与滚动条随之暗化
/// （Win11 必然生效，Win10 1809+ 生效，更旧降级为 WM_CTLCOLOR 行为，
/// 不撕裂）。亮色恢复 `Explorer`。需 comctl32 v6 manifest 才能生效。
fn apply_listview_theme(list_view: HWND, dark: bool) {
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
}

/// 处理 ListView 自定义绘制（NM_CUSTOMDRAW，问题 9.2/9.3）
///
/// 按主题调色板为列表行着色：
/// - 奇偶行交替底色（listview_bg / listview_alt_bg）；
/// - 选中行用 selected 色；
/// - 文字统一 listview_fg；
/// - 返回 CDRF_NEWFONT 告知使用新字体（配合双缓冲消除闪烁）。
fn handle_list_customdraw(lparam: LPARAM) -> LRESULT {
    // SAFETY: NM_CUSTOMDRAW 的 lParam 指向 NMLVCUSTOMDRAW，WM_NOTIFY 期间有效。
    let lvcd = unsafe { &mut *(lparam.0 as *mut NMLVCUSTOMDRAW) };
    let stage = lvcd.nmcd.dwDrawStage;
    // CDDS_PREPAINT → 返回 CDRF_NOTIFYITEMDRAW 请求逐行通知（默认行为）
    // CDDS_ITEMPREPAINT → 在此设置每行底色与文字色
    if (stage.0 & CDDS_ITEMPREPAINT.0) != 0 {
        let colors = theme_colors().unwrap_or_else(crate::ui::theme::light_colors);
        let idx = lvcd.nmcd.dwItemSpec;
        // 判断选中态：ListView 选中行由 LVIS_SELECTED 标记，nmcd.uItemState
        // 已反映（custom draw 期间系统会置位）
        let selected =
            (lvcd.nmcd.uItemState.0 & windows::Win32::UI::Controls::CDIS_SELECTED.0) != 0;
        if selected {
            lvcd.clrTextBk = colors.selected;
            lvcd.clrText = colors.listview_fg;
        } else if idx % 2 == 1 {
            lvcd.clrTextBk = colors.listview_alt_bg;
            lvcd.clrText = colors.listview_fg;
        } else {
            lvcd.clrTextBk = colors.listview_bg;
            lvcd.clrText = colors.listview_fg;
        }
        LRESULT(CDRF_NEWFONT as isize)
    } else {
        LRESULT(CDRF_DODEFAULT as isize)
    }
}

/// 处理列表项双击：跳转到对应窗口
///
/// 从双击行的 `lParam` 中取出窗口句柄，经 [`IsWindow`] 校验有效性：
/// - 有效：将目标窗口置前到前台（`SetForegroundWindow` + `SetWindowPos` 置顶）；
/// - 无效（窗口已关闭）：从标签存储移除对应条目并刷新列表。
fn handle_list_dblclk(hwnd: HWND, item_index: i32) {
    if item_index < 0 {
        return;
    }
    // SAFETY: 面板窗口由 create_panel 创建，窗口存活期间 PanelData 有效。
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() {
        return;
    }
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    let Ok(list_view) = (unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) }) else {
        return;
    };

    // 读取选中行的 lParam（插入列表时写入的是目标窗口句柄）
    let mut item = LVITEMW {
        mask: LVIF_PARAM,
        iItem: item_index,
        ..Default::default()
    };
    // SAFETY: item 为栈上变量，SendMessageW 消息返回前保持存活；
    // LVM_GETITEMW 返回时向 item.lParam 写入目标窗口句柄。
    let ret = unsafe {
        SendMessageW(
            list_view,
            LVM_GETITEMW,
            WPARAM(item_index as usize),
            LPARAM(std::ptr::addr_of_mut!(item) as isize),
        )
    };
    if ret.0 == 0 {
        // 项不存在或已失效：刷新列表兜底
        refresh_list(hwnd);
        return;
    }

    let target = HWND(item.lParam.0 as *mut std::ffi::c_void);

    // SAFETY: IsWindow 为只读查询，校验目标窗口是否仍存活，无副作用。
    if unsafe { IsWindow(target) }.as_bool() {
        // SAFETY: 目标窗口已通过 IsWindow 校验存活；SetForegroundWindow 将窗口
        // 前置到前台（由用户双击授权），SetWindowPos 置顶但不抢占输入焦点。
        unsafe {
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
        refresh_list(hwnd);
    }
}

fn refresh_list(hwnd: HWND) {
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

    // SAFETY: 向列表视图发送清空消息，参数为编译期常量。
    unsafe {
        let _ = SendMessageW(list_view, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
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

    for (idx, (hwnd, tag)) in entries.iter().enumerate() {
        let title_wide: Vec<u16> = tag.title.encode_utf16().chain(std::iter::once(0)).collect();
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: idx as i32,
            iSubItem: 0,
            pszText: windows::core::PWSTR(title_wide.as_ptr() as *mut _),
            cchTextMax: title_wide.len() as i32,
            lParam: LPARAM(**hwnd),
            ..Default::default()
        };

        // SAFETY: item 与 title_wide 在 SendMessageW 调用期间存活，
        // LVM_INSERTITEMW 在消息返回前完成数据拷贝。
        let inserted = unsafe {
            SendMessageW(
                list_view,
                LVM_INSERTITEMW,
                WPARAM(0),
                LPARAM(std::ptr::addr_of_mut!(item) as isize),
            )
        };

        if inserted.0 >= 0 {
            let row = inserted.0;

            let sub_texts = [
                (1, &tag.note),
                (2, &tag.window_title),
                (3, &tag.process_name),
            ];

            for (col, text) in &sub_texts {
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let mut sub = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: row as i32,
                    iSubItem: *col,
                    pszText: windows::core::PWSTR(wide.as_ptr() as *mut _),
                    cchTextMax: wide.len() as i32,
                    ..Default::default()
                };
                // SAFETY: sub 与 wide 在 SendMessageW 调用期间存活。
                unsafe {
                    let _ = SendMessageW(
                        list_view,
                        LVM_SETITEMTEXTW,
                        WPARAM(row as usize),
                        LPARAM(std::ptr::addr_of_mut!(sub) as isize),
                    );
                }
            }
        }
    }
}

/// 重新应用主题到面板的 ListView（供 main.rs reapply_theme 调用）
///
/// 主题切换后 ListView 的 DarkMode_Explorer 需重新设置才能让表头/滚动条
/// 跟随新主题（问题 9.3/9.5 的热更新）。取 IDC_LIST_VIEW 子控件并刷新主题。
pub fn reapply_listview_theme(panel_hwnd: HWND, dark: bool) {
    // SAFETY: panel_hwnd 由调用方保证存活；GetDlgItem 按子控件 ID 查询，
    // 失败时返回 Err 被忽略。
    if let Ok(list_view) = unsafe { GetDlgItem(panel_hwnd, IDC_LIST_VIEW) } {
        apply_listview_theme(list_view, dark);
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
        refresh_list(hwnd);
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
