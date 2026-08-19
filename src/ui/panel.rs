use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVITEMW,
    INITCOMMONCONTROLSEX, ICC_LISTVIEW_CLASSES, LVS_EX_FULLROWSELECT,
    LVS_REPORT, LVCF_TEXT, LVCF_WIDTH, LVIF_TEXT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetDlgItem, GetWindowTextW, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, EN_CHANGE, GWLP_USERDATA, HWND_TOP,
    SW_HIDE, SW_SHOW, SWP_NOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_DESTROY, WM_SIZE, WS_CHILD, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE, WS_EX_CLIENTEDGE, ES_AUTOHSCROLL,
};

use crate::core::tag::TagStore;

const IDC_SEARCH_EDIT: i32 = 201;
const IDC_LIST_VIEW: i32 = 202;

/// 读取窗口用户数据（Save/Restore）
unsafe fn get_userdata<T>(hwnd: HWND) -> *mut T {
    let old = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, old);
    old as *mut T
}

pub struct PanelData {
    pub tag_store: Arc<Mutex<TagStore>>,
    pub visible: bool,
}

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

    unsafe { let _ = RegisterClassW(&wc); }

    // 初始化通用控件
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    unsafe { let _ = InitCommonControlsEx(&icc); }

    let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_VISIBLE.0);
    unsafe {
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
        .expect("创建面板失败")
    }
}

extern "system" fn panel_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let data = unsafe {
                let cs = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
                cs.lpCreateParams as *mut PanelData
            };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize); }

            let instance = windows::Win32::Foundation::HINSTANCE::default();

            // 搜索框
            let search_ws = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | ES_AUTOHSCROLL as u32);
            unsafe {
                let _ = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    windows::core::w!(""),
                    search_ws,
                    10, 10, 300, 25,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(IDC_SEARCH_EDIT as *mut std::ffi::c_void),
                    instance, None,
                );
            }

            // 列表视图
            let lv_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | LVS_REPORT as u32);
            let list_view = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("SysListView32"),
                    windows::core::w!(""),
                    lv_style,
                    10, 45, 560, 350,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(IDC_LIST_VIEW as *mut std::ffi::c_void),
                    instance, None,
                )
                .expect("创建列表视图失败")
            };

            // 整行选择
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
                let mut col = windows::Win32::UI::Controls::LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH,
                    pszText: windows::core::PWSTR(wide.as_ptr() as *mut _),
                    cx: *width,
                    ..Default::default()
                };
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

            if let Ok(search_edit) = unsafe { GetDlgItem(hwnd, IDC_SEARCH_EDIT) } {
                unsafe {
                    let _ = SetWindowPos(search_edit, HWND_TOP, 0, 0, width - 20, 25, SWP_NOACTIVATE);
                }
            }
            if let Ok(list_view) = unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) } {
                unsafe {
                    let _ = SetWindowPos(list_view, HWND_TOP, 0, 0, width - 20, height - 55, SWP_NOACTIVATE);
                }
            }

            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;

            if id == IDC_SEARCH_EDIT && code == EN_CHANGE as u32 {
                refresh_list(hwnd);
            }

            LRESULT(0)
        }
        WM_CLOSE => {
            let data = unsafe { get_userdata::<PanelData>(hwnd) };
            if !data.is_null() {
                unsafe { (*data).visible = false; }
            }
            unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
            LRESULT(0)
        }
        WM_DESTROY => {
            let data = unsafe { get_userdata::<PanelData>(hwnd) };
            if !data.is_null() {
                unsafe { drop(Box::from_raw(data)); }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn refresh_list(hwnd: HWND) {
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() { return; }

    let Ok(list_view) = (unsafe { GetDlgItem(hwnd, IDC_LIST_VIEW) }) else { return };

    // 读取搜索文本
    let mut search_buf = [0u16; 256];
    let query = if let Ok(search_edit) = unsafe { GetDlgItem(hwnd, IDC_SEARCH_EDIT) } {
        let len = unsafe { GetWindowTextW(search_edit, &mut search_buf) } as usize;
        String::from_utf16_lossy(&search_buf[..len.min(255)]).trim().to_lowercase()
    } else {
        String::new()
    };

    let store = unsafe { (*data).tag_store.lock().unwrap() };

    // 清空列表
    unsafe { let _ = SendMessageW(list_view, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0)); }

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

        let inserted = unsafe {
            SendMessageW(list_view, LVM_INSERTITEMW, WPARAM(0), LPARAM(std::ptr::addr_of_mut!(item) as isize))
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

pub fn toggle_panel(hwnd: HWND) {
    let data = unsafe { get_userdata::<PanelData>(hwnd) };
    if data.is_null() { return; }

    let visible = unsafe { (*data).visible };
    if !visible {
        unsafe { (*data).visible = true; }
        refresh_list(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    } else {
        unsafe {
            (*data).visible = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}