//! 主线程与 iced 线程之间的纯协议层（D27，叶子模块）
//!
//! 本模块只依赖标准库，不依赖 iced / Win32，供 `main.rs`（主线程）与
//! `ui::iced_app`（iced 线程）共用同一对消息契约。跨线程仅传 `Clone + Send`
//! 类型（`String` / 数值 / `TagColor` 等），窗口句柄一律以 `isize` 承载。
//!
//! 设计约束（见 doc/iced-migration.md 「总线最小化铁律」）：
//! - 主线程 → iced 线程经 [`IcedCommand`]；iced 线程 → 主线程经 [`GuiEvent`]；
//! - 全项目仅此一对跨线程通道，后续新增窗口/消息都复用，**禁止新增 channel**；
//! - 同一消息只有一个出口：原生 `WM_APP_*` 只负责「进主循环」，出主循环走本通道。
//!
//! 本模块依赖方向：`ui → core`（可引用 `core::settings::Settings`、`core::tag`），
//! 但不依赖 iced / Win32。

use crate::core::settings::Settings;
use crate::core::tag::Tag;

/// 主线程 → iced 线程：请求 iced 执行某个界面动作。
///
/// 由 `main.rs` 经 crossbeam 通道发送到 iced 线程，`ui::iced_app` 在
/// `subscription` 中消费后驱动的 `update`。
///
/// 阶段 G0 落地退出确认流，G2 扩展设置页指令，G3 扩展标签编辑弹窗指令。
#[derive(Debug, Clone)]
pub enum IcedCommand {
    /// 打开"退出确认"窗口（`count` = 待丢弃的标签/便签数量）。
    ///
    /// 主线程在 `request_exit`（有标签且未确认）时发送；iced 线程收到后
    /// 以居中弹窗展示并聚焦"确认"按钮。
    ShowConfirm { count: usize },
    /// 关闭"退出确认"窗口。
    ///
    /// 供主线程请求强制收起确认窗（如再次请求退出被取消时的收尾）。
    CloseConfirm,
    /// 打开设置窗口（G2）。
    ///
    /// iced 线程收到后读取全局设置的当前快照（`core::settings::global_settings`）
    /// 预填表单，并创建设置窗口。
    OpenSettings,
    /// 应用主题（G2：`dark` = 是否暗色）。
    ///
    /// 主线程在 `reapply_theme`（设置保存广播/系统主题切换）后发送，
    /// iced 线程据此更新各 iced 窗口的主题。
    ApplyTheme { dark: bool },
    /// 打开标签编辑弹窗（G3）。
    ///
    /// `target` = 目标窗口句柄（isize）；`position` = 主线程算好的弹窗左上角
    /// 物理像素坐标（光标右下偏移 + 钳制到工作区）；`tag` = 预填的标签数据
    /// （新建时 title 预填窗口标题、note 空、color 默认橙；编辑时保留原值）。
    EditTag {
        target: isize,
        position: (i32, i32),
        tag: Tag,
    },
}

/// iced 线程 → 主线程：iced 产出的界面事件，由主线程 `pump_background_events`
/// 轮询消费并分发到现有窗口动作。
///
/// 由 `ui::iced_app` 在 `update` 中经 crossbeam 通道发送回主线程。
#[derive(Debug, Clone)]
pub enum GuiEvent {
    /// 用户在"退出确认"窗点击"退出"（或回车）。
    ///
    /// 主线程收到后走既有 `WM_APP_EXIT(wParam=1)` 退出流（复用
    /// `should_confirm_exit`，见 `main.rs`）。
    ConfirmExit,
    /// 用户取消退出（点击"取消" / Esc / 关闭窗口）。
    CancelExit,
    /// 用户在设置页点击"保存"（`Settings` = 保存后的完整设置，G2）。
    ///
    /// 主线程写入全局设置 + 持久化 + 触发 `reapply_theme`。
    SettingsChanged(Settings),
    /// 用户在标签弹窗点击"保存"（G3）。
    ///
    /// `target` = 目标窗口句柄；`tag` = 编辑后的标签（`window_title`/`process_name`
    /// 已在 [`IcedCommand::EditTag`] 时由主线程带入，此处保留）。
    TagSaved { target: isize, tag: Tag },
}

// ---------------------------------------------------------------------
// 标签弹窗单例决策（G3，自 `ui/popup.rs` 移入；纯函数可单测）
// ---------------------------------------------------------------------

/// 根据活动弹窗登记、目标窗口句柄与旧弹窗存活状态，决策新建 / 复用 / 替换
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupPlan {
    /// 首次弹窗（无活动弹窗或旧弹窗已销毁）
    Fresh,
    /// 同目标且旧弹窗存活：复用并置前聚焦（载荷为旧弹窗句柄）
    Reuse(isize),
    /// 异目标且旧弹窗存活：销毁旧弹窗后新建（载荷为旧弹窗句柄）
    Replace(isize),
}

/// 弹出标签弹窗前的决策函数（纯函数，无副作用）
///
/// - `active` 为 `None` → [`PopupPlan::Fresh`]；
/// - 旧弹窗已销毁（`old_alive == false`）→ [`PopupPlan::Fresh`]；
/// - 同目标且存活 → [`PopupPlan::Reuse`]；
/// - 异目标且存活 → [`PopupPlan::Replace`]。
///
/// `old_alive` 由调用方计算后传入，本函数不访问任何窗口 API，便于单元测试。
pub fn plan_popup_action(
    active: Option<(isize, isize)>,
    target_hwnd: isize,
    old_alive: bool,
) -> PopupPlan {
    match active {
        None => PopupPlan::Fresh,
        Some((old_target, old_hwnd)) => {
            if !old_alive {
                PopupPlan::Fresh
            } else if old_target == target_hwnd {
                PopupPlan::Reuse(old_hwnd)
            } else {
                PopupPlan::Replace(old_hwnd)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- plan_popup_action ----

    /// 无活动弹窗 → Fresh；旧弹窗已销毁（old_alive=false）→ Fresh
    #[test]
    fn plan_fresh_when_none_or_dead() {
        assert_eq!(plan_popup_action(None, 100, true), PopupPlan::Fresh);
        assert_eq!(plan_popup_action(None, 100, false), PopupPlan::Fresh);
        // 旧弹窗已销毁 → 清空过期登记，直接新建
        assert_eq!(
            plan_popup_action(Some((98, 42)), 100, false),
            PopupPlan::Fresh
        );
    }

    /// 同目标且旧弹窗存活 → Reuse（复用置前）
    #[test]
    fn plan_reuse_same_target_alive() {
        assert_eq!(
            plan_popup_action(Some((100, 42)), 100, true),
            PopupPlan::Reuse(42)
        );
    }

    /// 异目标且旧弹窗存活 → Replace（销毁旧窗后新建）
    #[test]
    fn plan_replace_diff_target_alive() {
        assert_eq!(
            plan_popup_action(Some((150, 42)), 100, true),
            PopupPlan::Replace(42)
        );
    }

    /// 跨线程协议变体可 Debug + Clone（iced 消息要求）
    #[test]
    fn protocol_variants_debug_clone() {
        let cmd = IcedCommand::EditTag {
            target: 1,
            position: (2, 3),
            tag: default_tag(),
        };
        let _ = format!("{cmd:?}");
        let _ = cmd.clone();
        let ev = GuiEvent::TagSaved {
            target: 1,
            tag: default_tag(),
        };
        let _ = format!("{ev:?}");
        let _ = ev.clone();
    }

    fn default_tag() -> Tag {
        Tag {
            title: String::new(),
            note: String::new(),
            color: crate::core::tag::TagColor::Orange,
            window_title: String::new(),
            process_name: String::new(),
        }
    }
}
