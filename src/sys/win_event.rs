use anyhow::{Context, Result};
use std::ffi::c_void;
use std::sync::OnceLock;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PostMessageW, MSG, PM_NOREMOVE};

use crate::common::WM_APP_WINEVENT;

/// 系统级 WinEvent 动作分类。
///
/// 该枚举只描述“窗口语义层”可执行的界面动作，不直接执行任何 Win32 操作：
/// - [`WinEventAction::Sync`]：同步覆盖层位置/尺寸；
/// - [`WinEventAction::Hide`]：隐藏覆盖层；
/// - [`WinEventAction::Show`]：显示覆盖层；
/// - [`WinEventAction::BringToTop`]：将覆盖层重新置顶；
/// - [`WinEventAction::Forget`]：忘记/移除对应窗口状态；
/// - [`WinEventAction::Ignore`]：忽略该事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinEventAction {
    /// 触发覆盖层与目标窗口重新同步。
    Sync,
    /// 触发覆盖层隐藏。
    Hide,
    /// 触发覆盖层显示。
    Show,
    /// 触发覆盖层回到最顶层。
    BringToTop,
    /// 触发移除窗口记录。
    Forget,
    /// 窗口移动/缩放**开始**：主线程可据此**加速**兜底轮询（500ms→100ms），
    /// 弥补拖拽期间 `EVENT_OBJECT_LOCATIONCHANGE` 合并导致的位置滞后/丢失，
    /// 以及提权目标窗口 UIPI 拦截事件后仅靠事件同步的缺失（问题 18）。
    MoveStart,
    /// 窗口移动/缩放**结束**：主线程据此执行**强制最终同步**（`sync_position_force`，
    /// 优先取 `GetWindowRect` 规避 DWM extended frame bounds 拖拽中的陈旧值），
    /// 并恢复兜底轮询周期（100ms→500ms）。
    MoveEnd,
    /// 不产生任何动作。
    Ignore,
}

// 说明：Windows 事件常量在 windows-rs 0.58 中可能并非全部对外暴露，
// 因此这里采用 Win32 规范中的稳定字面量并配以中文注释，避免构建期常量缺失。
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003; // 系统前台窗口切换
const EVENT_SYSTEM_MOVESIZESTART: u32 = 0x000A; // 系统移动/缩放开始（拖动窗口标题栏）
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B; // 系统移动/缩放结束
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016; // 系统最小化开始
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017; // 系统最小化结束

const EVENT_OBJECT_DESTROY: u32 = 0x8001; // 对象销毁
const EVENT_OBJECT_SHOW: u32 = 0x8002; // 对象显示
const EVENT_OBJECT_HIDE: u32 = 0x8003; // 对象隐藏
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B; // 对象位置/尺寸变化
const EVENT_OBJECT_CLOAKED: u32 = 0x8017; // 对象被 Cloak（被系统遮蔽）
const EVENT_OBJECT_UNCLOAKED: u32 = 0x8018; // 对象解除 Cloak

const OBJID_WINDOW: isize = 0; // 仅处理窗口对象
const CHILDID_SELF: isize = 0; // 仅处理对象自身

static HIDDEN_HWND: OnceLock<usize> = OnceLock::new();
// WinEvent hook 标志位：OutOfContext + SkipOwnProcess。
// 0x0000 表示 out-of-context，0x0002 表示跳过本进程事件。
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;

/// 按 WinEvent 编号归类应执行的界面动作。
///
/// 该函数不访问任何全局状态，便于单测与复用。
pub fn classify(event: u32) -> WinEventAction {
    match event {
        EVENT_OBJECT_LOCATIONCHANGE => WinEventAction::Sync,
        EVENT_OBJECT_DESTROY => WinEventAction::Forget,
        EVENT_SYSTEM_MINIMIZESTART | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED => {
            WinEventAction::Hide
        }
        EVENT_SYSTEM_MINIMIZEEND | EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED => {
            WinEventAction::Show
        }
        EVENT_SYSTEM_FOREGROUND => WinEventAction::BringToTop,
        EVENT_SYSTEM_MOVESIZESTART => WinEventAction::MoveStart,
        EVENT_SYSTEM_MOVESIZEEND => WinEventAction::MoveEnd,
        _ => WinEventAction::Ignore,
    }
}

/// 判断回调收到的 WinEvent 参数是否应继续向主线程转发。
///
/// 只接受窗口对象本身，避免将大量无关辅助对象事件塞入消息队列。
pub fn should_forward(idobject: isize, idchild: isize, hwnd: HWND) -> bool {
    idobject == OBJID_WINDOW && idchild == CHILDID_SELF && !hwnd.0.is_null()
}

/// 持有 `SetWinEventHook` 返回的两个事件钩子句柄。
///
/// `hooks` 分别对应：
/// - 系统事件段：`0x0003..=0x0017`
/// - 对象事件段：`0x8001..=0x8018`
///
/// `degraded = true` 表示两个 hook 都安装失败，模块已退化为纯轮询模式；
/// 该标志不会因“部分成功”而置位。
#[derive(Debug)]
pub struct WinEventHooks {
    hooks: [Option<HWINEVENTHOOK>; 2],
    degraded: bool,
}

impl WinEventHooks {
    /// 返回当前事件监听是否已退化为纯轮询模式。
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }
}

/// 绑定隐藏窗口句柄，作为 WinEvent 回调转发的目标。
///
/// 必须在 [`install`] 之前调用；若多次调用，后续绑定会被静默忽略。
pub fn bind_hidden(hwnd: HWND) {
    let _ = HIDDEN_HWND.set(hwnd.0 as usize);
}

/// 安装系统级 WinEvent 监听，并将事件转发到已绑定的隐藏窗口。
///
/// 监听范围被拆分为两个 hook：系统段与对象段。若两个 hook 全部安装失败，
/// 返回一个 `degraded = true` 的实例，表示后续只能依赖纯轮询逻辑。
pub fn install() -> Result<WinEventHooks> {
    let _hidden = HIDDEN_HWND
        .get()
        .copied()
        .context("请先调用 bind_hidden(hwnd) 再安装 WinEvent hook")?;

    let hooks = [install_range(0x0003, 0x0017), install_range(0x8001, 0x8018)];

    let degraded = hooks.iter().all(|hook| hook.is_none());
    Ok(WinEventHooks { hooks, degraded })
}

impl Drop for WinEventHooks {
    fn drop(&mut self) {
        // SAFETY: 这些句柄都由本实例通过 SetWinEventHook 创建，且只在主线程
        // 消息泵架构中管理；UnhookWinEvent 只是注销回调，不触发额外 UI 动作。
        unsafe {
            for hook in &mut self.hooks {
                if let Some(handle) = hook.take() {
                    let _ = UnhookWinEvent(handle);
                }
            }
        }
    }
}

fn install_range(event_min: u32, event_max: u32) -> Option<HWINEVENTHOOK> {
    // SAFETY: SetWinEventHook 仅注册回调，不执行被监控进程内注入；
    // 这里固定使用 WINEVENT_OUTOFCONTEXT + WINEVENT_SKIPOWNPROCESS，
    // 符合本项目“主线程消息泵 + 仅转发消息”的架构约束。
    let hook = unsafe {
        SetWinEventHook(
            event_min,
            event_max,
            None,
            Some(win_event_callback),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };

    if hook.0.is_null() {
        None
    } else {
        Some(hook)
    }
}

unsafe extern "system" fn win_event_callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    idobject: i32,
    idchild: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if !should_forward(idobject as isize, idchild as isize, hwnd) {
        return;
    }

    let Some(target) = HIDDEN_HWND.get().copied() else {
        return;
    };
    let target = HWND(target as *mut c_void);

    // 事件风暴合并：LOCATIONCHANGE 等事件在窗口拖动/滚动时以帧频爆发。
    // 若消息队列中已存在未处理的 WM_APP_WINEVENT，说明主线程尚未消化上一批事件，
    // 跳过本次投递（主循环处理时以队列中最新的消息为准，中间状态可合并丢弃）。
    // 这避免消息队列无限堆积，将同步频率收敛到主循环的处理速度。
    // 例外：MOVESIZEEND 是"移动结束的最终同步"信号，若被合并丢弃会导致角标停在
    // 拖拽中途的陈旧位置（问题 18）；故该事件**不参与合并丢弃**，必保投递。
    // SAFETY: 回调运行在安装线程（主线程）消息泵内，PeekMessageW 仅查询本线程
    // 队列、不取出消息，无副作用；PM_NOREMOVE 保证消息保留在队列中。
    let mut probe = MSG::default();
    let has_pending = unsafe {
        PeekMessageW(
            &mut probe,
            None,
            WM_APP_WINEVENT,
            WM_APP_WINEVENT,
            PM_NOREMOVE,
        )
        .as_bool()
    };
    if has_pending && event != EVENT_SYSTEM_MOVESIZEEND {
        return;
    }

    // SAFETY: 回调在安装线程的消息泵内执行，只做轻量消息转发；
    // 目标窗口句柄由 bind_hidden 预先绑定，若窗口已销毁则 PostMessageW 失败也会被忽略。
    // 此处不做任何重活，不访问布局/重绘 API，避免阻塞 USER 队列。
    unsafe {
        let _ = PostMessageW(
            target,
            WM_APP_WINEVENT,
            WPARAM(hwnd.0 as usize),
            LPARAM(event as isize),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;

    fn non_null_hwnd() -> HWND {
        HWND(0x1234usize as *mut c_void)
    }

    #[test]
    fn classify_location_change_and_foreground_and_unknown() {
        assert_eq!(classify(EVENT_OBJECT_LOCATIONCHANGE), WinEventAction::Sync);
        assert_eq!(
            classify(EVENT_SYSTEM_FOREGROUND),
            WinEventAction::BringToTop
        );
        assert_eq!(classify(0x9999), WinEventAction::Ignore);
    }

    #[test]
    fn classify_destroy_and_minimize_start() {
        assert_eq!(classify(EVENT_OBJECT_DESTROY), WinEventAction::Forget);
        assert_eq!(classify(EVENT_SYSTEM_MINIMIZESTART), WinEventAction::Hide);
    }

    #[test]
    fn classify_hide_variants() {
        assert_eq!(classify(EVENT_OBJECT_HIDE), WinEventAction::Hide);
        assert_eq!(classify(EVENT_OBJECT_CLOAKED), WinEventAction::Hide);
    }

    #[test]
    fn classify_show_variants() {
        assert_eq!(classify(EVENT_SYSTEM_MINIMIZEEND), WinEventAction::Show);
        assert_eq!(classify(EVENT_OBJECT_SHOW), WinEventAction::Show);
        assert_eq!(classify(EVENT_OBJECT_UNCLOAKED), WinEventAction::Show);
    }

    #[test]
    fn classify_move_start_end() {
        assert_eq!(
            classify(EVENT_SYSTEM_MOVESIZESTART),
            WinEventAction::MoveStart
        );
        assert_eq!(classify(EVENT_SYSTEM_MOVESIZEEND), WinEventAction::MoveEnd);
    }

    #[test]
    fn should_forward_true_when_all_filters_match() {
        assert!(should_forward(OBJID_WINDOW, CHILDID_SELF, non_null_hwnd()));
    }

    #[test]
    fn should_forward_false_when_any_filter_breaks() {
        assert!(!should_forward(1, CHILDID_SELF, non_null_hwnd()));
        assert!(!should_forward(OBJID_WINDOW, 1, non_null_hwnd()));
        assert!(!should_forward(
            OBJID_WINDOW,
            CHILDID_SELF,
            HWND(std::ptr::null_mut())
        ));
    }
}
