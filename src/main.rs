use wintag::core;
use wintag::hotkey;
use wintag::sys;
use wintag::ui;

use core::tag::TagStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetMessageW, PostMessageW, RegisterClassW,
    TranslateMessage, DispatchMessageW, CS_HREDRAW, CS_VREDRAW, MSG, WINDOW_EX_STYLE,
    WM_HOTKEY, WNDCLASSW, WS_OVERLAPPED,
};

/// 自定义消息：创建覆盖层 (wParam = target_hwnd)
const WM_CREATE_OVERLAY: u32 = 0x8000 + 1;
/// 自定义消息：销毁覆盖层 (wParam = target_hwnd)
const WM_DESTROY_OVERLAY: u32 = 0x8000 + 2;

type OverlayMap = HashMap<isize, sys::overlay::Overlay>;

static OVERLAY_STORE: OnceLock<Arc<Mutex<OverlayMap>>> = OnceLock::new();
static PANEL_HWND: OnceLock<isize> = OnceLock::new();

fn main() -> anyhow::Result<()> {
    println!("WinTag 启动中...");

    // 创建隐藏窗口（热键 + 覆盖层管理）
    let hwnd = create_hidden_window()?;

    // 初始化覆盖层存储
    OVERLAY_STORE
        .set(Arc::new(Mutex::new(HashMap::new())))
        .unwrap();

    // 注册全局热键
    hotkey::register_all(hwnd)?;
    println!("热键已注册：");
    println!("  Ctrl+Shift+N — 快速标记当前窗口");
    println!("  Ctrl+Shift+M — 打开概览面板");

    // 共享的标签存储
    let tag_store: Arc<Mutex<TagStore>> = Arc::new(Mutex::new(TagStore::new()));

    // 设置全局标签存储引用（供覆盖层 WndProc 悬停查询）
    let _ = core::tag::TAG_STORE.set(Arc::clone(&tag_store));

    // 创建概览面板（隐藏）
    let panel_hwnd = ui::panel::create_panel(Arc::clone(&tag_store));
    PANEL_HWND.set(panel_hwnd.0 as isize).unwrap();

    // 运行 Windows 消息循环
    let store_clone = Arc::clone(&tag_store);
    let mut msg = MSG::default();

    loop {
        // SAFETY: GetMessageW 处理本线程所有窗口消息
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };

        if ret.0 == 0 {
            break;
        }

        if ret.0 == -1 {
            anyhow::bail!("GetMessage 错误");
        }

        if msg.message == WM_HOTKEY {
            let hotkey = hotkey::from_message(msg.message, msg.wParam.0, msg.lParam.0);
            if let Some(hk) = hotkey {
                match hk {
                    hotkey::Hotkey::QuickTag => {
                        println!("[热键] Ctrl+Shift+N 触发");
                        handle_quick_tag(
                            Arc::clone(&store_clone),
                            hwnd.0 as isize,
                        );
                    }
                    hotkey::Hotkey::TogglePanel => {
                        println!("[热键] Ctrl+Shift+M 触发");
                        if let Some(ph) = PANEL_HWND.get() {
                            ui::panel::toggle_panel(HWND(
                                *ph as *mut std::ffi::c_void,
                            ));
                        }
                    }
                }
            }
            continue;
        }

        // SAFETY: 标准消息翻译和分发
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }

    Ok(())
}

fn create_hidden_window() -> anyhow::Result<HWND> {
    let class_name = widestring("WinTagHiddenWnd");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(hidden_wndproc),
        hInstance: HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    // SAFETY: 注册自定义窗口类
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    // SAFETY: 创建隐藏窗口
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("WinTag"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
    }?;

    Ok(hwnd)
}

/// 隐藏窗口的窗口过程
extern "system" fn hidden_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE_OVERLAY => {
            let target_hwnd = wparam.0 as isize;
            println!("[覆盖层] 创建请求: HWND={}", target_hwnd);
            if let Some(store) = OVERLAY_STORE.get() {
                let mut overlays = store.lock().unwrap();
                if !overlays.contains_key(&target_hwnd) {
                    match sys::overlay::Overlay::create(target_hwnd) {
                        Ok(overlay) => {
                            overlays.insert(target_hwnd, overlay);
                            println!("[覆盖层] 创建成功: HWND={}", target_hwnd);
                        }
                        Err(e) => {
                            eprintln!("[覆盖层] 创建失败: {}", e);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY_OVERLAY => {
            let target_hwnd = wparam.0 as isize;
            println!("[覆盖层] 销毁请求: HWND={}", target_hwnd);
            if let Some(store) = OVERLAY_STORE.get() {
                let mut overlays = store.lock().unwrap();
                overlays.remove(&target_hwnd);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 处理快速标记热键
fn handle_quick_tag(store: Arc<Mutex<TagStore>>, hidden_hwnd: isize) {
    match sys::window::get_foreground_window_info() {
        Ok(info) => {
            println!(
                "[标记] 前台窗口: {} ({}), HWND={}",
                info.title, info.process_name, info.hwnd
            );

            let existing = {
                let store = store.lock().unwrap();
                store.get(&info.hwnd).cloned()
            };

            if let Some(tag) = existing {
                println!(
                    "窗口已有标签：{} ({}), 备注：{}",
                    tag.title, info.process_name, tag.note
                );
            }

            // 在主线程上创建覆盖层
            // SAFETY: PostMessage 发送覆盖层创建请求
            unsafe {
                let _ = PostMessageW(
                    HWND(hidden_hwnd as *mut std::ffi::c_void),
                    WM_CREATE_OVERLAY,
                    WPARAM(info.hwnd as usize),
                    LPARAM(0),
                );
            }

            // 创建 Win32 弹窗
            ui::popup::create_popup(
                store,
                info.hwnd,
                &info.title,
                &info.process_name,
                hidden_hwnd,
            );
        }
        Err(e) => {
            eprintln!("获取窗口信息失败: {}", e);
        }
    }
}

fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}