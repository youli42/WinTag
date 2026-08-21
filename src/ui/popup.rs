use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{SetBkColor, SetTextColor, HDC};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClassLongPtrW, GetDlgCtrlID, GetDlgItem,
    GetParent, GetWindowTextW, PostMessageW, RegisterClassW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, BS_PUSHBUTTON, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, GCLP_WNDPROC, GWLP_WNDPROC, HMENU,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_KEYDOWN, WNDCLASSW, WS_BORDER, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_SYSMENU, WS_VISIBLE,
    WS_VSCROLL,
};

use crate::common::{self, get_userdata, set_userdata, widestring};
use crate::core::matcher;
use crate::core::settings::ThemeMode;
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

            // 读取全局设置并应用暗色主题与圆角偏好（任务 T7）
            // global_settings 未注入（如独立测试环境）时回退默认设置（跟随系统 + 默认圆角）。
            // 需在子控件创建前完成：控件首次绘制即发 WM_CTLCOLOR* 请求配色，须先写入
            // THEME_STATE 保证画刷取色有值。
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
            // SAFETY: hwnd 为正在创建的弹窗窗口（WM_CREATE 期间有效）；
            // DWM 属性调用失败（如 Win10 不支持圆角属性）时静默忽略返回值。
            let _ = crate::ui::theme::apply_dark_mode(hwnd, dark);
            let _ = crate::ui::theme::apply_corner_preference(hwnd, settings.corner);

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
            let note_edit = unsafe {
                CreateWindowExW(
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
                )
            };
            if let Ok(note_edit) = note_edit {
                // 子类化备注编辑框：ESC 键转发给弹窗父窗口处理（问题 5.2）；
                // 回车键留给多行编辑框自身插入换行，不转发。
                // SAFETY: note_edit 为刚创建成功的有效子控件句柄，子类化仅替换
                // 实例窗口过程，无额外内存操作（由安全函数内部封装）。
                subclass_edit_for_keys(note_edit);
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
                IDC_OK_BUTTON => save_and_close(hwnd),
                IDC_CANCEL_BUTTON => cancel_and_close(hwnd, data),
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
                _ => {}
            }
            LRESULT(0)
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

/// 将 EDIT 子控件子类化：把回车（仅标题框）/ESC 键转发给父弹窗处理（问题 5.2）
///
/// 键盘消息（`WM_KEYDOWN`）只会投递给拥有键盘焦点的窗口——即编辑框本身；
/// 若不子类化，父弹窗收不到回车/ESC，`WM_KEYDOWN` 分支无法触发保存/取消。
/// 子类化过程仅做按键转发，其余消息经 `GetClassLongPtrW(GCLP_WNDPROC)` 取回的
/// EDIT 类原始窗口过程透传，不影响编辑框其他行为（如多行编辑框回车换行）。
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

/// 弹窗 EDIT 子控件的子类化窗口过程：转发回车/ESC 键给父弹窗，其余消息透传
///
/// - 标题编辑框（`IDC_TITLE_EDIT`）回车 → 转发父弹窗保存；
/// - 任一编辑框 ESC → 转发父弹窗取消；
/// - 备注编辑框（多行）回车不转发，保留编辑框自身插入换行行为；
/// - 其余消息调用 EDIT 类原始窗口过程处理。
unsafe extern "system" fn edit_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        let key = (wparam.0 & 0xFFFF) as u16;
        // SAFETY: GetDlgCtrlID 查询子控件 ID（创建时经 HMENU 传入）。
        let is_title = unsafe { GetDlgCtrlID(hwnd) } == IDC_TITLE_EDIT;
        if (is_title && key == VK_RETURN.0) || key == VK_ESCAPE.0 {
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
