use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVIF_PARAM, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETITEMW, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVS_EX_FULLROWSELECT,
    LVS_REPORT, NMITEMACTIVATE, NM_DBLCLK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetDlgItem, GetWindowTextW, IsWindow, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, EN_CHANGE, ES_AUTOHSCROLL, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY,
    WM_NOTIFY, WM_SIZE, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::common::{get_userdata, set_userdata, widestring};
use crate::core::tag::TagStore;

const IDC_SEARCH_EDIT: i32 = 201;
const IDC_LIST_VIEW: i32 = 202;

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
            600,
            450,
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

            let instance = windows::Win32::Foundation::HINSTANCE::default();

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
                    10,
                    10,
                    300,
                    25,
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
                    10,
                    45,
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

            // 整行选择
            // SAFETY: 向列表视图发送扩展样式消息，参数为编译期常量，无生命周期问题。
            unsafe {
                let _ = SendMessageW(
                    list_view,
                    LVM_SETEXTENDEDLISTVIEWSTYLE,
                    WPARAM(0),
                    LPARAM(LVS_EX_FULLROWSELECT as isize),
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

            LRESULT(0)
        }
        WM_SIZE => {
            let width = (lparam.0 & 0xFFFF) as i32;
            let height = ((lparam.0 >> 16) & 0xFFFF) as i32;

            // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
            if let Ok(search_edit) = unsafe { GetDlgItem(hwnd, IDC_SEARCH_EDIT) } {
                // SAFETY: SetWindowPos 调整子控件尺寸位置，参数均为栈上局部值。
                unsafe {
                    let _ =
                        SetWindowPos(search_edit, HWND_TOP, 0, 0, width - 20, 25, SWP_NOACTIVATE);
                }
            }
            // SAFETY: GetDlgItem 按子控件 ID 查询，失败时返回 Err 被忽略。
            if let Ok(list_view) = unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) } {
                // SAFETY: SetWindowPos 调整子控件尺寸位置，参数均为栈上局部值。
                unsafe {
                    let _ = SetWindowPos(
                        list_view,
                        HWND_TOP,
                        0,
                        0,
                        width - 20,
                        height - 55,
                        SWP_NOACTIVATE,
                    );
                }
            }

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
            // NMHDR，故先解引用 NMITEMACTIVATE 读取 hdr 是安全的；仅当 code 为
            // NM_DBLCLK（列表视图双击）时才进一步读取 iItem 字段。
            let nm = unsafe { &*(lparam.0 as *const NMITEMACTIVATE) };
            if nm.hdr.code == NM_DBLCLK && nm.hdr.idFrom == IDC_LIST_VIEW as usize {
                handle_list_dblclk(hwnd, nm.iItem);
            }
            LRESULT(0)
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
