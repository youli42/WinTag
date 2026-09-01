//! 原生层偏好注入收敛（D27 阶段 G1）
//!
//! 「总线最小化」铁律之一：原生层（`sys/overlay`、`sys/tray`）需要的 UI 偏好
//! 此前由主线程经 4 个 `set_*` 逐项注入（`set_show_title`/`set_badge_always_top`/
//! `set_tooltip_theme`/`set_balloon_enabled`），改一条偏好要同步改 N 处。此处
//! 收敛为**一个 [`NativePrefs`] 纯值结构**，由主线程在启动与 `reapply_theme`
//! 时各写入一次（`set_native_prefs`）。
//!
//! 依赖方向：本模块是 sys 层叶子，只依赖标准库与 windows-rs，**不感知
//! `core::settings` 与 `ui`**——偏好值与 [`crate::core::settings::Settings`]
//! 的映射由调用方（`main.rs` 的 `apply_native_prefs` 纯函数）负责，保持
//! `ui → core → sys` 依赖方向不被破坏。
//!
//! 注入的另有两条不属于「偏好」的持久注入点，本模块不收敛、各保持原样：
//! - `sys::overlay::set_tag_store`（数据本体 `Arc<Mutex<TagStore>>`）；
//! - `sys::overlay::set_message_target`（`WM_APP_EDIT_TAG` 消息中转的隐藏窗口，
//!   即 NativeBridge 的回传出口）。

use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::COLORREF;

/// 原生层偏好（纯值结构，`Copy` 便于跨函数高频读取）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePrefs {
    /// 角标标题条是否显示（R6：设置页"角标显示标题"）
    pub show_title: bool,
    /// 角标是否始终置顶（R19：设置页"角标始终置顶"）
    pub badge_always_top: bool,
    /// tooltip 背景色（`COLORREF`，`0x00BBGGRR` 布局）
    pub tooltip_bg: COLORREF,
    /// tooltip 前景（文字）色
    pub tooltip_fg: COLORREF,
    /// 托盘启动气泡是否显示（R18/D24：设置页"气泡提示"）
    pub balloon_enabled: bool,
}

impl Default for NativePrefs {
    /// 未注入时的回退值：显示标题 + 始终置顶 + 气泡打开；tooltip 白底黑字。
    fn default() -> Self {
        NativePrefs {
            show_title: true,
            badge_always_top: true,
            tooltip_bg: COLORREF(0x00FFFFFF),
            tooltip_fg: COLORREF(0x00000000),
            balloon_enabled: true,
        }
    }
}

/// 原生层偏好全局状态（`Mutex` 承载以支持设置保存后热更新，镜像原
/// `TOOLTIP_THEME` 的 Mutex 语义）
static NATIVE_PREFS: OnceLock<Mutex<NativePrefs>> = OnceLock::new();

/// 写入原生层偏好（主线程启动 + 设置保存广播后各调用一次）
///
/// 首次调用初始化存储；此后每次调用覆盖为最新值，使覆盖层/tooltip/托盘
/// 即时采用新偏好。
pub fn set_native_prefs(prefs: NativePrefs) {
    let state = NATIVE_PREFS.get_or_init(|| Mutex::new(prefs));
    if let Ok(mut guard) = state.lock() {
        *guard = prefs;
    }
}

/// 读取当前原生层偏好（未注入或锁中毒时回退 [`Default`]，必不 panic）
pub fn native_prefs() -> NativePrefs {
    NATIVE_PREFS
        .get()
        .and_then(|s| s.lock().ok())
        .map(|guard| *guard)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认值：显示标题 + 始终置顶 + 气泡打开 + tooltip 白底黑字
    #[test]
    fn default_fields() {
        let prefs = NativePrefs::default();
        assert!(prefs.show_title);
        assert!(prefs.badge_always_top);
        assert!(prefs.balloon_enabled);
        assert_eq!(prefs.tooltip_bg, COLORREF(0x00FFFFFF));
        assert_eq!(prefs.tooltip_fg, COLORREF(0x00000000));
    }

    /// 写入后读取返回相同值；覆盖写入采用新值
    #[test]
    fn set_then_read_roundtrip() {
        let prefs = NativePrefs {
            show_title: false,
            badge_always_top: false,
            tooltip_bg: COLORREF(0x00202020),
            tooltip_fg: COLORREF(0x00E6E6E6),
            balloon_enabled: false,
        };
        set_native_prefs(prefs);
        assert_eq!(native_prefs(), prefs);
    }
}
