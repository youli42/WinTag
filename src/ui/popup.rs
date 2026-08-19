use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetDlgItem, GetWindowTextW, PostMessageW,
    RegisterClassW, SetWindowLongPtrW, ShowWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    GWLP_USERDATA, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_DESTROY, WS_BORDER, WS_CHILD, WS_EX_TOPMOST,
    WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW, WS_SYSMENU, WS_VISIBLE, WS_VSCROLL,
    WS_EX_CLIENTEDGE, ES_AUTOHSCROLL, ES_MULTILINE, ES_AUTOVSCROLL, BS_PUSHBUTTON,
};

use crate::core::tag::{Tag, TagColor, TagStore};
use crate::core::matcher;

const IDC_TITLE_EDIT: i32 = 101;
const IDC_NOTE_EDIT: i32 = 103;
const IDC_OK_BUTTON: i32 = 104;
const IDC_CANCEL_BUTTON: i32 = 105;

/// 读取窗口用户数据（Save/Restore）
unsafe fn get_userdata<T>(hwnd: HWND) -> *mut T {
    let old = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, old);
    old as *mut T
}

struct PopupData {
    tag_store: Arc<Mutex<TagStore>>,
    target_hwnd: isize,
    window_title: String,
    process_name: String,
    hidden_hwnd: isize,
}

pub fn create_popup(
    store: Arc<Mutex<TagStore>>,
    target_hwnd: isize,
    window_title: &str,
    process_name: &str,
    hidden_hwnd: isize,
) {
    let data = Box::new(PopupData {
        tag_store: store,
        target_hwnd,
        window_title: window_title.to_string(),
        process_name: process_name.to_string(),
        hidden_hwnd,
    });
    let data_ptr = Box::into_raw(data);

    let class_name = widestring("WinTagPopup");

    let wc = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(popup_wndproc),
        hInstance: windows::Win32::Foundation::HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    unsafe { let _ = RegisterClassW(&wc); }

    let style = WINDOW_STYLE(
        (WS_OVERLAPPEDWINDOW.0 & !((WS_SYSMENU | WS_BORDER).0)) | WS_VISIBLE.0,
    );
    let ex_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0);

    unsafe {
        let hwnd = CreateWindowExW(
            ex_style,
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("标记窗口"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            420,
            320,
            None,
            None,
            None,
            Some(data_ptr as *const std::ffi::c_void),
        )
        .expect("创建弹窗失败");
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

extern "system" fn popup_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let data = unsafe {
                let cs = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
                cs.lpCreateParams as *mut PopupData
            };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize); }

            let instance = windows::Win32::Foundation::HINSTANCE::default();
            let child_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0);

            // 信息文本
            let info = unsafe {
                format!("窗口：{}\r\n进程：{}", (*data).window_title, (*data).process_name)
            };
            let info_wide: Vec<u16> = info.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    PCWSTR(info_wide.as_ptr()),
                    child_style,
                    10, 10, 380, 40,
                    hwnd, None, instance, None,
                );
            }

            // 标题标签
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("标题："),
                    child_style,
                    10, 60, 50, 25,
                    hwnd, None, instance, None,
                );
            }

            // 标题编辑框
            let title_ws = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | ES_AUTOHSCROLL as u32);
            let title_wide: Vec<u16> = unsafe {
                (*data).window_title.encode_utf16().chain(std::iter::once(0)).collect()
            };
            unsafe {
                let _ = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    PCWSTR(title_wide.as_ptr()),
                    title_ws,
                    60, 58, 330, 25,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(IDC_TITLE_EDIT as *mut std::ffi::c_void),
                    instance, None,
                );
            }

            // 备注标签
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("备注："),
                    child_style,
                    10, 100, 50, 25,
                    hwnd, None, instance, None,
                );
            }

            // 备注编辑框
            let note_ws = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_VSCROLL.0
                | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32);
            unsafe {
                let _ = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    windows::core::w!(""),
                    note_ws,
                    10, 130, 380, 100,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(IDC_NOTE_EDIT as *mut std::ffi::c_void),
                    instance, None,
                );
            }

            // 确认按钮
            let btn_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | BS_PUSHBUTTON as u32);
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("BUTTON"),
                    windows::core::w!("确认"),
                    btn_style,
                    260, 245, 70, 28,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(IDC_OK_BUTTON as *mut std::ffi::c_void),
                    instance, None,
                );
            }

            // 取消按钮
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("BUTTON"),
                    windows::core::w!("取消"),
                    btn_style,
                    335, 245, 70, 28,
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::HMENU(IDC_CANCEL_BUTTON as *mut std::ffi::c_void),
                    instance, None,
                );
            }

            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let data = unsafe { get_userdata::<PopupData>(hwnd) };
            if data.is_null() {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }

            match id {
                IDC_OK_BUTTON => {
                    let mut title_buf = [0u16; 256];
                    let title_hwnd = unsafe { GetDlgItem(hwnd, IDC_TITLE_EDIT) };
                    if let Ok(th) = title_hwnd {
                        let title_len = unsafe { GetWindowTextW(th, &mut title_buf) } as usize;
                        let title = String::from_utf16_lossy(&title_buf[..title_len.min(255)]).trim().to_string();

                        let mut note_buf = [0u16; 1024];
                        let note_hwnd = unsafe { GetDlgItem(hwnd, IDC_NOTE_EDIT) };
                        let note = if let Ok(nh) = note_hwnd {
                            let note_len = unsafe { GetWindowTextW(nh, &mut note_buf) } as usize;
                            String::from_utf16_lossy(&note_buf[..note_len.min(1023)]).trim().to_string()
                        } else {
                            String::new()
                        };

                        let tag = Tag {
                            title: if title.is_empty() {
                                unsafe { (*data).window_title.clone() }
                            } else {
                                title
                            },
                            note,
                            color: TagColor::Orange,
                            window_title: unsafe { (*data).window_title.clone() },
                            process_name: unsafe { (*data).process_name.clone() },
                        };

                        {
                            let mut store = unsafe { (*data).tag_store.lock().unwrap() };
                            matcher::upsert_tag(&mut store, unsafe { (*data).target_hwnd }, tag);
                        }
                        println!("已标记窗口：{}", unsafe { &(*data).window_title });
                    }
                    unsafe { let _ = DestroyWindow(hwnd); }
                }
                IDC_CANCEL_BUTTON => {
                    let hidden = unsafe { (*data).hidden_hwnd };
                    let target = unsafe { (*data).target_hwnd };
                    unsafe {
                        let _ = PostMessageW(
                            HWND(hidden as *mut std::ffi::c_void),
                            0x8000 + 2,
                            WPARAM(target as usize),
                            LPARAM(0),
                        );
                    }
                    println!("取消标记窗口：{}", unsafe { &(*data).window_title });
                    unsafe { let _ = DestroyWindow(hwnd); }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let data = unsafe { get_userdata::<PopupData>(hwnd) };
            if !data.is_null() {
                let hidden = unsafe { (*data).hidden_hwnd };
                let target = unsafe { (*data).target_hwnd };
                unsafe {
                    let _ = PostMessageW(
                        HWND(hidden as *mut std::ffi::c_void),
                        0x8000 + 2,
                        WPARAM(target as usize),
                        LPARAM(0),
                    );
                }
            }
            unsafe { let _ = DestroyWindow(hwnd); }
            LRESULT(0)
        }
        WM_DESTROY => {
            let data = unsafe { get_userdata::<PopupData>(hwnd) };
            if !data.is_null() {
                unsafe { drop(Box::from_raw(data)); }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}