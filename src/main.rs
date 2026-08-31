use wintag::core;
use wintag::hotkey;
use wintag::sys;
use wintag::ui;

use core::settings::{Settings, ThemeMode};
use core::tag::TagStore;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_C_EVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, IsIconic, IsWindow,
    IsWindowVisible, PostMessageW, RegisterClassW, SetTimer, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, MSG, WINDOW_EX_STYLE, WM_HOTKEY, WM_QUIT, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
    WS_OVERLAPPED,
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
/// 隐藏窗口句柄（供 Ctrl+C 处理线程定向投递 WM_QUIT，触发主消息循环优雅退出）
static CTRL_C_WND: OnceLock<isize> = OnceLock::new();

/// 判断 Ctrl+C 处理函数是否应接管本次控制台事件（仅 CTRL_C_EVENT 且窗口已就绪时接管）
///
/// 纯函数、无副作用，便于单元测试。其余事件（CTRL_BREAK/CTRL_CLOSE/CTRL_LOGOFF/CTRL_SHUTDOWN）
/// 一律交回默认处理器，避免在系统关闭/注销等阶段做重活导致进程响应失败。
fn ctrl_c_handled(ctrl_type: u32, window_ready: bool) -> bool {
    ctrl_type == CTRL_C_EVENT && window_ready
}

/// Ctrl+C（CTRL_C_EVENT）控制台处理器
///
/// 目标：把 Ctrl+C 从"CRT 默认以 0xC000013A 强制终止"改为"走主消息循环优雅退出"。
/// 该回调由 CRT 在**独立线程**上调用，因此不能用 `PostQuitMessage`（它只投递到当前线程队列）；
/// 必须定向 `PostMessageW(hwnd, WM_QUIT)` 到主线程拥有的隐藏窗口，令 `GetMessageW` 返回 0，
/// `main` 正常返回 `Ok(())`，`winevent_hooks` Drop 注销 WinEvent hook，退出码为 0。
///
/// 返回 `TRUE` 表示已处理（阻止默认终止）；返回 `FALSE` 表示未接管（交回默认处理器）。
///
/// # Safety
///
/// - 本函数作为 `HANDLER_ROUTINE` 注册，运行在 CRT 为控制台事件创建的独立线程上；
/// - 仅调用线程安全的 `PostMessageW`，并把 `CTRL_C_WND`（`OnceLock`，启动时 set 后不再变）
///   读入局部变量，不访问任何可变共享状态；
/// - `CTRL_C_WND` 在调用 `SetConsoleCtrlHandler` 之前 set，故回调触发时窗口必已登记。
unsafe extern "system" fn ctrl_c_handler(ctrl_type: u32) -> BOOL {
    if ctrl_c_handled(ctrl_type, CTRL_C_WND.get().is_some()) {
        if let Some(&h) = CTRL_C_WND.get() {
            let _ = PostMessageW(
                HWND(h as *mut std::ffi::c_void),
                WM_QUIT,
                WPARAM(0),
                LPARAM(0),
            );
            return TRUE;
        }
    }
    FALSE
}

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

    // 加载配置并注入全局设置（缺失/损坏时回退默认，见 core::settings::load）；
    // 该 Arc 与设置页窗口共享同一实例，保存后经 WM_APP_THEME_CHANGED 广播重应用。
    let settings: Arc<Mutex<Settings>> = Arc::new(Mutex::new(core::settings::load()));
    core::settings::set_global_settings(Arc::clone(&settings));

    // 创建隐藏窗口（热键 + 覆盖层管理 + WinEvent 消息中转）
    let hwnd = create_hidden_window()?;

    // 注册 Ctrl+C 处理器（问题 3）：先登记窗口句柄（handler 触发时须已就绪），再注册。
    // 失败不致命：回退到 CRT 默认终止（仍能退出，只是非零码且不执行 drop 清理）。
    let _ = CTRL_C_WND.set(hwnd.0 as isize);
    // SAFETY: CTRL_C_WND 已 set；handler 只做 PostMessageW/WM_QUIT 线程安全投递。
    if unsafe { SetConsoleCtrlHandler(Some(ctrl_c_handler), TRUE) }.is_err() {
        eprintln!("[退出] Ctrl+C 处理器注册失败，回退默认终止行为");
    }

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
    // 注入覆盖层的消息中转目标（R5：角标/标题条单击 → WM_APP_EDIT_TAG 请求编辑）
    sys::overlay::set_message_target(hwnd.0 as isize);

    // 创建概览面板（隐藏）
    let panel_hwnd = ui::panel::create_panel(Arc::clone(&tag_store), hwnd.0 as isize);
    if PANEL_HWND.get().is_none() {
        let _ = PANEL_HWND.set(panel_hwnd.0 as isize);
    }

    // 解析并注入主题：按配置主题 + 系统深浅色解析调色板并应用到隐藏窗口。
    // 面板/设置窗口在各自 WM_CREATE 中读取同一全局调色板（theme_colors），
    // 此处先 set_theme 保证创建期 WM_CTLCOLOR* 取到正确配色。
    let cfg = settings.lock().ok().map(|guard| *guard).unwrap_or_default();
    let system_dark = ui::theme::detect_system_dark();
    let colors = ui::theme::resolve_colors(cfg.theme, system_dark);
    ui::theme::set_theme(colors);
    // 暗色判定：显式深色，或跟随系统且系统当前为深色
    let dark = cfg.theme == ThemeMode::Dark || (cfg.theme == ThemeMode::System && system_dark);
    // SAFETY: hwnd 为刚创建的隐藏窗口，窗口存活；DWM 属性调用失败
    // （如旧版系统不支持圆角属性）时静默忽略返回值。
    let _ = ui::theme::apply_dark_mode(hwnd, dark);
    let _ = ui::theme::apply_corner_preference(hwnd, cfg.corner);

    // 注入 tooltip 配色与标题条显示开关（Mutex/AtomicBool 可热更新：
    // reapply_theme 在设置保存广播后重新注入，新内容即时采用新配色/开关）
    sys::overlay::set_tooltip_theme(colors.tooltip_bg, colors.tooltip_fg);
    sys::overlay::set_show_title(cfg.show_badge_title);
    sys::overlay::set_badge_always_top(cfg.badge_always_top);

    // 创建设置窗口（初始隐藏，由热键 / WM_APP_OPEN_SETTINGS 切换显隐）
    let settings_hwnd = ui::settings::create_settings(ui::settings::SettingsData {
        settings: Arc::clone(&settings),
        hidden_hwnd: hwnd.0 as isize,
        visible: false,
        theme_combo: HWND::default(),
        corner_combo: HWND::default(),
        theme_edit: HWND::default(),
        corner_edit: HWND::default(),
        title_check: HWND::default(),
        top_check: HWND::default(),
    });
    if settings_hwnd == HWND::default() {
        eprintln!("[设置] 设置窗口创建失败，热键仍可用（打开时自动重试创建）");
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
    println!("  Ctrl+Shift+S — 打开设置页面");

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
                    hotkey::Hotkey::OpenSettings => {
                        println!("[热键] Ctrl+Shift+S 触发");
                        // 设置窗口未创建时先懒创建（失败打印告警后静默）
                        let shwnd = ensure_settings_window(hwnd.0 as isize);
                        if shwnd != HWND::default() {
                            ui::settings::toggle_settings(
                                shwnd,
                                hwnd.0 as isize,
                                Arc::clone(&settings),
                            );
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
                                    // 创建后立即强制一次重绘（与设置保存后 reapply_theme
                                    // 的 refresh() 恢复路径一致）：确保首次 UpdateLayeredWindow
                                    // 内容生效，角标立即可见。
                                    overlay.refresh();
                                    // 立即建立 z 序：覆盖层带 WS_EX_TOPMOST 但未调用
                                    // sync_position 时，若目标窗口同属 topmost 带且被激活，
                                    // 会压住覆盖层导致角标被遮挡。此处重申 HWND_TOPMOST（或
                                    // insert-after 目标窗口）消除创建后到首次事件/轮询间
                                    // 的 z 序空窗期。
                                    let _ = overlay.sync_position();
                                    v.insert(overlay);
                                    println!("[覆盖层] 创建成功: HWND={}", target_hwnd);
                                }
                                Err(e) => {
                                    eprintln!("[覆盖层] 创建失败: {}", e);
                                }
                            },
                            Entry::Occupied(o) => {
                                // 该窗口已有覆盖层：强制重绘刷新标签内容/配色
                                // （重新标注同一窗口时标题条与颜色即时更新）
                                o.get().refresh();
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
        common::WM_APP_OPEN_SETTINGS => {
            // 打开设置窗口请求：未创建时先懒创建，再切换显隐。
            // 隐藏窗口句柄即本窗口（hwnd），供设置页保存后广播主题变更回传。
            let shwnd = ensure_settings_window(hwnd.0 as isize);
            if shwnd != HWND::default() {
                // 取全局设置实例（未注入时回退默认实例，保证 toggle 不 panic）
                let settings = core::settings::global_settings()
                    .unwrap_or_else(|| Arc::new(Mutex::new(Settings::default())));
                ui::settings::toggle_settings(shwnd, hwnd.0 as isize, settings);
            }
            LRESULT(0)
        }
        common::WM_APP_EDIT_TAG => {
            // 角标/标题条单击或面板右键菜单"编辑标签"（R5/R16）：
            // 目标窗口仍存活且已有标签时，打开预填编辑弹窗
            let target_hwnd = wparam.0 as isize;
            // SAFETY: IsWindow 为只读查询，句柄失效返回 FALSE，无副作用。
            let valid = unsafe { IsWindow(HWND(target_hwnd as *mut std::ffi::c_void)).as_bool() };
            if !valid {
                eprintln!("[编辑] 拒绝无效窗口句柄: HWND={}", target_hwnd);
                return LRESULT(0);
            }
            let Some(store) = GLOBAL_TAG_STORE.get() else {
                return LRESULT(0);
            };
            let Ok(tags) = store.lock() else {
                return LRESULT(0);
            };
            let Some(tag) = tags.get(&target_hwnd) else {
                eprintln!("[编辑] 目标窗口无标签: HWND={}", target_hwnd);
                return LRESULT(0);
            };
            let window_title = tag.window_title.clone();
            let process_name = tag.process_name.clone();
            drop(tags);
            ui::popup::create_popup(
                Arc::clone(store),
                target_hwnd,
                &window_title,
                &process_name,
                hwnd.0 as isize,
            );
            LRESULT(0)
        }
        common::WM_APP_THEME_CHANGED => {
            // 设置页保存后广播：重新读取全局设置并应用到所有已知窗口
            reapply_theme(hwnd);
            LRESULT(0)
        }
        common::WM_APP_TAGS_CHANGED => {
            // 便签弹窗保存标签后广播：转发给概览面板刷新树形列表
            // （镜像 WM_APP_THEME_CHANGED 的注入/广播模式，ui 层不直接依赖面板句柄）
            if let Some(&panel) = PANEL_HWND.get() {
                if panel != 0 {
                    let panel_hwnd = HWND(panel as *mut std::ffi::c_void);
                    // SAFETY: IsWindowVisible 为只读查询，面板句柄由 main 启动时
                    // 写入且窗口随进程存活，无生命周期风险。
                    if unsafe { IsWindowVisible(panel_hwnd) }.as_bool() {
                        // SAFETY: PostMessageW 为线程安全投递 API，wParam/lparam
                        // 原样透传（wParam = 目标窗口句柄），面板自行取用。
                        unsafe {
                            let _ = PostMessageW(
                                panel_hwnd,
                                common::WM_APP_TAGS_CHANGED,
                                wparam,
                                lparam,
                            );
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // 系统设置变更（如系统主题切换）：仅"跟随系统"模式需要重新检测，
            // 其余模式由 WM_APP_THEME_CHANGED 覆盖，避免无谓的注册表读取。
            let theme = core::settings::global_settings()
                .and_then(|s| s.lock().ok().map(|guard| guard.theme))
                .unwrap_or(ThemeMode::System);
            if theme == ThemeMode::System {
                reapply_theme(hwnd);
            }
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

/// 确保设置窗口已创建，返回其窗口句柄（懒创建）
///
/// 已创建（[`ui::settings::settings_hwnd`] 非 None）时直接复用；
/// 未创建时用当前全局设置（未注入时回退默认实例）调用
/// [`ui::settings::create_settings`] 创建（初始隐藏，由调用方切换显隐）。
/// 创建失败返回默认（NULL）句柄，调用方自行决定是否忽略。
fn ensure_settings_window(hidden_hwnd: isize) -> HWND {
    if let Some(sh) = ui::settings::settings_hwnd() {
        return HWND(sh as *mut std::ffi::c_void);
    }
    let settings = core::settings::global_settings()
        .unwrap_or_else(|| Arc::new(Mutex::new(Settings::default())));
    ui::settings::create_settings(ui::settings::SettingsData {
        settings,
        hidden_hwnd,
        visible: false,
        theme_combo: HWND::default(),
        corner_combo: HWND::default(),
        theme_edit: HWND::default(),
        corner_edit: HWND::default(),
        title_check: HWND::default(),
        top_check: HWND::default(),
    })
}

/// 从全局设置重新解析主题并应用到所有已知窗口（主题/设置变更统一入口）
///
/// 供 hidden_wndproc 处理 `WM_APP_THEME_CHANGED`（设置页保存后广播）与
/// `WM_SETTINGCHANGE`（系统主题切换，跟随系统模式）时调用：
/// 重读设置 → 重解析调色板 → 更新全局主题 → 对 hidden/panel/settings 窗口
/// 重新应用 DWM 暗色与圆角属性并强制重绘（WM_CTLCOLOR* 换新配色）。
/// 同时重新注入覆盖层的 tooltip 配色与标题条开关，并强制所有已存在的
/// 覆盖层重绘（角标描边色 / 标题条配色与开关即时生效）。
fn reapply_theme(hidden_hwnd: HWND) {
    // 重读全局设置（未注入或锁中毒时回退默认）
    let cfg = core::settings::global_settings()
        .and_then(|s| s.lock().ok().map(|guard| *guard))
        .unwrap_or_default();
    let system_dark = ui::theme::detect_system_dark();
    let colors = ui::theme::resolve_colors(cfg.theme, system_dark);
    // 先更新全局调色板，确保后续 WM_CTLCOLOR* 重绘取到新配色
    ui::theme::set_theme(colors);
    // 暗色判定：显式深色，或跟随系统且系统当前为深色
    let dark = cfg.theme == ThemeMode::Dark || (cfg.theme == ThemeMode::System && system_dark);

    // 隐藏窗口：DWM 属性随主题切换即时更新
    // SAFETY: hidden_hwnd 由调用方保证存活；DWM 调用失败仅返回布尔值，忽略。
    let _ = ui::theme::apply_dark_mode(hidden_hwnd, dark);
    let _ = ui::theme::apply_corner_preference(hidden_hwnd, cfg.corner);

    // 概览面板（经 PANEL_HWND）：DWM 属性 + ListView 主题刷新 + 强制重绘
    if let Some(ph) = PANEL_HWND.get() {
        let panel_hwnd = HWND(*ph as *mut std::ffi::c_void);
        // SAFETY: panel_hwnd 由 create_panel 成功后写入 PANEL_HWND，窗口存活。
        let _ = ui::theme::apply_dark_mode(panel_hwnd, dark);
        let _ = ui::theme::apply_corner_preference(panel_hwnd, cfg.corner);
        // 刷新树形列表主题（DarkMode_Explorer 热更新，问题 9.3/9.5）
        ui::panel::reapply_tree_theme(panel_hwnd, dark);
        // 子控件主题变体热更新（D17）
        ui::theme::apply_control_theme(panel_hwnd, dark);
        // SAFETY: InvalidateRect 仅标记重绘区域，由消息循环触发 WM_PAINT 重绘。
        unsafe {
            let _ = InvalidateRect(panel_hwnd, None, FALSE);
        }
    }

    // 设置窗口（经 settings_hwnd；未创建时跳过）
    if let Some(sh) = ui::settings::settings_hwnd() {
        let settings_hwnd = HWND(sh as *mut std::ffi::c_void);
        // SAFETY: settings_hwnd 由 create_settings 成功后写入，窗口存活。
        let _ = ui::theme::apply_dark_mode(settings_hwnd, dark);
        let _ = ui::theme::apply_corner_preference(settings_hwnd, cfg.corner);
        // 子控件主题变体热更新（D17）：下拉框/复选框随主题切换
        ui::theme::apply_control_theme(settings_hwnd, dark);
        // SAFETY: InvalidateRect 仅标记重绘区域，由消息循环触发 WM_PAINT 重绘。
        unsafe {
            let _ = InvalidateRect(settings_hwnd, None, FALSE);
        }
    }

    // 重新注入 tooltip 配色（Mutex 可热更新，主题切换后新 tooltip 即时采用新配色，
    // 修复原先 OnceLock 一次性注入导致主题切换后 tooltip 沿用启动配色的遗留问题）
    sys::overlay::set_tooltip_theme(colors.tooltip_bg, colors.tooltip_fg);
    // 重新注入标题条显示开关（R6），并强制所有已存在的覆盖层重绘：
    // 主题切换后角标描边色 / 标题条配色即时更新，开关切换即时生效。
    sys::overlay::set_show_title(cfg.show_badge_title);
    // R19：角标置顶开关注入，并对已存在的覆盖层立刻重排 z 序（新覆盖层
    // 创建时读取开关，旧覆盖层依赖事件/500ms 轮询也会自行收敛，此处即时生效）
    sys::overlay::set_badge_always_top(cfg.badge_always_top);
    if let Some(store) = OVERLAY_STORE.get() {
        if let Ok(overlays) = store.lock() {
            for overlay in overlays.values() {
                overlay.refresh();
                let _ = overlay.sync_position();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CTRL_C_EVENT 且隐藏窗口已就绪 → 应接管（返回 TRUE）
    #[test]
    fn ctrl_c_handled_takes_event_when_c_and_ready() {
        assert!(ctrl_c_handled(CTRL_C_EVENT, true));
    }

    /// CTRL_C_EVENT 但窗口未就绪 → 不应接管（返回 FALSE）
    #[test]
    fn ctrl_c_handled_ignores_c_when_window_not_ready() {
        assert!(!ctrl_c_handled(CTRL_C_EVENT, false));
    }

    /// 其它控制台事件（如 CTRL_CLOSE_EVENT=2）即使窗口就绪也不接管
    #[test]
    fn ctrl_c_handled_ignores_non_c_event() {
        assert!(!ctrl_c_handled(2, true));
    }

    /// 未知事件编号不接管
    #[test]
    fn ctrl_c_handled_ignores_unknown_event() {
        assert!(!ctrl_c_handled(999, true));
    }
}
