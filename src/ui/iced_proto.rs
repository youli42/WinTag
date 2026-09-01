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

/// 主线程 → iced 线程：请求 iced 执行某个界面动作。
///
/// 由 `main.rs` 经 crossbeam 通道发送到 iced 线程，`ui::iced_app` 在
/// `subscription` 中消费后驱动的 `update`。
///
/// 阶段 G0 仅承载退出确认流程；后续阶段（G2-G4）依次扩展设置页 / 标签弹窗 /
/// 概览面板的指令变体。
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
}
