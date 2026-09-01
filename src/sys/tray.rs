//! 托盘事件解码纯逻辑层。
//!
//! 本模块不调用任何 Win32 API（无 `Shell_NotifyIcon` / `NOTIFYICONDATAW` 等），
//! 只负责把托盘 `uCallbackMessage` 回调中承载在 `lParam` 上的事件码解码为
//! 高层命令，以及气泡提示的显示判定。真正的托盘图标创建/销毁、右键菜单等
//! 系统交互由后续 Win32 实现层负责，其回调可直接复用本模块的解码函数。

// 事件码一律使用 Win32 规范中的稳定字面量（u32），避免引入 windows-rs 常量
// 依赖，保证本模块“零 Win32 调用”的纯逻辑定位。
pub const WM_LBUTTONUP: u32 = 0x0202; // 鼠标左键在托盘图标上释放（单击）
pub const WM_RBUTTONUP: u32 = 0x0205; // 鼠标右键在托盘图标上释放（弹出菜单，由 Win32 实现层处理）
pub const WM_USER: u32 = 0x0400; // 托盘回调自定义消息基址
pub const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5; // 用户点击气泡提示本体

/// 托盘事件解码后的高层界面命令。
///
/// 只描述“托盘交互应触发什么界面动作”，不执行任何 Win32 操作，
/// 由后续 Win32 实现层（Shell_NotifyIcon 回调）持有本枚举并分发到主线程：
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

/// 把托盘 `uCallbackMessage` 回调中承载在 `lParam` 上的事件码解码为高层命令。
///
/// 映射规则（与后续 Win32 实现层的职责划分）：
/// - [`WM_LBUTTONUP`]（0x0202，左键单击托盘图标）与 [`NIN_BALLOONUSERCLICK`]
///   （WM_USER+5，点击气泡提示本体）→ `Some(TrayCommand::OpenPanel)`；
/// - 其余事件码（含 [`WM_RBUTTONUP`] 0x0205 右键弹出菜单、NIN_SELECT 选中等）
///   → `None`，由 Win32 实现层的右键菜单/其他事件分支另行处理，不属于本解码函数职责。
///
/// 该函数为纯函数：不访问任何全局状态，便于单测与复用。
pub fn tray_command_from_lparam(lparam: u32) -> Option<TrayCommand> {
    match lparam {
        WM_LBUTTONUP | NIN_BALLOONUSERCLICK => Some(TrayCommand::OpenPanel),
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
// 以下为 Win32 实现层（D22 托盘落地）：托盘图标创建/销毁、气泡提示、
// 右键菜单、TaskbarCreated 重注册与气泡开关注入。
// 模块顶部说明中的"不调用任何 Win32 API"仅描述上方纯逻辑段；
// 本段与纯逻辑层之间通过 `TrayCommand` / `tray_command_from_lparam`
// 衔接，不改动纯逻辑层任何一行。
// =====================================================================

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetClassLongPtrW, GetCursorPos, LoadIconW,
    RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, GCLP_HICONSM, HICON, IDI_WINLOGO,
    MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

/// 托盘右键菜单项 id：打开概览面板（连续常量，`TrackPopupMenu` 的
/// `TPM_RETURNCMD` 返回值按此解码）
const MENU_ID_OPEN_PANEL: usize = 100;
/// 托盘右键菜单项 id：打开设置页
const MENU_ID_OPEN_SETTINGS: usize = 101;
/// 托盘右键菜单项 id：快速标记
const MENU_ID_QUICK_TAG: usize = 102;
/// 托盘右键菜单项 id：退出
const MENU_ID_EXIT: usize = 103;

/// 托盘气泡显示开关（设置项 `show_balloon` 的 sys 层注入镜像）
///
/// 依赖方向约束（ui → core → sys）不允许 sys 层读取 `core::settings`，
/// 因此由主线程经 [`set_balloon_enabled`] 注入（启动时 + 设置保存广播后），
/// 镜像 `sys::overlay::set_show_title` 的注入模式。未注入时默认显示。
static BALLOON_ENABLED: AtomicBool = AtomicBool::new(true);

/// 注入托盘气泡显示开关（[`show_balloon`] 实际弹出前读取本开关）
pub fn set_balloon_enabled(enabled: bool) {
    BALLOON_ENABLED.store(enabled, Ordering::Relaxed);
}

/// 读取托盘气泡显示开关（未注入时默认 `true`）
pub fn balloon_enabled() -> bool {
    BALLOON_ENABLED.load(Ordering::Relaxed)
}

/// 把 UTF-8 字符串按 UTF-16 码元写入定长宽字符数组，末尾保留一个 NUL。
///
/// 最多写入 `max_units` 个码元（超出部分截断），须满足 `max_units + 1 <= buf.len()`；
/// 调用方保证 `buf` 已全零初始化（`NOTIFYICONDATAW::default()` 满足），
/// 因此 NUL 之后未被写入的元素保持为 0，满足 Windows 对 `szTip`/`szInfo`
/// 等字段"以 NUL 结尾"的要求。
fn fill_wide(buf: &mut [u16], s: &str, max_units: usize) {
    let mut written = 0;
    for (dst, ch) in buf.iter_mut().take(max_units).zip(s.encode_utf16()) {
        *dst = ch;
        written += 1;
    }
    buf[written] = 0;
}

/// 加载托盘图标：优先取 `icon_hwnd` 窗口类的小图标，失败回退系统共享图标 IDI_WINLOGO。
///
/// 两类来源均为共享图标（窗口类图标 / 系统图标），不产生本程序私有资源，
/// 因此无需（也不应）`DestroyIcon`。
fn load_tray_icon(icon_hwnd: Option<HWND>) -> anyhow::Result<HICON> {
    if let Some(hwnd) = icon_hwnd {
        // SAFETY: hwnd 由调用方保证存活；GetClassLongPtrW 只读查询窗口类小图标
        // 句柄，无副作用；返回 0 表示该窗口类无图标，进入回退分支。
        let raw = unsafe { GetClassLongPtrW(hwnd, GCLP_HICONSM) } as *mut core::ffi::c_void;
        if !raw.is_null() {
            return Ok(HICON(raw));
        }
    }
    // SAFETY: LoadIconW 加载系统共享图标资源（IDI_WINLOGO），不产生新资源
    // 所有权；hInstance 传 None 表示系统图标。
    unsafe { LoadIconW(None, IDI_WINLOGO) }
        .map_err(|e| anyhow::anyhow!("加载系统托盘图标失败: {e}"))
}

/// 在系统托盘创建 WinTag 图标（D22）。
///
/// 图标特性 `NIF_ICON | NIF_MESSAGE | NIF_TIP`：回调消息为
/// [`crate::common::WM_APP_TRAY`]（投递给 `hidden_hwnd` 的 WndProc），
/// 悬浮提示 "WinTag"（按 Windows 要求截断到 127 个 UTF-16 码元）。
/// `icon_hwnd` 可选指定图标来源窗口，`None` 时使用系统共享图标 IDI_WINLOGO。
pub fn add_tray(hidden_hwnd: HWND, icon_hwnd: Option<HWND>) -> anyhow::Result<()> {
    let hicon = load_tray_icon(icon_hwnd)?;
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hidden_hwnd,
        uID: 0,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: crate::common::WM_APP_TRAY,
        hIcon: hicon,
        ..Default::default()
    };
    fill_wide(&mut nid.szTip, "WinTag", 127);
    // SAFETY: nid 为栈上局部变量，调用期间指针有效；cbSize 已按结构体实际大小
    // 填充，hIcon 为共享图标（系统/窗口类），无需销毁。
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
    if !ok.as_bool() {
        anyhow::bail!("Shell_NotifyIconW(NIM_ADD) 失败");
    }
    Ok(())
}

/// 从系统托盘移除 WinTag 图标（D22）。
///
/// 只执行 `NIM_DELETE`：按 `hWnd + uID` 定位并移除图标。
/// 共享系统图标 IDI_WINLOGO（或窗口类图标）不归本程序所有，
/// 不调用 `DestroyIcon`。
pub fn remove_tray(hidden_hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hidden_hwnd,
        uID: 0,
        ..Default::default()
    };
    // SAFETY: nid 为栈上局部变量，调用期间指针有效；NIM_DELETE 不销毁 hIcon
    // （共享图标无需释放）。失败（如 shell 重启后图标已不存在）可安全忽略。
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

/// 弹出托盘气泡提示（D22）。
///
/// 填充 `NIF_INFO` 的 `szInfoTitle`/`szInfo`（UTF-16，各截断到 63/255 码元）
/// 后经 `NIM_MODIFY` 更新图标。实际弹出前先读取 [`balloon_enabled`]，
/// 开关关闭时静默返回；是否调用本函数由主线程结合 `no_tray` 与纯逻辑层
/// [`should_show_balloon`] 决定。
pub fn show_balloon(hidden_hwnd: HWND, title: &str, message: &str) {
    if !balloon_enabled() {
        return;
    }
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hidden_hwnd,
        uID: 0,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    fill_wide(&mut nid.szInfoTitle, title, 63);
    fill_wide(&mut nid.szInfo, message, 255);
    // SAFETY: nid 为栈上局部变量，调用期间指针有效；szInfo/szInfoTitle 已按
    // Windows 要求以 NUL 结尾且长度合法。失败（图标尚未创建）可安全忽略。
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

/// 在托盘图标处弹出右键菜单并返回用户选择的高层命令（D22）。
///
/// 菜单四项：打开概览面板(100)/打开设置页(101)/快速标记(102)/退出(103)。
/// 先 `SetForegroundWindow(hidden_hwnd)` 再 `TrackPopupMenu`（MSDN 规范做法，
/// 保证点击菜单外区域时菜单正确关闭），弹出位置取当前光标屏幕坐标
/// （`GetCursorPos` 失败时回退 (0,0)）。
///
/// 注意：菜单被取消（返回 0）或解码失败时回退返回 [`TrayCommand::OpenPanel`]
/// ——签名约定必须返回具体命令，打开面板是所有候选中唯一无破坏性且可被用户
/// 立即关闭的动作（绝不回退到 Exit 造成误退出）。
pub fn show_context_menu(hidden_hwnd: HWND) -> TrayCommand {
    // SAFETY: CreatePopupMenu 无副作用；失败返回 null 句柄，进入回退分支。
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return TrayCommand::OpenPanel;
    };

    let open_panel = crate::common::widestring("打开概览面板");
    let open_settings = crate::common::widestring("打开设置页");
    let quick_tag = crate::common::widestring("快速标记");
    let exit = crate::common::widestring("退出");
    // SAFETY: menu 为刚创建的有效菜单；各宽字符串 Vec 在调用期间存活，
    // AppendMenuW 内部复制文本，调用后 Vec 即可释放；SetForegroundWindow 无副作用。
    unsafe {
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_ID_OPEN_PANEL,
            PCWSTR(open_panel.as_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_ID_OPEN_SETTINGS,
            PCWSTR(open_settings.as_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_ID_QUICK_TAG,
            PCWSTR(quick_tag.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_EXIT, PCWSTR(exit.as_ptr()));
        let _ = SetForegroundWindow(hidden_hwnd);
    }

    let mut pt = POINT::default();
    // SAFETY: pt 为栈上局部变量；GetCursorPos 只读写入光标屏幕坐标。
    let _ = unsafe { GetCursorPos(&mut pt) };

    // SAFETY: menu 有效；hidden_hwnd 由调用方保证存活。TPM_RETURNCMD 使返回值
    // 为用户所选菜单项 id（0 表示未选择），TPM_RIGHTBUTTON 兼容托盘右键弹出。
    let chosen = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hidden_hwnd,
            None,
        )
    };
    // SAFETY: menu 有效；DestroyMenu 后不得再使用该句柄。
    unsafe {
        let _ = DestroyMenu(menu);
    }

    match chosen.0 as usize {
        MENU_ID_OPEN_PANEL => TrayCommand::OpenPanel,
        MENU_ID_OPEN_SETTINGS => TrayCommand::OpenSettings,
        MENU_ID_QUICK_TAG => TrayCommand::QuickTag,
        MENU_ID_EXIT => TrayCommand::Exit,
        _ => TrayCommand::OpenPanel,
    }
}

/// 注册 "TaskbarCreated" 窗口消息（D22 shell 重启兜底）。
///
/// explorer.exe 崩溃重启后系统托盘被重建，旧图标全部丢失；主线程在隐藏窗口
/// WndProc 中收到本消息时重新调用 [`add_tray`] 即可恢复图标。
/// 返回注册得到的消息号（同一字符串在会话内恒定），失败返回 Win32 错误。
pub fn register_taskbar_created() -> Result<u32, windows::core::Error> {
    let name = crate::common::widestring("TaskbarCreated");
    // SAFETY: name 为栈上局部 Vec，调用期间存活；RegisterWindowMessageW 为
    // 无副作用的全进程消息注册。
    let msg = unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) };
    if msg == 0 {
        Err(windows::core::Error::from_win32())
    } else {
        Ok(msg)
    }
}

/// 托盘回调消息解码入口（供 hidden_wndproc 调用，D22）。
///
/// sys 层没有消息分发上下文：主线程 WndProc 收到 [`crate::common::WM_APP_TRAY`]
/// 后把窗口消息的 `lParam`（事件码，参数类型 isize/usize）透传进来，
/// 本函数内部委托纯逻辑层 [`tray_command_from_lparam`] 解码为 [`TrayCommand`]。
/// 返回 `None` 表示该事件不属于纯逻辑层职责（如 WM_RBUTTONUP 右键弹菜单），
/// 由调用方另行分支处理。
pub fn tray_message(lparam: usize) -> Option<TrayCommand> {
    tray_command_from_lparam(lparam as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lbutton_up_maps_to_open_panel() {
        assert_eq!(
            tray_command_from_lparam(WM_LBUTTONUP),
            Some(TrayCommand::OpenPanel)
        );
    }

    #[test]
    fn balloon_user_click_maps_to_open_panel() {
        assert_eq!(
            tray_command_from_lparam(NIN_BALLOONUSERCLICK),
            Some(TrayCommand::OpenPanel)
        );
    }

    #[test]
    fn rbutton_up_and_zero_map_to_none() {
        assert_eq!(tray_command_from_lparam(WM_RBUTTONUP), None);
        assert_eq!(tray_command_from_lparam(0), None);
    }

    #[test]
    fn balloon_shown_only_when_enabled_and_tray_present() {
        assert!(should_show_balloon(false, true));
        assert!(!should_show_balloon(true, true));
        assert!(!should_show_balloon(false, false));
    }
}
