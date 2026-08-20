use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetDlgItem, GetWindowTextW, PostMessageW,
    RegisterClassW, ShowWindow, BS_PUSHBUTTON, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, HMENU, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WNDCLASSW, WS_BORDER, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_SYSMENU, WS_VISIBLE,
    WS_VSCROLL,
};

use crate::common::{self, get_userdata, set_userdata, widestring};
use crate::core::matcher;
use crate::core::tag::{Tag, TagColor, TagStore};

const IDC_TITLE_EDIT: i32 = 101;
const IDC_NOTE_EDIT: i32 = 103;
const IDC_OK_BUTTON: i32 = 104;
const IDC_CANCEL_BUTTON: i32 = 105;

/// 弹窗窗口的用户数据（随 `lpCreateParams` 传入，`WM_DESTROY` 时释放）
struct PopupData {
    tag_store: Arc<Mutex<TagStore>>,
    target_hwnd: isize,
    window_title: String,
    process_name: String,
    hidden_hwnd: isize,
}

/// 创建“标记窗口”弹窗
///
/// 在主线程中为指定目标窗口弹出标签编辑弹窗，用于输入标签标题与备注。
/// 弹窗数据（[`PopupData`]）通过 `lpCreateParams` 传入，窗口销毁（`WM_DESTROY`）时释放；
/// 确认按钮触发标签写入成功后，才发送 `WM_CREATE_OVERLAY` 请求覆盖层创建。
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
    let data = Box::new(PopupData {
        tag_store: store,
        target_hwnd,
        window_title: window_title.to_string(),
        process_name: process_name.to_string(),
        hidden_hwnd,
    });
    // SAFETY: data 的所有权转交给弹窗窗口（作为 lpCreateParams 传入），窗口 WM_DESTROY 时
    // 通过 Box::from_raw 归还；若 CreateWindowExW 失败则在本函数内归还释放，均只释放一次。
    let data_ptr = Box::into_raw(data);

    let class_name = widestring("WinTagPopup");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(popup_wndproc),
        hInstance: HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    // SAFETY: RegisterClassW 注册窗口类；类已存在时返回失败，忽略即可（幂等）。
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    let style =
        WINDOW_STYLE((WS_OVERLAPPEDWINDOW.0 & !((WS_SYSMENU | WS_BORDER).0)) | WS_VISIBLE.0);
    let ex_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0);

    // SAFETY: CreateWindowExW 为线程安全标准 API；失败时归还 data_ptr 所有权并打印错误，
    // 提前返回，避免 Box 泄漏。
    match unsafe {
        CreateWindowExW(
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
    } {
        Ok(hwnd) => {
            // SAFETY: hwnd 为刚创建成功的有效窗口句柄。
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
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

            // 信息文本
            let info = format!("窗口：{}\r\n进程：{}", pd.window_title, pd.process_name);
            let info_wide = widestring(&info);
            // SAFETY: info_wide 为 NUL 结尾宽字符串且存活于调用期间；CreateWindowExW 为
            // 线程安全标准 API，返回值忽略（静态文本控件创建失败不影响弹窗功能）。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    PCWSTR(info_wide.as_ptr()),
                    child_style,
                    10,
                    10,
                    380,
                    40,
                    hwnd,
                    None,
                    instance,
                    None,
                );
            }

            // 标题标签
            // SAFETY: 同信息文本，静态标签创建失败忽略。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("标题："),
                    child_style,
                    10,
                    60,
                    50,
                    25,
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
                    60,
                    58,
                    330,
                    25,
                    hwnd,
                    HMENU(IDC_TITLE_EDIT as *mut std::ffi::c_void),
                    instance,
                    None,
                )
            };
            if let Ok(title_edit) = title_edit {
                // SAFETY: title_edit 为刚创建成功的有效子控件句柄；SetFocus 失败仅返回
                // Err，忽略即可（不影响弹窗功能）。
                unsafe {
                    let _ = SetFocus(title_edit);
                }
            }

            // 备注标签
            // SAFETY: 同信息文本，静态标签创建失败忽略。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("备注："),
                    child_style,
                    10,
                    100,
                    50,
                    25,
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
            unsafe {
                let _ = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    windows::core::w!("EDIT"),
                    PCWSTR(note_wide.as_ptr()),
                    note_ws,
                    10,
                    130,
                    380,
                    100,
                    hwnd,
                    HMENU(IDC_NOTE_EDIT as *mut std::ffi::c_void),
                    instance,
                    None,
                );
            }

            // 确认按钮
            let btn_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | BS_PUSHBUTTON as u32);
            // SAFETY: 按钮控件创建失败忽略，不影响其余子控件。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("BUTTON"),
                    windows::core::w!("确认"),
                    btn_style,
                    260,
                    245,
                    70,
                    28,
                    hwnd,
                    HMENU(IDC_OK_BUTTON as *mut std::ffi::c_void),
                    instance,
                    None,
                );
            }

            // 取消按钮
            // SAFETY: 按钮控件创建失败忽略，不影响其余子控件。
            unsafe {
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("BUTTON"),
                    windows::core::w!("取消"),
                    btn_style,
                    335,
                    245,
                    70,
                    28,
                    hwnd,
                    HMENU(IDC_CANCEL_BUTTON as *mut std::ffi::c_void),
                    instance,
                    None,
                );
            }

            LRESULT(0)
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
                IDC_OK_BUTTON => {
                    let mut title_buf = [0u16; 256];
                    // SAFETY: GetDlgItem 查询本窗口子控件，失败返回 Err 由调用方处理。
                    let title_hwnd = unsafe { GetDlgItem(hwnd, IDC_TITLE_EDIT) };
                    if let Ok(th) = title_hwnd {
                        // SAFETY: th 为有效子控件句柄；GetWindowTextW 写入栈上缓冲区，
                        // 长度受数组上限约束，无越界风险。
                        let title_len = unsafe { GetWindowTextW(th, &mut title_buf) } as usize;
                        let title = String::from_utf16_lossy(&title_buf[..title_len.min(255)])
                            .trim()
                            .to_string();

                        let mut note_buf = [0u16; 1024];
                        // SAFETY: GetDlgItem 查询本窗口子控件，失败返回 Err 由调用方处理。
                        let note_hwnd = unsafe { GetDlgItem(hwnd, IDC_NOTE_EDIT) };
                        let note = if let Ok(nh) = note_hwnd {
                            // SAFETY: nh 为有效子控件句柄；GetWindowTextW 写入栈上缓冲区，
                            // 长度受数组上限约束，无越界风险。
                            let note_len = unsafe { GetWindowTextW(nh, &mut note_buf) } as usize;
                            String::from_utf16_lossy(&note_buf[..note_len.min(1023)])
                                .trim()
                                .to_string()
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
                            color: TagColor::Orange,
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
                IDC_CANCEL_BUTTON => {
                    println!("取消标记窗口：{}", unsafe { &(*data).window_title });
                    // SAFETY: 取消统一走 WM_CLOSE 单一路径关闭弹窗（覆盖层无需销毁，见 WM_CLOSE）。
                    unsafe {
                        let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                _ => {}
            }
            LRESULT(0)
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
