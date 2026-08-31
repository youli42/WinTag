use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, ScreenToClient, SetBkColor, SetTextColor, HDC};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, HTREEITEM, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, NMHDR, NM_CLICK,
    NM_DBLCLK, TVE_COLLAPSE, TVE_EXPAND, TVGN_CARET, TVGN_NEXT, TVGN_PARENT, TVGN_ROOT,
    TVHITTESTINFO, TVHT_ONITEM, TVHT_ONITEMBUTTON, TVIF_HANDLE, TVIF_PARAM, TVIF_STATE, TVIF_TEXT,
    TVINSERTSTRUCTW, TVINSERTSTRUCTW_0, TVIS_EXPANDED, TVITEMW, TVI_LAST, TVI_ROOT, TVM_DELETEITEM,
    TVM_EXPAND, TVM_GETITEMW, TVM_GETNEXTITEM, TVM_HITTEST, TVM_INSERTITEMW, TVM_SETBKCOLOR,
    TVM_SETLINECOLOR, TVM_SETTEXTCOLOR, TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT,
    TVS_SHOWSELALWAYS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, SetFocus, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetClassLongPtrW,
    GetClientRect, GetCursorPos, GetDlgCtrlID, GetDlgItem, GetParent, GetWindowTextW, IsIconic,
    IsWindow, PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TrackPopupMenu, BN_CLICKED, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    EN_CHANGE, ES_AUTOHSCROLL, GCLP_WNDPROC, GWLP_WNDPROC, HWND_TOP, MF_STRING, MINMAXINFO,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_RESTORE, SW_SHOW, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
    WM_CREATE, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_NOTIFY, WM_SIZE, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::common::{get_userdata, set_userdata, widestring, WM_APP_EDIT_TAG, WM_APP_TAGS_CHANGED};
use crate::core::tag::TagStore;
use crate::ui::button::{self, ButtonStyle};
use crate::ui::layout::dp;
use crate::ui::theme::{apply_font_to_children, theme_colors};

const IDC_SEARCH_EDIT: i32 = 201;
const IDC_LIST_VIEW: i32 = 202;
/// 一键全部展开 / 全部收起按钮（问题 20）
const IDC_EXPAND_ALL: i32 = 203;
const IDC_COLLAPSE_ALL: i32 = 204;

/// 键盘焦点循环顺序（问题 22）：搜索框 → 树形列表 → 全部展开 → 全部收起
const FOCUS_ORDER: [i32; 4] = [
    IDC_SEARCH_EDIT,
    IDC_LIST_VIEW,
    IDC_EXPAND_ALL,
    IDC_COLLAPSE_ALL,
];

/// 右键菜单命令 ID（R17，配合 `TPM_RETURNCMD` 直接取回选择）
const IDM_ACTIVATE: i32 = 301;
const IDM_EDIT: i32 = 302;
const IDM_REMOVE: i32 = 303;

/// 设计像素常量（96 DPI 基准），运行时经 [`dp`] 缩放为物理像素
const MARGIN: i32 = 12;
const SEARCH_H: i32 = 28;
const SEARCH_GAP: i32 = 8;
/// 按钮行（问题 20）：全部展开 / 全部收起按钮，与搜索框同排右对齐
const BTN_W: i32 = 76;
const BTN_H: i32 = 28;
const BTN_GAP: i32 = 8;
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
    /// 主线程隐藏窗口句柄（右键菜单"编辑标签"经 WM_APP_EDIT_TAG 转发，R17）
    pub hidden_hwnd: isize,
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
pub fn create_panel(data: Arc<Mutex<TagStore>>, hidden_hwnd: isize) -> HWND {
    let panel_data = Box::new(PanelData {
        tag_store: data,
        visible: false,
        hidden_hwnd,
    });
    let data_ptr = Box::into_raw(panel_data);

    let class_name = widestring("WinTagPanel");

    let wc = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(panel_wndproc),
        hInstance: windows::Win32::Foundation::HINSTANCE::default(),
        // 类光标必须非 NULL（NULL 会让 DefWindowProc 的 WM_SETCURSOR 隐藏光标）
        hCursor: crate::common::arrow_cursor(),
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

            // 统一主题管理器（D17）：解析全局设置 → 写入全局调色板 → 取得暗色判定
            //（需在子控件创建前完成，保证 WM_CTLCOLOR* 取色有值）
            let theme_ctx = crate::ui::theme::sync_window_theme();
            let dark = theme_ctx.dark;
            // SAFETY: hwnd 为正在创建的面板窗口（WM_CREATE 期间有效）；
            // DWM 属性调用失败（如 Win10 不支持圆角属性）时静默忽略返回值。
            let _ = crate::ui::theme::apply_dark_mode(hwnd, dark);
            let _ = crate::ui::theme::apply_corner_preference(hwnd, theme_ctx.corner);

            let instance = windows::Win32::Foundation::HINSTANCE::default();

            // DPI 缩放后的布局常量
            let m = dp(hwnd, MARGIN);
            let search_h = dp(hwnd, SEARCH_H);
            let list_y = m + search_h + dp(hwnd, SEARCH_GAP);

            // 搜索框（子类化转发 Esc → 关闭面板，R17）
            let search_ws = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | ES_AUTOHSCROLL as u32);
            // SAFETY: 创建 EDIT 子控件（ID = IDC_SEARCH_EDIT）；
            // 失败时忽略，搜索功能不可用但不影响面板主体。
            let search_result = unsafe {
                CreateWindowExW(
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
                )
            };
            if let Ok(search_edit) = search_result {
                // SAFETY: search_edit 为刚创建成功的有效子控件句柄；子类化仅替换
                // 实例窗口过程，无额外内存操作。
                unsafe {
                    let _ = SetWindowLongPtrW(
                        search_edit,
                        GWLP_WNDPROC,
                        search_edit_subclass_proc as *const () as isize,
                    );
                }
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
            // 子类化树控件（问题 22）：回车 / Tab 转发父面板统一处理键盘操作
            //（树自身默认消费回车切换展开，必须拦截才能回车激活选中项）
            if list_view != HWND::default() {
                // SAFETY: list_view 为刚创建成功的有效子控件句柄；子类化仅替换
                // 实例窗口过程，无额外内存操作。函数指针先经 `as *const ()` 再
                // 转 isize，避免 function_casts_as_integer 警告。
                unsafe {
                    let _ = SetWindowLongPtrW(
                        list_view,
                        GWLP_WNDPROC,
                        tree_subclass_proc as *const () as isize,
                    );
                }
            }

            // 按钮行（问题 20）：全部展开 / 全部收起，与搜索框同排右对齐。
            // 自绘圆角按钮（BS_OWNERDRAW），绘制由 WM_DRAWITEM 分支分发到
            // ui::button；初始坐标按 WIN_W 估算，WM_SIZE/layout_children 校正。
            let btn_w = dp(hwnd, BTN_W);
            let btn_h = dp(hwnd, BTN_H);
            let btn_gap = dp(hwnd, BTN_GAP);
            let (_search_w, btn_x, btn_y) = button_row_layout(dp(hwnd, WIN_W), m, btn_w, btn_gap);
            // SAFETY: create_button 内部注册状态并子类化；失败返回 Err，忽略即可
            // （按钮不可用不影响面板其余功能，WM_COMMAND 仍走原 ID 路由）。
            let _ = button::create_button(
                hwnd,
                IDC_EXPAND_ALL,
                "全部展开",
                btn_x,
                btn_y,
                btn_w,
                btn_h,
                ButtonStyle::Secondary,
            );
            let _ = button::create_button(
                hwnd,
                IDC_COLLAPSE_ALL,
                "全部收起",
                btn_x + btn_w + btn_gap,
                btn_y,
                btn_w,
                btn_h,
                ButtonStyle::Secondary,
            );

            // 暗色时设 DarkMode_Explorer 主题（滚动条随之暗化）；亮色恢复 Explorer。
            // 同时按主题调色板设置背景/文字/连线色。需 comctl32 v6 manifest
            // （build.rs 嵌入）才能生效。
            apply_tree_theme(list_view, dark);

            // 首次填充树形列表
            refresh_tree(hwnd);

            // 全局消息字体注入所有子控件（搜索框 + 列表视图）
            apply_font_to_children(hwnd);

            // 子控件 comctl32 主题变体（D17）：搜索框随主题渲染
            crate::ui::theme::apply_control_theme(hwnd, dark);

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
            } else if code == BN_CLICKED {
                // 按钮点击（问题 20）：一键全部展开 / 全部收起
                // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
                if let Ok(list_view) = unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) } {
                    match id {
                        IDC_EXPAND_ALL => expand_all_roots(list_view),
                        IDC_COLLAPSE_ALL => collapse_all_roots(list_view),
                        _ => {}
                    }
                }
            }

            LRESULT(0)
        }
        WM_DRAWITEM => {
            // 自绘按钮（问题 20）：全部展开 / 全部收起按钮由 ui::button 统一
            // 绘制（暗色主题圆角 + 悬停/按压态 + 键盘焦点框）
            if button::handle_draw_item(lparam) {
                LRESULT(1)
            } else {
                // SAFETY: 非按钮的 WM_DRAWITEM 透传默认过程。
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
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
        WM_CONTEXTMENU => {
            // 右键树项（R17）：根项弹出 置前/编辑/移除 菜单
            handle_tree_context_menu(hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN => {
            let key = (wparam.0 & 0xFFFF) as u16;
            // Esc 关闭面板（R17）：搜索框聚焦时由其子类化过程转发到达
            if key == VK_ESCAPE.0 {
                // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
                let data = unsafe { get_userdata::<PanelData>(hwnd) };
                if !data.is_null() {
                    // SAFETY: data 已校验非空，标记为隐藏。
                    unsafe {
                        (*data).visible = false;
                    }
                }
                // SAFETY: 仅隐藏面板，不销毁（镜像 WM_CLOSE 行为）。
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                LRESULT(0)
            } else if key == VK_TAB.0 {
                // Tab / Shift+Tab 循环焦点（问题 22）：搜索框、树、按钮子类化
                // 过程均已转发 VK_TAB 到面板，统一在此分发。
                // SAFETY: GetKeyState 查询虚拟键状态，最高位为 1（负值）表示按下。
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                focus_next_control(hwnd, !shift);
                LRESULT(0)
            } else if key == VK_RETURN.0 {
                // 回车激活树选中项（问题 22）：树子类化过程转发到达
                activate_caret_item(hwnd);
                LRESULT(0)
            } else {
                // SAFETY: 未处理按键透传默认窗口过程（保持面板现有语义）。
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
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

/// 按钮行布局纯函数（问题 20：一键全部展开 / 全部收起）
///
/// 按钮与搜索框同排、右对齐：搜索框占左段，两个按钮（全部展开 / 全部收起）
/// 依次占据右段。返回值 `(search_w, btn_x, btn_y)`：
///
/// - `search_w`：留给搜索框的宽度（按钮左侧到左边距之间，扣除按钮间隔）；
/// - `btn_x`：第一个按钮（全部展开）的 x 坐标，右对齐；客户区过窄时钳到左边距；
/// - `btn_y`：按钮行 y 坐标（= 搜索框行 y = `m`）。
///
/// 边界保证：客户区极窄时 `search_w` 与 `btn_x` 均经 `.max` 钳位非负。
pub(crate) fn button_row_layout(
    client_w: i32,
    m: i32,
    btn_w: i32,
    btn_gap: i32,
) -> (i32, i32, i32) {
    let btn_area = btn_w * 2 + btn_gap;
    // 右对齐：btn_x = client_w - m - 按钮区总宽；过窄时钳到左边距 m
    let btn_x = (client_w - m - btn_area).max(m);
    // 搜索框宽度 = 按钮左缘 - 左边距 - 按钮间隔；过窄时钳到 0
    let search_w = (btn_x - m - btn_gap).max(0);
    (search_w, btn_x, m)
}

/// Tab 焦点循环纯函数（问题 22）：计算下一个获得焦点的控件在顺序表中的下标
///
/// 从 `order` 中定位 `current`（当前焦点控件 ID，经 `GetDlgCtrlID` 取得）：
///
/// - 命中位置 `i`：正向返回 `(i + 1) % n`，反向返回 `(i + n - 1) % n`
///   （即 i - 1 模 n），首尾天然回绕；
/// - 不在 `order` 中（未知 ID，异常路径）：返回 0（落到第一个控件）。
///
/// 由 [`focus_next_control`] 复用（与 popup 焦点循环同一逻辑，抽为纯函数
/// 便于单测）。
pub(crate) fn tab_cycle_index(current: i32, order: &[i32], forward: bool) -> usize {
    match order.iter().position(|&id| id == current) {
        Some(i) => {
            let n = order.len();
            let delta = if forward { 1 } else { n - 1 };
            (i + delta) % n
        }
        None => 0,
    }
}

/// 在面板子控件间循环切换键盘焦点（Tab 正向 / Shift+Tab 反向，问题 22）
///
/// 按 [`FOCUS_ORDER`] 顺序从当前焦点控件取下一个（经 [`tab_cycle_index`]）；
/// 焦点不在已知控件上（异常路径）时落到第一个控件（搜索框）。
fn focus_next_control(hwnd: HWND, forward: bool) {
    // SAFETY: GetFocus 查询调用线程当前焦点窗口，无失败路径（无焦点返回 NULL）。
    let current = unsafe { GetFocus() };
    // SAFETY: current 为本线程焦点窗口句柄（可能为 NULL，查询返回 0 无副作用）。
    let cur_id = unsafe { GetDlgCtrlID(current) };
    let next_idx = tab_cycle_index(cur_id, &FOCUS_ORDER, forward);
    // SAFETY: GetDlgItem 按 ID 查询本窗口子控件，失败返回 Err 被忽略。
    if let Ok(next) = unsafe { GetDlgItem(hwnd, FOCUS_ORDER[next_idx]) } {
        // SAFETY: next 为存活子控件句柄；SetFocus 失败仅返回 Err，忽略。
        unsafe {
            let _ = SetFocus(next);
        }
    }
}

/// 子控件布局（WM_SIZE / WM_CREATE 末尾统一调用）
///
/// 修正原 WM_SIZE 的 bug（SetWindowPos 缺 SWP_NOMOVE，控件被吸到 (0,0)），
/// 恢复四边 MARGIN 内边距：搜索框顶部对齐、按钮（问题 20）与搜索框同排
/// 右对齐、列表占剩余空间。
fn layout_children(hwnd: HWND, width: i32, height: i32) {
    let m = dp(hwnd, MARGIN);
    let search_h = dp(hwnd, SEARCH_H);
    let btn_w = dp(hwnd, BTN_W);
    let btn_h = dp(hwnd, BTN_H);
    let btn_gap = dp(hwnd, BTN_GAP);
    // 按钮与搜索框同排：行高取两者较大者（BTN_H == SEARCH_H 时列表 y 不变）
    let row_h = btn_h.max(search_h);
    let list_y = m + row_h + dp(hwnd, SEARCH_GAP);
    let (search_w, btn_x, btn_y) = button_row_layout(width, m, btn_w, btn_gap);

    // 搜索框：保持原位置（m, m），宽度按按钮行布局压缩
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
                search_w,
                search_h,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
            );
        }
    }
    // 按钮行（问题 20）：全部展开 / 全部收起，搜索框右侧依次排布
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    if let Ok(expand_btn) = unsafe { GetDlgItem(hwnd, IDC_EXPAND_ALL) } {
        // SAFETY: SetWindowPos 移动到按钮行位置并调整尺寸；SWP_NOZORDER 保留 Z 序。
        use windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER;
        unsafe {
            let _ = SetWindowPos(
                expand_btn,
                HWND_TOP,
                btn_x,
                btn_y,
                btn_w,
                btn_h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }
    // SAFETY: 同上，GetDlgItem 按子控件 ID 查询，失败时忽略。
    if let Ok(collapse_btn) = unsafe { GetDlgItem(hwnd, IDC_COLLAPSE_ALL) } {
        // SAFETY: SetWindowPos 移动收起按钮到展开按钮右侧并调整尺寸。
        use windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER;
        unsafe {
            let _ = SetWindowPos(
                collapse_btn,
                HWND_TOP,
                btn_x + btn_w + btn_gap,
                btn_y,
                btn_w,
                btn_h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }
    // 列表：占按钮行下方到客户区底边
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    if let Ok(list_view) = unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) } {
        let list_h = (height - list_y - m).max(1);
        let content_w = (width - 2 * m).max(1);
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

/// 回车激活树当前选中项（问题 22）：取 `TVGN_CARET` 选中项 → 子项上溯到
/// 根项 → 置前目标窗口
///
/// 键盘焦点在树控件上时，方向键移动选中项（`TVGN_CARET` 随之更新），回车
/// 经树子类化过程转发到面板 `WM_KEYDOWN` 分支后调用本函数。选中项为详情
/// 子项时上溯到其根项（与右键菜单 [`handle_tree_context_menu`] 同一规则）。
fn activate_caret_item(hwnd: HWND) {
    // SAFETY: 面板窗口由 create_panel 创建，窗口存活期间 PanelData 有效。
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() {
        return;
    }
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    let Ok(list_view) = (unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) }) else {
        return;
    };
    // SAFETY: TVM_GETNEXTITEM(TVGN_CARET) 只读查询当前选中项，无选中时返回 0。
    let selected = unsafe {
        SendMessageW(
            list_view,
            TVM_GETNEXTITEM,
            WPARAM(TVGN_CARET as usize),
            LPARAM(0),
        )
    };
    if selected.0 == 0 {
        return;
    }
    // 命中详情子项 → 上溯到根项（键盘激活与右键菜单同规则）
    // SAFETY: selected 为 TVM_GETNEXTITEM 返回的有效句柄，TVM_GETNEXTITEM 只读查询。
    let parent = unsafe {
        SendMessageW(
            list_view,
            TVM_GETNEXTITEM,
            WPARAM(TVGN_PARENT as usize),
            LPARAM(selected.0),
        )
    };
    let root_item = if parent.0 != 0 {
        HTREEITEM(parent.0)
    } else {
        HTREEITEM(selected.0)
    };
    activate_target(hwnd, data, list_view, root_item);
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

/// 处理树形列表右键菜单（R17）：根项弹出 置前窗口 / 编辑标签 / 移除标签
///
/// 仅鼠标右键（lParam != -1）触发；命中详情子项时上溯到其根项。
/// `TPM_RETURNCMD` 直接取回所选命令，无需 WM_COMMAND 路由。
fn handle_tree_context_menu(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: get_userdata 由 common 封装，hwnd 为本窗口且仅在消息循环内调用。
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() {
        // SAFETY: DefWindowProcW 透传未处理消息。
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    if lparam.0 == -1 {
        // 键盘 Shift+F10：无光标坐标，忽略
        return LRESULT(0);
    }
    // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
    let Ok(list_view) = (unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) }) else {
        return LRESULT(0);
    };
    let x = (lparam.0 & 0xFFFF) as u16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i32;
    let mut client = POINT { x, y };
    // SAFETY: ScreenToClient 坐标换算，client 为栈上值。
    unsafe {
        let _ = ScreenToClient(list_view, &mut client);
    }
    let mut ht = TVHITTESTINFO {
        pt: client,
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
        return LRESULT(0);
    }
    // 命中详情子项 → 上溯到根项（菜单仅对根项/标签生效）
    // SAFETY: hItem 为 TVM_HITTEST 返回的有效句柄，TVM_GETNEXTITEM 只读查询。
    let parent = unsafe {
        SendMessageW(
            list_view,
            TVM_GETNEXTITEM,
            WPARAM(TVGN_PARENT as usize),
            LPARAM(ht.hItem.0),
        )
    };
    let hitem = if parent.0 != 0 {
        HTREEITEM(parent.0)
    } else {
        ht.hItem
    };

    // 读根项 lParam（目标窗口句柄）
    let mut tvi = TVITEMW {
        mask: TVIF_HANDLE | TVIF_PARAM,
        hItem: hitem,
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
    let target = tvi.lParam.0;
    if target == 0 {
        return LRESULT(0);
    }

    // 构建并弹出菜单（项文本为编译期常量）
    // SAFETY: CreatePopupMenu 返回新建菜单句柄，失败返回 Err 被忽略。
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return LRESULT(0);
    };
    // SAFETY: menu 为刚创建的存活菜单句柄，AppendMenuW 仅追加项。
    unsafe {
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_ACTIVATE as usize,
            windows::core::w!("置前窗口"),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_EDIT as usize,
            windows::core::w!("编辑标签…"),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_REMOVE as usize,
            windows::core::w!("移除标签"),
        );
    }
    // TPM_RETURNCMD：返回值即所选命令 ID（0 = 未选择）；菜单操作为模态阻塞。
    // SAFETY: menu/hwnd 均为存活句柄。
    let cmd = unsafe { TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, 0, hwnd, None) };
    // SAFETY: 菜单已弹出完毕，销毁防泄漏。
    unsafe {
        let _ = DestroyMenu(menu);
    }

    match cmd.0 {
        id if id == IDM_ACTIVATE => activate_target(hwnd, data, list_view, hitem),
        id if id == IDM_EDIT => {
            // 经隐藏窗口请求打开编辑弹窗（与角标单击同一路径，R5/R17）
            // SAFETY: hidden_hwnd 为启动时注入的存活隐藏窗口句柄；
            // PostMessageW 为线程安全标准 API。
            unsafe {
                let _ = PostMessageW(
                    HWND((*data).hidden_hwnd as *mut std::ffi::c_void),
                    WM_APP_EDIT_TAG,
                    WPARAM(target as usize),
                    LPARAM(0),
                );
            }
        }
        id if id == IDM_REMOVE => {
            // 移除标签：清存储 → 通知主线程销毁覆盖层 → 刷新列表
            // SAFETY: data 已校验非空；锁中毒时跳过（仅覆盖层残留，下次清理兜底）。
            if let Ok(mut store) = unsafe { (*data).tag_store.lock() } {
                store.remove(&target);
            }
            // SAFETY: 同上，hidden_hwnd 为存活隐藏窗口句柄。
            unsafe {
                let _ = PostMessageW(
                    HWND((*data).hidden_hwnd as *mut std::ffi::c_void),
                    crate::common::WM_DESTROY_OVERLAY,
                    WPARAM(target as usize),
                    LPARAM(0),
                );
            }
            refresh_tree(hwnd);
        }
        _ => {}
    }
    LRESULT(0)
}

/// 面板搜索框子类化过程：Esc / Tab 键转发父面板（R17 / 问题 22），其余透传
///
/// 标准 EDIT 类过程会吞掉 Esc，不转发则搜索框聚焦时 Esc 无法关闭面板；
/// 单行 EDIT 在非 dialog 父窗口下 Tab 也不会自动切换焦点，需手动转发给
/// 父面板的 `WM_KEYDOWN` 分支统一循环焦点。
unsafe extern "system" fn search_edit_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        let key = (wparam.0 & 0xFFFF) as u16;
        if key == VK_ESCAPE.0 || key == VK_TAB.0 {
            // SAFETY: GetParent 返回创建时指定的父窗口。
            if let Ok(parent) = unsafe { GetParent(hwnd) } {
                // SAFETY: PostMessageW 为线程安全标准 API，异步投递。
                unsafe {
                    let _ = PostMessageW(parent, WM_KEYDOWN, wparam, lparam);
                }
            }
            return LRESULT(0);
        }
    }
    // SAFETY: GetClassLongPtrW(GCLP_WNDPROC) 返回 EDIT 类原始窗口过程，
    // 函数指针↔整数 transmute 往返在 Windows ABI 下良定义。
    let orig = unsafe { GetClassLongPtrW(hwnd, GCLP_WNDPROC) };
    let orig_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        unsafe { std::mem::transmute(orig) };
    orig_proc(hwnd, msg, wparam, lparam)
}

/// 树控件子类化过程（问题 22）：回车 / Tab / Esc 转发父面板统一处理，其余透传
///
/// SysTreeView32 自身会消费回车（切换展开）、Tab（项目内导航）等按键；为统一键盘
/// 语义（回车激活选中项、Tab 循环焦点、Esc 关闭面板——焦点在树时 Esc 不会向父窗口
/// 冒泡，必须在此拦截，否则已验收的 R17「Esc 关闭面板」从面板默认状态失效），
/// 在此拦截 `VK_RETURN` / `VK_TAB` / `VK_ESCAPE` 转发给父面板的 `WM_KEYDOWN`
/// 分支处理，其余消息透传树类原始窗口过程。
unsafe extern "system" fn tree_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        let key = (wparam.0 & 0xFFFF) as u16;
        // Tab/Esc 转发镜像 button_subclass_proc（panel 既有按钮子类化转发三者），
        // 回车为树选中项激活。
        if key == VK_RETURN.0 || key == VK_TAB.0 || key == VK_ESCAPE.0 {
            // SAFETY: GetParent 返回创建时指定的父面板窗口。
            if let Ok(parent) = unsafe { GetParent(hwnd) } {
                // SAFETY: PostMessageW 为线程安全标准 API，异步投递。
                unsafe {
                    let _ = PostMessageW(parent, WM_KEYDOWN, wparam, lparam);
                }
            }
            return LRESULT(0);
        }
    }
    // 其余消息透传 SysTreeView32 类原始窗口过程（子类化仅替换实例过程，类过程不变）
    // SAFETY: GetClassLongPtrW(GCLP_WNDPROC) 返回树类默认窗口过程，签名与
    // 窗口过程一致；transmute 为函数指针↔整数往返转换，Windows ABI 下良定义。
    let orig = unsafe { GetClassLongPtrW(hwnd, GCLP_WNDPROC) };
    let orig_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        unsafe { std::mem::transmute(orig) };
    orig_proc(hwnd, msg, wparam, lparam)
}

/// 按搜索条件重建树形列表
///
/// 每个标签一个根项（文本 = "标题 | 窗口名称"，`lParam` = 目标窗口句柄），
/// 根项下挂备注详情子项（R15）："备注：" 标签行 + 备注完整内容——TreeView
/// 项为单行，多行备注逐行拆为独立子项才能完整显示（备注为空时显示"（无）"）。
/// 重建前记录各根项展开/折叠状态（按目标窗口句柄），重建后按
/// [`root_expand_default`] 恢复：已折叠的保持折叠、已展开的保持展开、新出现
/// 的根项默认展开（问题 19），供标签变更广播触发的自动刷新不打断浏览。
/// 搜索匹配字段与旧列表视图一致：标题 / 备注 / 窗口标题 / 进程名。
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

    // 重建前记录各根项的展开/折叠状态（TVM_DELETEITEM 会失效旧项句柄，
    // 必须在清空前完成遍历）
    let (expanded, collapsed) = collect_tree_snapshot(list_view);

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

        // 恢复重建前该根项的展开状态（按目标窗口句柄匹配）；
        // 未知目标（首次出现）默认展开（问题 19）
        if root_expand_default(&expanded, &collapsed, *target_hwnd) {
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

/// 沿根项兄弟链遍历整棵树（`TVGN_ROOT` → `TVGN_NEXT`）
///
/// 对每个根项调用 `f`；`TVM_GETNEXTITEM` 返回 0 时遍历结束。供
/// [`collect_tree_snapshot`] 与 [`expand_all_roots`]/[`collapse_all_roots`]
/// 复用同一遍历骨架，避免三处重复实现漂移。
fn for_each_root(list_view: HWND, mut f: impl FnMut(HTREEITEM)) {
    // SAFETY: TVM_GETNEXTITEM 为只读遍历，参数为常量或消息返回的项句柄；
    // 树控件存活（调用方经 GetDlgItem 确认），返回 0 表示遍历结束。
    let mut item = unsafe {
        SendMessageW(
            list_view,
            TVM_GETNEXTITEM,
            WPARAM(TVGN_ROOT as usize),
            LPARAM(0),
        )
    };
    while item.0 != 0 {
        f(HTREEITEM(item.0));
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
}

/// 一键展开树中全部根项（问题 20）
fn expand_all_roots(list_view: HWND) {
    set_all_roots_expanded(list_view, true);
}

/// 一键收起树中全部根项（问题 20）
fn collapse_all_roots(list_view: HWND) {
    set_all_roots_expanded(list_view, false);
}

/// 对全部根项统一设置展开 / 收起状态（TVM_EXPAND + TVE_EXPAND/TVE_COLLAPSE）
fn set_all_roots_expanded(list_view: HWND, expand: bool) {
    let flag = if expand {
        TVE_EXPAND.0 as usize
    } else {
        TVE_COLLAPSE.0 as usize
    };
    for_each_root(list_view, |item| {
        // SAFETY: item 为遍历返回的有效项句柄（当前树内），TVM_EXPAND 仅
        // 切换该项的展开/收起态，无副作用。
        unsafe {
            let _ = SendMessageW(list_view, TVM_EXPAND, WPARAM(flag), LPARAM(item.0));
        }
    });
}

/// 收集树中所有根项的目标窗口句柄（lParam）及其展开/折叠状态
///
/// 从根项起沿兄弟链遍历（`TVGN_ROOT` → `TVGN_NEXT`），逐项读取
/// `TVIS_EXPANDED` 状态，返回 `(展开集合, 折叠集合)` 供 [`refresh_tree`]
/// 重建后经 [`root_expand_default`] 决策每个根项的展开状态。
///
/// 必须在 `TVM_DELETEITEM` 清空之前调用：遍历读到的项句柄属于旧树，
/// 清空后全部失效。
fn collect_tree_snapshot(list_view: HWND) -> (HashSet<isize>, HashSet<isize>) {
    let mut expanded = HashSet::new();
    let mut collapsed = HashSet::new();
    for_each_root(list_view, |item| {
        let mut tvi = TVITEMW {
            mask: TVIF_HANDLE | TVIF_STATE | TVIF_PARAM,
            hItem: item,
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
        } else {
            collapsed.insert(tvi.lParam.0);
        }
    });
    (expanded, collapsed)
}

/// 判定重建后根项是否默认展开（问题 19：树形列表默认展开详情）
///
/// 决策规则：
/// 1. 目标句柄仅出现在 `collapsed` → `false`（保留用户手动折叠）；
/// 2. 目标句柄仅出现在 `expanded` 或两集合均不含（首次出现）→ `true`；
/// 3. 不变式：正常快照下 `expanded` 与 `collapsed` 互斥，若同一句柄
///    意外出现在两集合中，`expanded` 优先 → `true`。
pub(crate) fn root_expand_default(
    expanded: &HashSet<isize>,
    collapsed: &HashSet<isize>,
    target: isize,
) -> bool {
    // 不在折叠集 → 展开（含首次出现的未知目标）；两集合同含时 expanded 优先
    !collapsed.contains(&target) || expanded.contains(&target)
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
        // 面板打开后把键盘焦点交给树控件（问题 22）：Tab 循环与回车激活
        // 从树开始，方向键即可浏览标签项。
        // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
        if let Ok(list_view) = unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) } {
            // SAFETY: list_view 为面板存活子控件，SetForegroundWindow 已激活
            // 面板；SetFocus 失败仅返回 Err，忽略。
            unsafe {
                let _ = SetFocus(list_view);
            }
        }
    } else {
        // SAFETY: data 已校验非空，隐藏面板。
        unsafe {
            (*data).visible = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未知目标（两集合均不含）：首次出现，默认展开（问题 19）
    #[test]
    fn unknown_target_expands_by_default() {
        let expanded = HashSet::new();
        let collapsed = HashSet::new();
        assert!(root_expand_default(&expanded, &collapsed, 42));
    }

    /// 用户手动折叠过的目标：保持折叠
    #[test]
    fn collapsed_target_stays_collapsed() {
        let expanded = HashSet::new();
        let collapsed = HashSet::from([42]);
        assert!(!root_expand_default(&expanded, &collapsed, 42));
    }

    /// 已展开的目标：保持展开
    #[test]
    fn expanded_target_stays_expanded() {
        let expanded = HashSet::from([42]);
        let collapsed = HashSet::new();
        assert!(root_expand_default(&expanded, &collapsed, 42));
    }

    /// 两集合同含同一句柄时 expanded 优先（文档不变式，正常快照互斥）
    #[test]
    fn expanded_takes_priority_when_in_both_sets() {
        let expanded = HashSet::from([42]);
        let collapsed = HashSet::from([42]);
        assert!(root_expand_default(&expanded, &collapsed, 42));
    }

    /// 按钮行布局（问题 20）：常规宽度下按钮右对齐，btn_x + 按钮区 + 右边距 == client_w
    #[test]
    fn button_row_normal_width_right_aligns() {
        let (search_w, btn_x, btn_y) = button_row_layout(400, 12, 64, 8);
        // 按钮区总宽 = 2 * 64 + 8 = 136；btn_x = 400 - 12 - 136 = 252
        assert_eq!(btn_x, 252);
        assert_eq!(btn_x + 64 * 2 + 8 + 12, 400);
        // 搜索框宽度 = client_w - 2*m - 按钮区 - 间隔 = 400 - 24 - 136 - 8 = 232
        assert_eq!(search_w, 232);
        // 按钮与搜索框同行（y = m）
        assert_eq!(btn_y, 12);
    }

    /// 最小宽度（MIN_W）下按钮与搜索框仍为正宽（问题 20 边界）
    #[test]
    fn button_row_min_width_keeps_positive_sizes() {
        let (search_w, btn_x, btn_y) = button_row_layout(MIN_W, 12, 64, 8);
        assert!(search_w > 0);
        assert!(btn_x >= 12);
        assert_eq!(btn_y, 12);
    }

    /// 极窄客户区：搜索框宽度钳到 0、按钮左缘钳到左边距（问题 20 边界）
    #[test]
    fn button_row_extremely_narrow_clamps() {
        let (search_w, btn_x, btn_y) = button_row_layout(100, 12, 64, 8);
        assert_eq!(search_w, 0);
        assert_eq!(btn_x, 12);
        assert_eq!(btn_y, 12);
    }

    /// btn_y 恒等于搜索框行 y（m），不随宽度变化
    #[test]
    fn button_row_y_is_margin_always() {
        for w in [160, 300, 400, 800] {
            assert_eq!(button_row_layout(w, 12, 64, 8).2, 12);
        }
    }

    /// Tab 循环（问题 22）：当前焦点在顺序表中，正向返回下一个下标
    #[test]
    fn tab_cycle_forward_returns_next_index() {
        let order = [201, 202, 203, 204];
        assert_eq!(tab_cycle_index(201, &order, true), 1);
        assert_eq!(tab_cycle_index(202, &order, true), 2);
        assert_eq!(tab_cycle_index(203, &order, true), 3);
    }

    /// Tab 反向（Shift+Tab，问题 22）：返回前一个下标（i-1 模 n）
    #[test]
    fn tab_cycle_backward_returns_previous_index() {
        let order = [201, 202, 203, 204];
        assert_eq!(tab_cycle_index(202, &order, false), 0);
        assert_eq!(tab_cycle_index(203, &order, false), 1);
        assert_eq!(tab_cycle_index(204, &order, false), 2);
    }

    /// 回绕（问题 22）：最后一个正向 → 0；第一个反向 → n-1
    #[test]
    fn tab_cycle_wraps_around_both_directions() {
        let order = [201, 202, 203, 204];
        assert_eq!(tab_cycle_index(204, &order, true), 0);
        assert_eq!(tab_cycle_index(201, &order, false), 3);
    }

    /// 未知控件 ID（不在顺序表中，问题 22）：落到第一个控件下标 0
    #[test]
    fn tab_cycle_unknown_id_returns_first() {
        let order = [201, 202, 203, 204];
        assert_eq!(tab_cycle_index(999, &order, true), 0);
        assert_eq!(tab_cycle_index(-1, &order, false), 0);
    }

    /// 真实焦点顺序表（FOCUS_ORDER）正向/反向往返一致（问题 22）
    #[test]
    fn tab_cycle_round_trip_on_focus_order() {
        for &id in FOCUS_ORDER.iter() {
            let fwd = tab_cycle_index(id, &FOCUS_ORDER, true);
            let back = tab_cycle_index(FOCUS_ORDER[fwd], &FOCUS_ORDER, false);
            assert_eq!(FOCUS_ORDER[back], id);
        }
    }
}
