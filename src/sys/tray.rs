//! 托盘适配层（D26：底层交给 `tray-icon`(tauri)）。
//!
//! 本模块不再手写任何 `Shell_NotifyIconW` / `NOTIFYICONDATAW` / 菜单构建
//! Win32 代码，统一由 [`tray_icon`] crate（tauri）承担：
//! - [`create_tray`] 构建系统托盘图标（图标取自嵌入资源 `ID=1`）；
//! - [`TrayIconEvent`] / [`MenuEvent`] 经 crossbeam channel 投递，由主线程
//!   在消息循环非阻塞 `try_recv` 轮询（见 `main.rs`）；
//! - [`show_balloon`] 改由 [`notify_rust`] 发送 Windows TOAST 系统通知。
//!
//! 保留"纯逻辑层 + 适配层"分层：命令映射（`TrayCommand` → 现有窗口动作）
//! 与事件解码保持纯函数，便于单测与复用。
//!
//! 依赖方向约束（`ui → core → sys`）：本层不源码级依赖 `core`/`ui`，仅
//! 产生 [`TrayCommand`]，由主线程经 [`crate`] 分发到现有窗口动作。

use std::sync::atomic::{AtomicBool, Ordering};

use tray_icon::{
    menu::{Menu, MenuId, MenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

/// 托盘右键菜单项 id：打开概览面板（[`MenuEvent`] 按 id 解码为 [`TrayCommand`]）
const MENU_ID_OPEN_PANEL: &str = "open_panel";
/// 托盘右键菜单项 id：打开设置页
const MENU_ID_OPEN_SETTINGS: &str = "open_settings";
/// 托盘右键菜单项 id：快速标记
const MENU_ID_QUICK_TAG: &str = "quick_tag";
/// 托盘右键菜单项 id：退出
const MENU_ID_EXIT: &str = "exit";

/// 嵌入资源中的应用图标 ordinal（build.rs `set_icon` 写入 ID=1）
const APP_ICON_RESOURCE_ID: u16 = 1;

/// 托盘气泡显示开关（设置项 `show_balloon` 的 sys 层注入镜像）
///
/// 依赖方向约束不允许 sys 层读取 `core::settings`，因此由主线程经
/// [`set_balloon_enabled`] 注入（启动时 + 设置保存广播后）。
/// 未注入时默认显示；[`show_balloon`] 实际弹出前读取本开关。
/// 注入模式镜像 `sys::overlay::set_show_title`。
static BALLOON_ENABLED: AtomicBool = AtomicBool::new(true);

// =====================================================================
// 纯逻辑层：TrayCommand + 事件/待命解码（零 tray-icon 依赖，可单测）
// =====================================================================

/// 托盘交互解码后的高层界面命令。
///
/// 只描述"托盘交互应触发什么界面动作"，不执行任何 Win32/托盘操作，
/// 由主线程经 `dispatch_tray_command` 分发到现有窗口动作：
/// - [`TrayCommand::OpenPanel`]：打开全局概览面板；
/// - [`TrayCommand::OpenSettings`]：打开设置页面；
/// - [`TrayCommand::QuickTag`]：为当前活动窗口快速打标签；
/// - [`TrayCommand::Exit`]：退出程序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// 打开全局概览面板。
    OpenPanel,
    /// 打开设置页面。
    OpenSettings,
    /// 为当前活动窗口快速打标签。
    QuickTag,
    /// 退出程序。
    Exit,
}

/// 把托盘图标点击事件解码为高层命令。
///
/// 映射规则：
/// - [`TrayIconEvent::Click`] 且按钮为 `Left`（左键单击托盘图标）→
///   `Some(TrayCommand::OpenPanel)`；
/// - 其余（右键 `Right`、双击 `DoubleClick`、`Enter`/`Move`/`Leave`）→ `None`。
///
/// 右键单击由 [`tray_icon`] 配置 `with_menu_on_right_click(true)` 自动弹出
/// 上下文菜单，其选择经 [`MenuEvent`] 由 [`menu_id_to_command`] 解码，
/// 均不属于本函数职责。该函数为纯函数，便于单测与复用。
pub fn icon_event_to_command(event: &TrayIconEvent) -> Option<TrayCommand> {
    match event {
        TrayIconEvent::Click {
            button,
            button_state,
            ..
        } => {
            if matches!(button, tray_icon::MouseButton::Left)
                && matches!(button_state, tray_icon::MouseButtonState::Up)
            {
                Some(TrayCommand::OpenPanel)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 把菜单事件解码为高层命令。
///
/// 按 [`MenuId`] 区分四项菜单（打开概览面板/打开设置页/快速标记/退出）。
/// 未知 id 返回 `None`（调用方忽略）。该函数为纯函数，便于单测。
pub fn menu_id_to_command(id: &MenuId) -> Option<TrayCommand> {
    match id.as_ref() {
        MENU_ID_OPEN_PANEL => Some(TrayCommand::OpenPanel),
        MENU_ID_OPEN_SETTINGS => Some(TrayCommand::OpenSettings),
        MENU_ID_QUICK_TAG => Some(TrayCommand::QuickTag),
        MENU_ID_EXIT => Some(TrayCommand::Exit),
        _ => None,
    }
}

/// 判定托盘启动气泡提示是否应显示。
///
/// 仅当托盘图标存在（`no_tray == false`）且配置开启气泡（`show_balloon == true`）
/// 时返回 `true`；托盘禁用时无论配置如何都不弹气泡。
pub fn should_show_balloon(no_tray: bool, show_balloon: bool) -> bool {
    !no_tray && show_balloon
}

// =====================================================================
// 适配层：托盘图标创建/销毁、气泡通知（依赖 tray-icon / notify-rust）
// =====================================================================

/// 注入托盘气泡显示开关（[`show_balloon`] 实际弹出前读取本开关）
pub fn set_balloon_enabled(enabled: bool) {
    BALLOON_ENABLED.store(enabled, Ordering::Relaxed);
}

/// 读取托盘气泡显示开关（未注入时默认 `true`）
pub fn balloon_enabled() -> bool {
    BALLOON_ENABLED.load(Ordering::Relaxed)
}

/// 构建四项托盘右键菜单（打开概览面板/打开设置页/快速标记/退出）。
///
/// 菜单项经 `MenuId` 关联到命令，用户选择时 [`MenuEvent`] 携带该 id，
/// 由 [`menu_id_to_command`] 解码。
fn build_menu() -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_OPEN_PANEL,
        "打开概览面板",
        true,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_OPEN_SETTINGS,
        "打开设置页",
        true,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_QUICK_TAG,
        "快速标记",
        true,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(MENU_ID_EXIT, "退出", true, None));
    menu
}

/// 加载托盘图标：从嵌入资源（ID=1）读取。
///
/// 图标经 build.rs 的 `winresource::set_icon` 嵌入到 exe 资源 ID=1，
/// [`Icon::from_resource`] 内层用 `LoadImageW` + `LR_DEFAULTSIZE` 读取。
/// 资源缺失或读取失败返回 `None`（调用方直接报错给 `create_tray`）。
fn load_tray_icon() -> Option<Icon> {
    match Icon::from_resource(APP_ICON_RESOURCE_ID, None) {
        Ok(icon) => Some(icon),
        Err(e) => {
            eprintln!("加载托盘图标资源失败: {e}");
            None
        }
    }
}

/// 在系统托盘创建 WinTag 图标（D26：tray-icon）。
///
/// - 图标取自嵌入资源 `ID=1`；
/// - 悬浮提示 "WinTag"；
/// - 右键单击自动弹出上下文菜单（`with_menu_on_right_click(true)`）；
/// - 左键单击不弹菜单（默认 `with_menu_on_left_click(true)` 需关闭，
///   改由 [`icon_event_to_command`] 解码为 `OpenPanel` 打开面板）。
///
/// 返回的 [`TrayIcon`] 参考计数，最后实例 drop 时自动从系统托盘移除。
/// **线程约束**：tray-icon 要求托盘与 Win32 事件循环同线程创建——主线程
/// `GetMessageW` 消息泵恰好满足，故无需额外线程（见 `main.rs`）。
pub fn create_tray() -> anyhow::Result<TrayIcon> {
    let icon = load_tray_icon().ok_or_else(|| anyhow::anyhow!("无可用托盘图标资源"))?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(build_menu()))
        .with_menu_on_left_click(false)
        .with_tooltip("WinTag")
        .with_icon(icon)
        .build()
        .map_err(|e| anyhow::anyhow!("创建托盘图标失败: {e}"))?;
    Ok(tray)
}

/// 弹出托盘气泡提示（D26：notify-rust TOAST 通知）。
///
/// 实际弹出前先读取 [`balloon_enabled`] 开关，关闭时静默返回。
/// 是否调用本函数由主线程结合 `no_tray` 与纯逻辑层 [`should_show_balloon`] 决定。
pub fn show_balloon(title: &str, message: &str) {
    if !balloon_enabled() {
        return;
    }
    // notify-rust 走 Windows WinRT TOAST（经 tauri-winrt-notification）。
    // 失败静默（气泡缺失非致命，不影响功能）。
    if let Err(e) = notify_rust::Notification::new()
        .summary(title)
        .body(message)
        .show()
    {
        eprintln!("发送气泡通知失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_click_up_maps_to_open_panel() {
        let event = TrayIconEvent::Click {
            id: tray_icon::TrayIconId::from("tray"),
            position: tray_icon::menu::dpi::PhysicalPosition::new(0.0, 0.0),
            rect: tray_icon::Rect {
                position: tray_icon::menu::dpi::PhysicalPosition::new(0.0, 0.0),
                size: tray_icon::menu::dpi::PhysicalSize::new(16, 16),
            },
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
        };
        assert_eq!(icon_event_to_command(&event), Some(TrayCommand::OpenPanel));
    }

    #[test]
    fn right_click_and_non_clicks_map_to_none() {
        let right = TrayIconEvent::Click {
            id: tray_icon::TrayIconId::from("tray"),
            position: tray_icon::menu::dpi::PhysicalPosition::new(0.0, 0.0),
            rect: tray_icon::Rect {
                position: tray_icon::menu::dpi::PhysicalPosition::new(0.0, 0.0),
                size: tray_icon::menu::dpi::PhysicalSize::new(16, 16),
            },
            button: tray_icon::MouseButton::Right,
            button_state: tray_icon::MouseButtonState::Up,
        };
        let down = TrayIconEvent::Click {
            id: tray_icon::TrayIconId::from("tray"),
            position: tray_icon::menu::dpi::PhysicalPosition::new(0.0, 0.0),
            rect: tray_icon::Rect {
                position: tray_icon::menu::dpi::PhysicalPosition::new(0.0, 0.0),
                size: tray_icon::menu::dpi::PhysicalSize::new(16, 16),
            },
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Down,
        };
        assert_eq!(icon_event_to_command(&right), None);
        assert_eq!(icon_event_to_command(&down), None);
    }

    #[test]
    fn menu_ids_map_to_commands() {
        assert_eq!(
            menu_id_to_command(&MenuId::new(MENU_ID_OPEN_PANEL)),
            Some(TrayCommand::OpenPanel)
        );
        assert_eq!(
            menu_id_to_command(&MenuId::new(MENU_ID_OPEN_SETTINGS)),
            Some(TrayCommand::OpenSettings)
        );
        assert_eq!(
            menu_id_to_command(&MenuId::new(MENU_ID_QUICK_TAG)),
            Some(TrayCommand::QuickTag)
        );
        assert_eq!(
            menu_id_to_command(&MenuId::new(MENU_ID_EXIT)),
            Some(TrayCommand::Exit)
        );
        assert_eq!(menu_id_to_command(&MenuId::new("unknown")), None);
    }

    #[test]
    fn balloon_shown_only_when_enabled_and_tray_present() {
        assert!(should_show_balloon(false, true));
        assert!(!should_show_balloon(true, true));
        assert!(!should_show_balloon(false, false));
    }
}
