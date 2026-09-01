#![windows_subsystem = "windows"]

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
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, BOOL, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT,
    WIN32_ERROR, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, HRGN, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, IsIconic, IsWindow,
    IsWindowVisible, KillTimer, PostMessageW, RegisterClassW, SetTimer, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, MSG, WINDOW_EX_STYLE, WM_HOTKEY, WM_QUIT, WM_SETTINGCHANGE, WM_TIMER,
    WNDCLASSW, WS_OVERLAPPED,
};

use wintag::common::{self, widestring};

/// 兜底轮询定时器 ID（500ms 周期，捕获事件丢失/最小化窗口可见性误判）
const TIMER_POLL_OVERLAYS: usize = 0x1234;
/// 托盘启动气泡一次性定时器 ID（1500ms 延迟到消息循环就绪后弹出，
/// 避免托盘图标注册初期 NIM_MODIFY 气泡被 shell 丢弃）
const TIMER_BALLOON: usize = 0x1235;

type OverlayMap = HashMap<isize, sys::overlay::Overlay>;

/// 覆盖层存储：目标窗口句柄 → 覆盖层（仅主线程消息泵访问）
static OVERLAY_STORE: OnceLock<Arc<Mutex<OverlayMap>>> = OnceLock::new();
/// 概览面板窗口句柄
static PANEL_HWND: OnceLock<isize> = OnceLock::new();
/// 全局标签存储（供 WndProc 清理路径访问；与 `overlay::set_tag_store` 注入的是同一份 Arc）
static GLOBAL_TAG_STORE: OnceLock<Arc<Mutex<TagStore>>> = OnceLock::new();
/// 主线程 → iced 线程的命令发送端（D27）
///
/// 镜像 `GLOBAL_TAG_STORE` 的注入模式：`main` 启动时写入一次，供
/// `request_exit`（有标签且未确认时弹退出确认窗）等按需发送
/// [`ui::iced_proto::IcedCommand`]，维持「主窗口对象只在主线程持有」的约定。
static ICED_CMD_TX: OnceLock<crossbeam_channel::Sender<ui::iced_proto::IcedCommand>> =
    OnceLock::new();

/// 判定单实例冲突：`CreateMutexW` 返回的句柄因同名命名互斥量已存在
/// （`GetLastError == ERROR_ALREADY_EXISTS`）时，说明另一 WinTag 实例正在运行。
///
/// 纯函数、无副作用，便于单元测试。
fn single_instance_conflict(err: WIN32_ERROR) -> bool {
    err == ERROR_ALREADY_EXISTS
}

/// 判定退出前是否需要弹确认窗：有标签数据且尚未确认时返回 true。
///
/// 纯函数、无副作用，便于单元测试。
fn should_confirm_exit(has_tags: bool, confirmed: bool) -> bool {
    has_tags && !confirmed
}

/// 解析命令行是否含 `--no-tray`（托盘常驻化禁用开关）
///
/// 遍历全部 `args`（含 argv[0]），任一项 `== "--no-tray"` 即返回 `true`。
/// 使用 `OsString` 逐项比较（非 UTF-8 argv 不 panic，仅做相等判定），
/// 不做前缀匹配以免误判 `--no-tray-extra` 之类的未知参数。
fn parse_cli_no_tray(args: &[std::ffi::OsString]) -> bool {
    args.iter().any(|arg| arg == "--no-tray")
}

fn main() -> anyhow::Result<()> {
    // 命令行参数处理（D22/R1）：`--config-dir` 由 core::settings::config_root()
    // 解析链消费（内部读取 args_os 并 memoize，见 settings.rs）；`--no-tray`
    // 请求禁用托盘图标/气泡，退出改走概览面板内"退出"按钮。
    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let no_tray = parse_cli_no_tray(&raw_args);

    // 单实例保护（D22）：创建命名互斥量 `WinTag_SingleInstance`（跨进程唯一）。
    // Windows 窗类注册是进程私有的，不能用于跨进程单实例（跨进程同名窗类不冲突，
    // 会产生多个实例）；命名互斥量才是标准、可靠的跨进程单实例机制。
    // 需要通过 `Win32_Security` feature（已加入 Cargo.toml）才能调用 CreateMutexW。
    let instance_name = widestring("WinTag_SingleInstance");
    // SAFETY: CreateMutexW 创建/打开命名互斥量；SECURITY_ATTRIBUTES 传 None（默认安全描述符），
    // 无长度参数问题。返回的 HANDLE 由本函数持有至 main 结束（进程退出时系统自动回收）。
    let instance_handle =
        unsafe { CreateMutexW(None, BOOL::from(false), PCWSTR(instance_name.as_ptr())) };
    if let Ok(handle) = instance_handle {
        // SAFETY: GetLastError 必须在 CreateMutexW 调用之后立即读取（线程本地最后错误）；
        // 此处紧随调用返回，中间无其他系统调用，可正确读取 ERROR_ALREADY_EXISTS。
        if single_instance_conflict(unsafe { GetLastError() }) {
            // 已存在另一实例：关闭本实例持有的互斥量句柄并退出（退 0）。
            // SAFETY: handle 为 CreateMutexW 返回的有效句柄，CloseHandle 释放它。
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Ok(());
        }
        // 首次创建（持互斥量直至进程退出，保证单实例）；句柄经 `_handle` 绑定，
        // 防止编译期未使用告警（进程退出时系统已自动回收，无需显式关闭）。
        let _hold = handle;
    }

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

    // D27：启动 iced 线程（四个 GUI 窗口的宿主，阶段 G0 仅退出确认窗）。
    // 主线程与 iced 线程经一对 crossbeam 通道双向通信：主线程发 IcedCommand、
    // iced 线程回 GuiEvent，契约见 ui::iced_proto。iced 以独立线程跑
    // `iced::daemon`，主线程 Win32 消息泵不受影响；线程退出随进程结束。
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ui::iced_proto::IcedCommand>();
    let (gui_tx, gui_rx) = crossbeam_channel::unbounded::<ui::iced_proto::GuiEvent>();
    let gui_dark = dark;
    let _ = std::thread::Builder::new()
        .name("wintag-gui".to_string())
        .spawn(move || {
            let result = iced::daemon(
                ui::iced_app::WinTagApp::title,
                ui::iced_app::WinTagApp::update,
                ui::iced_app::WinTagApp::view,
            )
            .subscription(ui::iced_app::WinTagApp::subscription)
            .theme(ui::iced_app::WinTagApp::theme)
            .run_with(move || ui::iced_app::WinTagApp::new(gui_tx, cmd_rx, gui_dark));
            if let Err(err) = result {
                eprintln!("[iced] 启动失败: {err:?}");
            }
        });
    // 登记主线程侧发送端，供 request_exit 等按需向 iced 线程发送命令
    let _ = ICED_CMD_TX.set(cmd_tx);

    // 创建设置窗口（初始隐藏，由热键 / WM_APP_OPEN_SETTINGS 切换显隐）。
    // 创建失败时句柄为 NULL 且 ui::settings::settings_hwnd() 全局记录缺失，
    // 热键/托盘打开设置时经 ensure_settings_window 懒创建重试，故仅保活不读取。
    let _settings_hwnd = ui::settings::create_settings(ui::settings::SettingsData {
        settings: Arc::clone(&settings),
        hidden_hwnd: hwnd.0 as isize,
        visible: false,
        theme_combo: HWND::default(),
        corner_combo: HWND::default(),
        theme_edit: HWND::default(),
        corner_edit: HWND::default(),
        title_check: HWND::default(),
        top_check: HWND::default(),
        balloon_check: HWND::default(),
    });

    // 安装 WinEvent 事件监听：绑定隐藏窗口为转发目标，事件经 WM_APP_WINEVENT 分发。
    // _winevent_hooks 作为 main 局部变量存活至退出，Drop 时自动注销 hook
    // （下划线前缀：值不再被读取，仅借 Drop 生命周期保活）。
    sys::win_event::bind_hidden(hwnd);
    let _winevent_hooks = sys::win_event::install()?;

    // 注册全局热键
    hotkey::register_all(hwnd)?;

    // 创建系统托盘图标（--no-tray 时跳过）；创建失败非致命，降级为无托盘模式
    // （概览面板/热键/设置均不受影响）。D26 由 tray-icon 承担，事件经
    // crossbeam channel 投递，由下方消息循环 try_recv 轮询分发。
    // TrayIcon 参考计数、最后实例 drop 时自动从系统托盘移除；以 main 局部
    // 变量持有至进程退出（退出流经 WM_QUIT 循环退出后即 drop）。
    let _tray = if !no_tray {
        Some(sys::tray::create_tray()?)
    } else {
        None
    };

    // 兜底轮询定时器：捕获 WinEvent 事件丢失 / 最小化窗口可见性误判
    // SAFETY: SetTimer 在消息循环前调用，hwnd 为存活窗口；失败仅返回 0，忽略即可
    // （事件驱动同步仍是主路径）。
    unsafe {
        let _ = SetTimer(hwnd, TIMER_POLL_OVERLAYS, 500, None);
    }

    // 启动气泡：托盘创建成功后经一次性定时器（1500ms）推迟到消息循环就绪后弹出。
    // 直接 NIM_MODIFY 常被 shell 在图标注册初期丢弃；延迟到循环内再弹更稳，
    // WM_TIMER 收到 TIMER_BALLOON 时 KillTimer 转为一次性。开关判定复用
    // sys::tray 纯函数（--no-tray 或配置关闭气泡时跳过）。
    if sys::tray::should_show_balloon(no_tray, cfg.show_balloon) {
        // SAFETY: hwnd 为存活隐藏窗口；SetTimer 失败返回 0，忽略（气泡丢失非致命）。
        unsafe {
            let _ = SetTimer(hwnd, TIMER_BALLOON, 1500, None);
        }
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

        // D26：托盘事件经 tray-icon 的 crossbeam channel 投递；D27：iced 线程
        // 事件经另一条 crossbeam channel 回投。此处非阻塞 try_recv 排空三通道
        // 并分发到现有窗口动作。
        pump_background_events(hwnd, &gui_rx);

        if msg.message == WM_HOTKEY {
            let hotkey = hotkey::from_message(msg.message, msg.wParam.0, msg.lParam.0);
            if let Some(hk) = hotkey {
                match hk {
                    hotkey::Hotkey::QuickTag => {
                        handle_quick_tag(Arc::clone(&store_clone), hwnd.0 as isize);
                    }
                    hotkey::Hotkey::TogglePanel => {
                        if let Some(ph) = PANEL_HWND.get() {
                            ui::panel::toggle_panel(HWND(*ph as *mut std::ffi::c_void));
                        }
                    }
                    hotkey::Hotkey::OpenSettings => {
                        // 设置窗口未创建时先懒创建（失败静默）
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
            if let Some(store) = OVERLAY_STORE.get() {
                // 锁中毒时静默跳过本次创建请求
                if let Ok(mut overlays) = store.lock() {
                    // 防御：伪造消息可能携带无效/任意句柄，先校验窗口真实存在
                    // SAFETY: IsWindow 为只读查询，句柄失效返回 FALSE，无副作用。
                    let valid =
                        unsafe { IsWindow(HWND(target_hwnd as *mut std::ffi::c_void)).as_bool() };
                    if !valid {
                        return LRESULT(0);
                    }
                    // 防御：覆盖层数量上限，防止伪造消息耗尽 USER/GDI 资源
                    if overlays.len() >= MAX_OVERLAYS {
                        return LRESULT(0);
                    }
                    match overlays.entry(target_hwnd) {
                        Entry::Vacant(v) => {
                            // 创建失败静默：下次事件/轮询仍会重试创建
                            if let Ok(overlay) = sys::overlay::Overlay::create(target_hwnd) {
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
                            }
                        }
                        Entry::Occupied(o) => {
                            // 该窗口已有覆盖层：强制重绘刷新标签内容/配色
                            // （重新标注同一窗口时标题条与颜色即时更新）
                            o.get().refresh();
                        }
                    }
                }
            }
            LRESULT(0)
        }
        common::WM_DESTROY_OVERLAY => {
            let target_hwnd = wparam.0 as isize;
            if let Some(store) = OVERLAY_STORE.get() {
                // 锁中毒时静默跳过本次销毁请求
                if let Ok(mut overlays) = store.lock() {
                    overlays.remove(&target_hwnd);
                }
            }
            LRESULT(0)
        }
        common::WM_APP_WINEVENT => {
            // 事件由 WinEvent 回调转发而来（wParam = 目标窗口, lParam = 事件编号）
            let target_hwnd = wparam.0 as isize;
            let event = lparam.0 as u32;
            // 传入隐藏窗口句柄：MoveStart/MoveEnd 需重设兜底轮询周期加速/恢复
            handle_winevent(hwnd, target_hwnd, event);
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
                return LRESULT(0);
            }
            let Some(store) = GLOBAL_TAG_STORE.get() else {
                return LRESULT(0);
            };
            let Ok(tags) = store.lock() else {
                return LRESULT(0);
            };
            let Some(tag) = tags.get(&target_hwnd) else {
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
        common::WM_APP_EXIT => {
            // 退出请求：面板"退出"按钮 wParam=0 仅请求；确认弹窗"确定" wParam=1
            // 已确认。request_exit 内部做"有标签且未确认 → 弹确认窗"判定。
            request_exit(hwnd, wparam.0 != 0);
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
            } else if wparam.0 == TIMER_BALLOON {
                // 一次性启动气泡：先 KillTimer 防重入，再经 notify-rust 弹出
                // （show_balloon 内部还会校验 balloon_enabled 注入开关）。
                // D26 由 notify-rust 走 Windows TOAST，不经 NIM_MODIFY。
                // SAFETY: hwnd 存活；KillTimer 失败仅返回 Err，忽略（防重入非关键）。
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_BALLOON);
                }
                sys::tray::show_balloon(
                    "WinTag",
                    "WinTag 已启动。点击查看已标注窗口，或按 Ctrl+Shift+M 打开概览",
                );
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 处理 WinEvent 事件：按动作分类驱动覆盖层同步 / 显隐 / 清理
fn handle_winevent(hidden_hwnd: HWND, target_hwnd: isize, event: u32) {
    use sys::win_event::WinEventAction;
    match sys::win_event::classify(event) {
        // 位置变化与前台切换都走同步：sync_position 内部使用 HWND_TOPMOST 天然置顶
        WinEventAction::Sync | WinEventAction::BringToTop => {
            with_overlay(target_hwnd, |overlay| {
                let _ = overlay.sync_position();
            });
        }
        WinEventAction::Hide => {
            with_overlay(target_hwnd, |overlay| overlay.hide());
        }
        WinEventAction::Show => {
            with_overlay(target_hwnd, |overlay| {
                overlay.show();
                let _ = overlay.sync_position();
            });
        }
        WinEventAction::Forget => {
            // 目标窗口正在销毁：不做任何窗口查询，直接清理覆盖层与标签
            forget_target(target_hwnd);
        }
        WinEventAction::MoveStart => {
            // 移动/缩放开始：加速兜底轮询（500→100ms），弥补拖拽期间
            // LOCATIONCHANGE 合并/提权窗口 UIPI 拦截事件导致的位置滞后（问题 18）。
            // 仅对已标记窗口加速，避免全局误触。
            let has_overlay = OVERLAY_STORE.get().is_some_and(|s| {
                s.lock()
                    .map(|m| m.contains_key(&target_hwnd))
                    .unwrap_or(false)
            });
            if has_overlay {
                set_poll_interval(hidden_hwnd, 100);
            }
        }
        WinEventAction::MoveEnd => {
            // 移动/缩放结束：强制最终同步（GetWindowRect 规避 DWM 陈旧值）+ 恢复轮询周期。
            with_overlay(target_hwnd, |overlay| {
                let _ = overlay.sync_position_force();
            });
            set_poll_interval(hidden_hwnd, 500);
        }
        WinEventAction::Ignore => {}
    }
}

/// 重设兜底轮询定时器周期（问题 18：拖拽期间加速到 100ms，结束后恢复 500ms）
///
/// `SetTimer` 以同一 ID 重复设置会直接改写周期（无需先 KillTimer），失败静默
/// （窗口仍存活，事件同步仍是主路径）。
fn set_poll_interval(hwnd: HWND, interval_ms: u32) {
    // SAFETY: hwnd 为主线程创建的隐藏窗口且存活；SetTimer 仅改定时器周期，
    // 失败返回 NULL（0），忽略即可（下一事件/既有定时器仍会兜底）。
    unsafe {
        let _ = SetTimer(hwnd, TIMER_POLL_OVERLAYS, interval_ms, None);
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
        let _ = overlay.sync_position();
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
    // 移除即触发 Overlay::drop，自动销毁覆盖层窗口（不存在或锁中毒时静默）
    if let Some(store) = OVERLAY_STORE.get() {
        if let Ok(mut overlays) = store.lock() {
            overlays.remove(&target_hwnd);
        }
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
    // 前台窗口信息获取失败时静默（无窗口可标记）
    if let Ok(info) = sys::window::get_foreground_window_info() {
        // 创建 Win32 弹窗（覆盖层创建已移到弹窗确认分支）
        ui::popup::create_popup(
            store,
            info.hwnd,
            &info.title,
            &info.process_name,
            hidden_hwnd,
        );
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
        balloon_check: HWND::default(),
    })
}

/// 排空托盘图标/菜单事件与 iced 事件三通道并分发高层命令（D26/D27）。
///
/// tray-icon 把图标点击/菜单选择事件写入静态 crossbeam channel；iced 线程把
/// 界面事件写入主线程侧的 crossbeam channel。主线程在消息循环 `GetMessageW`
/// 返回后非阻塞 `try_recv` 排空并分发——托盘事件经 [`sys::tray`] 纯映射为
/// [`TrayCommand`] 复用 [`dispatch_tray_command`]，iced 事件经
/// [`dispatch_iced_event`] 落到退出流（镜像热键分发语义）。
fn pump_background_events(
    hwnd: HWND,
    gui_rx: &crossbeam_channel::Receiver<ui::iced_proto::GuiEvent>,
) {
    while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
        if let Some(cmd) = sys::tray::icon_event_to_command(&event) {
            dispatch_tray_command(hwnd, cmd);
        }
    }
    while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
        if let Some(cmd) = sys::tray::menu_id_to_command(&event.id) {
            dispatch_tray_command(hwnd, cmd);
        }
    }
    while let Ok(event) = gui_rx.try_recv() {
        dispatch_iced_event(hwnd, event);
    }
}

/// 分发 iced 线程回投的界面事件（D27）
///
/// 当前仅退出确认流：确认 → 以 confirmed=true 重入规范退出流；取消 → 无动作
/// （iced 线程已在点击"取消"时自行关闭确认窗，主线程无需介入）。
fn dispatch_iced_event(hwnd: HWND, event: ui::iced_proto::GuiEvent) {
    match event {
        ui::iced_proto::GuiEvent::ConfirmExit => request_exit(hwnd, true),
        ui::iced_proto::GuiEvent::CancelExit => {}
    }
}

/// 分发托盘命令（右键菜单选择与图标单击解码结果共用，D26）
///
/// 镜像热键分发的动作语义：OpenPanel 切换概览面板、OpenSettings 确保创建后
/// 切换设置页、QuickTag 对前台窗口弹标记窗、Exit 走规范退出流。
fn dispatch_tray_command(hwnd: HWND, cmd: sys::tray::TrayCommand) {
    match cmd {
        sys::tray::TrayCommand::OpenPanel => {
            if let Some(ph) = PANEL_HWND.get() {
                ui::panel::toggle_panel(HWND(*ph as *mut std::ffi::c_void));
            }
        }
        sys::tray::TrayCommand::OpenSettings => {
            let shwnd = ensure_settings_window(hwnd.0 as isize);
            if shwnd != HWND::default() {
                // 取全局设置实例（未注入时回退默认实例，保证 toggle 不 panic）
                let settings = core::settings::global_settings()
                    .unwrap_or_else(|| Arc::new(Mutex::new(Settings::default())));
                ui::settings::toggle_settings(shwnd, hwnd.0 as isize, settings);
            }
        }
        sys::tray::TrayCommand::QuickTag => {
            if let Some(store) = GLOBAL_TAG_STORE.get() {
                handle_quick_tag(Arc::clone(store), hwnd.0 as isize);
            }
        }
        sys::tray::TrayCommand::Exit => request_exit(hwnd, false),
    }
}

/// 规范退出流（D22/D24）：有标签数据且未确认 → 弹确认窗；确认或无需确认 →
/// 投递 WM_QUIT，令 GetMessageW 返回 0、main 正常返回退出码 0。托盘图标由
/// [`sys::tray::create_tray`] 返回的 TrayIcon 在 main 退出时 drop 自动移除
/// （D26：tray-icon 参考计数，最后实例 drop 即从系统托盘移除）。
///
/// 入口有三：托盘右键菜单"退出"（未确认）、概览面板"退出"按钮（未确认，
/// wParam=0）、确认弹窗"确定"（已确认，wParam=1，经 WM_APP_EXIT 回投）。
fn request_exit(hwnd: HWND, confirmed: bool) {
    let tag_count = GLOBAL_TAG_STORE
        .get()
        .and_then(|s| s.lock().ok())
        .map(|tags| tags.len())
        .unwrap_or(0);
    if should_confirm_exit(tag_count > 0, confirmed) {
        // 交由 iced 线程弹退出确认窗（D27：四窗迁至 iced，Win32 confirm 不再用）。
        // 用户在确认窗点"退出"后 iced 回发 GuiEvent::ConfirmExit，主线程
        // dispatch_iced_event 以 confirmed=true 重入本函数完成退出；
        // 点"取消"则 iced 自行关闭窗口，本轮直接返回。
        if let Some(tx) = ICED_CMD_TX.get() {
            let _ = tx.send(ui::iced_proto::IcedCommand::ShowConfirm { count: tag_count });
        }
        return;
    }
    // SAFETY: PostMessageW 为线程安全投递 API；WM_QUIT 投递给本线程隐藏窗口，
    // 令主消息循环 GetMessageW 返回 0 优雅退出（_winevent_hooks 等随 main
    // 返回经 Drop 清理）。
    unsafe {
        let _ = PostMessageW(hwnd, WM_QUIT, WPARAM(0), LPARAM(0));
    }
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
        // SAFETY: RedrawWindow 带 RDW_ERASE 触发 WM_ERASEBKGND（父窗口背景按新调色板重绘，
        // 此前 InvalidateRect(...,FALSE) 因 bErase=FALSE 不擦背景，导致第一次切浅色时
        // 面板非子控件区域保留旧主题像素）；RDW_ALLCHILDREN 连子控件（含 owner-draw 按钮）
        // 一起带擦除失效重绘。
        unsafe {
            let _ = RedrawWindow(
                panel_hwnd,
                None,
                HRGN::default(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
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
        // SAFETY: 同面板——RDW_ERASE 触发 WM_ERASEBKGND 让设置窗口背景按新调色板重绘，
        // RDW_ALLCHILDREN 连 owner-draw 下拉框/按钮子控件一并带擦除重绘。
        unsafe {
            let _ = RedrawWindow(
                settings_hwnd,
                None,
                HRGN::default(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
    }

    // 重新注入 tooltip 配色（Mutex 可热更新，主题切换后新 tooltip 即时采用新配色，
    // 修复原先 OnceLock 一次性注入导致主题切换后 tooltip 沿用启动配色的遗留问题）
    sys::overlay::set_tooltip_theme(colors.tooltip_bg, colors.tooltip_fg);
    // 注入托盘气泡开关（sys 层镜像，设置保存广播后热更新；未注入时默认显示）
    sys::tray::set_balloon_enabled(cfg.show_balloon);
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

    /// ERROR_ALREADY_EXISTS → 判定为单实例冲突（命名互斥量已存在）
    #[test]
    fn single_instance_conflict_detects_duplicate() {
        assert!(single_instance_conflict(ERROR_ALREADY_EXISTS));
    }

    /// 其它错误码（0 / 任意值）→ 非冲突，继续启动
    #[test]
    fn single_instance_conflict_ignores_other_errors() {
        assert!(!single_instance_conflict(WIN32_ERROR(0)));
        assert!(!single_instance_conflict(WIN32_ERROR(5)));
    }

    /// 有标签且未确认 → 需要确认弹窗；其余组合（无标签/已确认）不需要
    #[test]
    fn should_confirm_exit_only_when_tags_and_unconfirmed() {
        assert!(should_confirm_exit(true, false));
        assert!(!should_confirm_exit(false, false));
        assert!(!should_confirm_exit(true, true));
        assert!(!should_confirm_exit(false, true));
    }

    // ---------- parse_cli_no_tray ----------

    /// 命令行含 `--no-tray`（任意位置，含 argv[0] 之后）→ true
    #[test]
    fn parse_no_tray_true_when_flag_present() {
        use std::ffi::OsString;
        let args = [
            OsString::from("wintag"),
            OsString::from("--no-tray"),
            OsString::from("--config-dir"),
            OsString::from("C:\\cfg"),
        ];
        assert!(parse_cli_no_tray(&args));
    }

    /// 命令行不含 `--no-tray`（仅 --config-dir / 无参数）→ false
    #[test]
    fn parse_no_tray_false_when_flag_absent() {
        use std::ffi::OsString;
        let args = [
            OsString::from("wintag"),
            OsString::from("--config-dir"),
            OsString::from("C:\\cfg"),
        ];
        assert!(!parse_cli_no_tray(&args));
        assert!(!parse_cli_no_tray(&[OsString::from("wintag")]));
    }

    /// 非 UTF-8 参数不 panic，且不误判为 --no-tray
    #[test]
    fn parse_no_tray_handles_non_utf8() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        let weird = OsString::from_wide(&[0xD800]);
        let args = [OsString::from("wintag"), weird];
        assert!(!parse_cli_no_tray(&args));
    }

    /// --no-tray 前若无其它参数干扰，靠 == 逐项比较而非前缀匹配
    #[test]
    fn parse_no_tray_does_not_match_prefix() {
        use std::ffi::OsString;
        let args = [OsString::from("wintag"), OsString::from("--no-tray-extra")];
        assert!(!parse_cli_no_tray(&args));
    }
}
