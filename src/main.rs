use wintag::core;
use wintag::hotkey;
use wintag::sys;
use wintag::ui;

use core::tag::TagStore;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, IsIconic, IsWindow,
    IsWindowVisible, RegisterClassW, SetTimer, TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG,
    WINDOW_EX_STYLE, WM_HOTKEY, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};

use wintag::common::{self, widestring};

/// 兜底轮询定时器 ID（500ms 周期，捕获事件丢失/最小化窗口可见性误判）
const TIMER_POLL_OVERLAYS: usize = 0x1234;

type OverlayMap = HashMap<isize, sys::overlay::Overlay>;

/// 覆盖层存储：目标窗口句柄 → 覆盖层（仅主线程消息泵访问）
static OVERLAY_STORE: OnceLock<Arc<Mutex<OverlayMap>>> = OnceLock::new();
/// 概览面板窗口句柄
static PANEL_HWND: OnceLock<isize> = OnceLock::new();
/// 全局标签存储（供 WndProc 清理路径访问；与 `overlay::set_tag_store` 注入的是同一份 Arc）
static GLOBAL_TAG_STORE: OnceLock<Arc<Mutex<TagStore>>> = OnceLock::new();

fn main() -> anyhow::Result<()> {
    println!("WinTag 启动中...");

    // 声明 Per-Monitor V2 DPI 感知（必须在创建任何窗口之前调用）
    // SAFETY: SetProcessDpiAwarenessContext 为进程级设置，无参数生命周期问题；
    // 失败时降级到 V1，保证高 DPI 下覆盖层坐标与目标窗口物理像素一致。
    unsafe {
        if windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
        .is_err()
        {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
            );
        }
    }

    // 创建隐藏窗口（热键 + 覆盖层管理 + WinEvent 消息中转）
    let hwnd = create_hidden_window()?;

    // 初始化覆盖层存储（OnceLock 仅首次设置生效，重复调用忽略）
    if OVERLAY_STORE.get().is_none() {
        let _ = OVERLAY_STORE.set(Arc::new(Mutex::new(HashMap::new())));
    }

    // 共享的标签存储：注入覆盖层悬停查询，并登记到全局供清理路径（WM_TIMER/Forget）使用
    let tag_store: Arc<Mutex<TagStore>> = Arc::new(Mutex::new(TagStore::new()));
    sys::overlay::set_tag_store(Arc::clone(&tag_store));
    if GLOBAL_TAG_STORE.get().is_none() {
        let _ = GLOBAL_TAG_STORE.set(Arc::clone(&tag_store));
    }

    // 创建概览面板（隐藏）
    let panel_hwnd = ui::panel::create_panel(Arc::clone(&tag_store));
    if PANEL_HWND.get().is_none() {
        let _ = PANEL_HWND.set(panel_hwnd.0 as isize);
    }

    // 安装 WinEvent 事件监听：绑定隐藏窗口为转发目标，事件经 WM_APP_WINEVENT 分发。
    // winevent_hooks 作为 main 局部变量存活至退出，Drop 时自动注销 hook。
    // （用普通命名而非下划线前缀：它被 is_degraded() 实际使用）
    sys::win_event::bind_hidden(hwnd);
    let winevent_hooks = sys::win_event::install()?;
    if winevent_hooks.is_degraded() {
        eprintln!("[WinEvent] 监听降级为轮询模式（500ms 兜底同步）");
    } else {
        println!("[WinEvent] 事件监听已安装（系统段 + 对象段）");
    }

    // 注册全局热键
    hotkey::register_all(hwnd)?;
    println!("热键已注册：");
    println!("  Ctrl+Shift+N — 快速标记当前窗口");
    println!("  Ctrl+Shift+M — 打开概览面板");

    // 兜底轮询定时器：捕获 WinEvent 事件丢失 / 最小化窗口可见性误判
    // SAFETY: SetTimer 在消息循环前调用，hwnd 为存活窗口；失败仅返回 0，忽略即可
    // （事件驱动同步仍是主路径）。
    unsafe {
        let _ = SetTimer(hwnd, TIMER_POLL_OVERLAYS, 500, None);
    }

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
                        handle_quick_tag(Arc::clone(&store_clone), hwnd.0 as isize);
                    }
                    hotkey::Hotkey::TogglePanel => {
                        println!("[热键] Ctrl+Shift+M 触发");
                        if let Some(ph) = PANEL_HWND.get() {
                            ui::panel::toggle_panel(HWND(*ph as *mut std::ffi::c_void));
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

    // SAFETY: 注册自定义窗口类，类数据在窗口生命周期内保持有效
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    // SAFETY: 创建隐藏窗口，失败返回 Err 由 `?` 传播
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

/// 覆盖层数量上限（防御：伪造 WM_CREATE_OVERLAY 消息导致的资源耗尽 DoS）
///
/// 同权限进程可向隐藏窗口 `PostMessage` 伪造创建请求（见 doc/decision-records.md D1 威胁模型），
/// 限制同时存在的覆盖层总数，超出后拒绝新请求。
const MAX_OVERLAYS: usize = 256;

/// 隐藏窗口的窗口过程
extern "system" fn hidden_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        common::WM_CREATE_OVERLAY => {
            let target_hwnd = wparam.0 as isize;
            println!("[覆盖层] 创建请求: HWND={}", target_hwnd);
            if let Some(store) = OVERLAY_STORE.get() {
                match store.lock() {
                    Ok(mut overlays) => {
                        // 防御：伪造消息可能携带无效/任意句柄，先校验窗口真实存在
                        // SAFETY: IsWindow 为只读查询，句柄失效返回 FALSE，无副作用。
                        let valid = unsafe {
                            IsWindow(HWND(target_hwnd as *mut std::ffi::c_void)).as_bool()
                        };
                        if !valid {
                            eprintln!("[覆盖层] 拒绝无效窗口句柄: HWND={}", target_hwnd);
                            return LRESULT(0);
                        }
                        // 防御：覆盖层数量上限，防止伪造消息耗尽 USER/GDI 资源
                        if overlays.len() >= MAX_OVERLAYS {
                            eprintln!("[覆盖层] 数量已达上限 {}，拒绝创建", MAX_OVERLAYS);
                            return LRESULT(0);
                        }
                        match overlays.entry(target_hwnd) {
                            Entry::Vacant(v) => match sys::overlay::Overlay::create(target_hwnd) {
                                Ok(overlay) => {
                                    v.insert(overlay);
                                    println!("[覆盖层] 创建成功: HWND={}", target_hwnd);
                                }
                                Err(e) => {
                                    eprintln!("[覆盖层] 创建失败: {}", e);
                                }
                            },
                            Entry::Occupied(_) => {
                                // 该窗口已有覆盖层，忽略重复请求
                            }
                        }
                    }
                    Err(_) => {
                        eprintln!("[覆盖层] 存储锁中毒，跳过创建");
                    }
                }
            }
            LRESULT(0)
        }
        common::WM_DESTROY_OVERLAY => {
            let target_hwnd = wparam.0 as isize;
            println!("[覆盖层] 销毁请求: HWND={}", target_hwnd);
            if let Some(store) = OVERLAY_STORE.get() {
                match store.lock() {
                    Ok(mut overlays) => {
                        overlays.remove(&target_hwnd);
                    }
                    Err(_) => {
                        eprintln!("[覆盖层] 存储锁中毒，跳过销毁");
                    }
                }
            }
            LRESULT(0)
        }
        common::WM_APP_WINEVENT => {
            // 事件由 WinEvent 回调转发而来（wParam = 目标窗口, lParam = 事件编号）
            let target_hwnd = wparam.0 as isize;
            let event = lparam.0 as u32;
            handle_winevent(target_hwnd, event);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_POLL_OVERLAYS {
                poll_overlays();
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 处理 WinEvent 事件：按动作分类驱动覆盖层同步 / 显隐 / 清理
fn handle_winevent(target_hwnd: isize, event: u32) {
    use sys::win_event::WinEventAction;
    match sys::win_event::classify(event) {
        // 位置变化与前台切换都走同步：sync_position 内部使用 HWND_TOPMOST 天然置顶
        WinEventAction::Sync | WinEventAction::BringToTop => {
            with_overlay(target_hwnd, |overlay| {
                if let Err(e) = overlay.sync_position() {
                    eprintln!("[WinEvent] 同步覆盖层失败: {}", e);
                }
            });
        }
        WinEventAction::Hide => {
            with_overlay(target_hwnd, |overlay| overlay.hide());
        }
        WinEventAction::Show => {
            with_overlay(target_hwnd, |overlay| {
                overlay.show();
                if let Err(e) = overlay.sync_position() {
                    eprintln!("[WinEvent] 同步覆盖层失败: {}", e);
                }
            });
        }
        WinEventAction::Forget => {
            // 目标窗口正在销毁：不做任何窗口查询，直接清理覆盖层与标签
            forget_target(target_hwnd);
        }
        WinEventAction::Ignore => {}
    }
}

/// 在覆盖层存储中查找目标窗口的覆盖层并执行动作（不存在或锁中毒时静默）
fn with_overlay<F: FnOnce(&sys::overlay::Overlay)>(target_hwnd: isize, action: F) {
    let Some(store) = OVERLAY_STORE.get() else {
        return;
    };
    let Ok(overlays) = store.lock() else {
        return;
    };
    if let Some(overlay) = overlays.get(&target_hwnd) {
        action(overlay);
    }
}

/// 兜底轮询：验证目标窗口仍存活，并同步仍有效的覆盖层位置
///
/// 目标窗口已销毁 → 移除覆盖层（Drop 销毁窗口）并删除标签；
/// 目标最小化 / 隐藏 → 由 [`sys::overlay::Overlay::sync_position`] 内部自动隐藏覆盖层；
/// 目标恢复可见但覆盖层仍隐藏 → 调用 [`show`] 恢复显示（兜底事件丢失场景）。
fn poll_overlays() {
    let Some(store) = OVERLAY_STORE.get() else {
        return;
    };
    let Ok(mut overlays) = store.lock() else {
        return;
    };

    let mut stale: Vec<isize> = Vec::new();
    for (target_hwnd, overlay) in overlays.iter() {
        // SAFETY: IsWindow 为只读查询，句柄失效时返回 FALSE，不会产生未定义行为；
        // 校验通过后再交给 sync_position 处理定位。
        let alive = unsafe { IsWindow(HWND(*target_hwnd as *mut std::ffi::c_void)).as_bool() };
        if !alive {
            stale.push(*target_hwnd);
            continue;
        }
        // 恢复显示校正：仅当目标窗口**当前可见**（未最小化/未隐藏）且覆盖层仍隐藏时
        // 才调用 show()。若目标不可见，sync_position 内部会保持覆盖层隐藏，
        // 此处不触发 show()，避免"最小化→强制显示→立即隐藏"的每 500ms 闪烁循环。
        // SAFETY: IsIconic/IsWindowVisible 为只读查询，句柄失效返回 FALSE/0，无副作用。
        let target_visible = unsafe {
            !IsIconic(HWND(*target_hwnd as *mut std::ffi::c_void)).as_bool()
                && IsWindowVisible(HWND(*target_hwnd as *mut std::ffi::c_void)).as_bool()
        };
        if target_visible && !overlay.is_visible() {
            overlay.show();
        }
        if let Err(e) = overlay.sync_position() {
            eprintln!("[轮询] 同步覆盖层失败: {}", e);
        }
    }

    // 延迟到遍历结束后统一移除（避免迭代期间修改 HashMap）
    for target_hwnd in stale {
        // 移除即触发 Overlay::drop，自动销毁覆盖层窗口
        overlays.remove(&target_hwnd);
        remove_tag(target_hwnd);
    }
}

/// 忘记目标窗口：销毁其覆盖层并从标签存储删除记录
///
/// 注意：调用场景多为窗口销毁事件，此时不应查询窗口属性。
fn forget_target(target_hwnd: isize) {
    let removed = OVERLAY_STORE
        .get()
        .and_then(|store| store.lock().ok())
        .map(|mut overlays| overlays.remove(&target_hwnd).is_some())
        .unwrap_or(false);
    if removed {
        println!("[清理] 目标窗口已销毁，移除覆盖层: HWND={}", target_hwnd);
    }
    remove_tag(target_hwnd);
}

/// 从全局标签存储删除指定窗口的标签（锁中毒时静默）
fn remove_tag(target_hwnd: isize) {
    let Some(store) = GLOBAL_TAG_STORE.get() else {
        return;
    };
    if let Ok(mut tags) = store.lock() {
        tags.remove(&target_hwnd);
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

            let existing = store.lock().ok().and_then(|s| s.get(&info.hwnd).cloned());

            if let Some(tag) = existing {
                println!(
                    "窗口已有标签：{} ({}), 备注：{}",
                    tag.title, info.process_name, tag.note
                );
            }

            // 创建 Win32 弹窗（覆盖层创建已移到弹窗确认分支）
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
