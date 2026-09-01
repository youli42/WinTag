//! iced 图形界面应用（D27：四个 GUI 窗口迁至 iced）
//!
//! 本模块运行在**独立线程**（`main.rs` 以 `std::thread` 启动），用 [`iced::daemon`]
//! 的多窗口模型承担 `confirm` / `settings` / `popup` / `panel` 四个窗口。阶段 G0
//! 仅落地退出确认窗（最小闭环），其余窗口由后续阶段（G2-G4）逐步迁入。
//!
//! 线程模型：主线程（Win32 消息泵）与本模块（iced 线程）经一对 crossbeam 通道
//! 双向通信——主线程发 [`IcedCommand`]、本模块回 [`GuiEvent`]，契约见
//! [`crate::ui::iced_proto`]。
//!
//! ## crossbeam（同步）与 iced（异步）的桥接
//!
//! crossbeam 通道是同步阻塞的，无法直接驱动 iced 的异步 `Subscription`。故启动时
//! 派生一个**桥接线程**：其 `recv()` 阻塞等待主线程命令，一旦收到即投递进
//! `futures` 的无界通道；`subscription` 则消费该无界通道，实现「主线程 → iced」。
//! 桥接通道仅在本模块内部存在，不属于对外信道，符合「总线最小化」铁律。

use std::cell::RefCell;

use iced::futures::StreamExt;
use iced::widget::{button, column, container, row, text};
use iced::window;
use iced::{Element, Size, Subscription, Task, Theme};

use crossbeam_channel::{Receiver, Sender};

use crate::ui::iced_proto::{GuiEvent, IcedCommand};

/// 退出确认窗口的目标尺寸（逻辑像素，iced 自管 DPI）
const CONFIRM_W: f32 = 380.0;
const CONFIRM_H: f32 = 160.0;

/// 退出确认窗的状态
struct ConfirmState {
    /// 窗口 id（`window::open` 返回，随窗口存在而有效）
    id: window::Id,
    /// 提示文本（"确定退出？将丢弃 N 个标签/便签"）
    message: String,
}

/// iced 应用状态（运行在 iced 线程，单线程访问，无需 `Send`）
///
/// 仅 [`Message`] 需要 `Send`（iced 事件循环要求），状态本体可含 `RefCell`。
pub struct WinTagApp {
    /// 回传 iced 事件的发送器（iced 线程 → 主线程）
    gui_tx: Sender<GuiEvent>,
    /// crossbeam（同步）→ `futures`（异步）的桥接接收端；首个 `subscription`
    /// 调用时取出并移入订阅流，之后为 `None`（订阅由固定 id 常驻，不再重建）。
    cmd_stream: RefCell<Option<iced::futures::channel::mpsc::UnboundedReceiver<IcedCommand>>>,
    /// 当前显示的退出确认窗（`None` = 未显示）
    confirm: Option<ConfirmState>,
    /// 是否暗色主题（启动时解析；热更新在后续阶段经 `ApplyTheme` 接入）
    dark: bool,
}

/// iced 应用的消息（`Send + Debug + 'static`，iced 事件循环要求）
#[derive(Debug, Clone)]
pub enum Message {
    /// 主线程经跨线程通道送达的指令
    Command(IcedCommand),
    /// 确认窗创建完成（窗口 id 就绪）
    WindowOpened(window::Id),
    /// 确认窗被关闭（用户点关闭按钮 / 系统关闭）
    WindowClosed(window::Id),
    /// 点击"退出"确认按钮（默认动作）
    ConfirmPressed,
    /// 点击"取消"按钮
    CancelPressed,
}

impl WinTagApp {
    /// 创建应用与初始状态（iced 线程启动入口）
    ///
    /// 启动时派生桥接线程：阻塞接收主线程的 [`IcedCommand`] 并转发进
    /// `futures` 无界通道，供 `subscription` 作为异步流消费。初始不显示任何窗口
    /// （`iced::daemon` 在零窗口下保持存活，按需 `ShowConfirm` 打开）。
    pub fn new(
        gui_tx: Sender<GuiEvent>,
        cmd_rx: Receiver<IcedCommand>,
        dark: bool,
    ) -> (Self, Task<Message>) {
        let (bridge_tx, bridge_rx) = iced::futures::channel::mpsc::unbounded();
        // SAFETY: 桥接线程独占 cmd_rx（克隆），阻塞 recv 到主线程退出；
        // bridge_tx 关闭（主线程 drop 时）即收到 Disconnected 退出循环。
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if bridge_tx.unbounded_send(cmd).is_err() {
                    break;
                }
            }
        });
        (
            Self {
                gui_tx,
                cmd_stream: RefCell::new(Some(bridge_rx)),
                confirm: None,
                dark,
            },
            Task::none(),
        )
    }

    /// 每窗口标题（确认窗显示"退出确认"，其余未显示窗口回退"WinTag"）
    pub fn title(&self, window: window::Id) -> String {
        if self.confirm.as_ref().is_some_and(|c| c.id == window) {
            "退出确认".to_string()
        } else {
            "WinTag".to_string()
        }
    }

    /// 状态更新：处理跨线程指令与界面事件，返回驱动窗口/退出流程的 [`Task`]
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Command(cmd) => self.handle_command(cmd),
            Message::WindowOpened(id) => {
                // window::open 已同步返回 id，此处仅确认窗口就绪（无额外动作）
                let _ = self.confirm.as_ref().is_some_and(|c| c.id == id);
                Task::none()
            }
            Message::WindowClosed(id) => {
                if self.confirm.as_ref().is_some_and(|c| c.id == id) {
                    self.confirm = None;
                }
                Task::none()
            }
            Message::ConfirmPressed => {
                let _ = self.gui_tx.send(GuiEvent::ConfirmExit);
                self.close_confirm()
            }
            Message::CancelPressed => {
                let _ = self.gui_tx.send(GuiEvent::CancelExit);
                self.close_confirm()
            }
        }
    }

    /// 处理主线程指令
    fn handle_command(&mut self, cmd: IcedCommand) -> Task<Message> {
        match cmd {
            IcedCommand::ShowConfirm { count } => self.open_confirm(count),
            IcedCommand::CloseConfirm => self.close_confirm(),
        }
    }

    /// 打开退出确认窗（居中 + 定尺寸；返回驱动窗口创建的 Task）
    fn open_confirm(&mut self, count: usize) -> Task<Message> {
        let message = format!("确定退出？将丢弃 {count} 个标签/便签");
        let (id, open) = window::open(window::Settings {
            position: window::Position::Centered,
            size: Size::new(CONFIRM_W, CONFIRM_H),
            resizable: false,
            ..window::Settings::default()
        });
        self.confirm = Some(ConfirmState { id, message });
        open.map(Message::WindowOpened)
    }

    /// 关闭确认窗（并将其从状态移除；`window::close` 返回关闭任务）
    fn close_confirm(&mut self) -> Task<Message> {
        if let Some(confirm) = self.confirm.take() {
            window::close(confirm.id)
        } else {
            Task::none()
        }
    }

    /// 按窗口渲染界面（阶段 G0：仅确认窗）
    pub fn view<'a>(&'a self, window_id: window::Id) -> Element<'a, Message> {
        if let Some(confirm) = &self.confirm {
            if confirm.id == window_id {
                return container(
                    column![
                        text(&confirm.message),
                        row![
                            button(text("取消")).on_press(Message::CancelPressed),
                            button(text("退出")).on_press(Message::ConfirmPressed),
                        ]
                        .spacing(8),
                    ]
                    .spacing(16)
                    .width(iced::Fill)
                    .height(iced::Fill)
                    .padding(16),
                )
                .into();
            }
        }
        // 尚未创建的实际窗口：渲染空容器（iced 要求每窗口都有 view）
        container(text("")).into()
    }

    /// 主题（阶段 G0：按启动时解析的明暗状态选取 iced 内建主题）
    pub fn theme(&self, _window: window::Id) -> Theme {
        if self.dark {
            Theme::Dark
        } else {
            Theme::Light
        }
    }

    /// 订阅：合并「主线程指令流」与「窗口关闭事件流」
    pub fn subscription(&self) -> Subscription<Message> {
        let mut slot = self.cmd_stream.borrow_mut();
        // 首调用取出桥接接收端，构建常驻订阅流；之后返回 None
        let cmd_sub = slot.take().map_or_else(Subscription::none, |rx| {
            Subscription::run_with_id(
                "wintag-main-cmd",
                iced::futures::stream::unfold(rx, |mut rx| async move {
                    rx.next().await.map(|cmd| (Message::Command(cmd), rx))
                }),
            )
        });
        Subscription::batch([cmd_sub, window::close_events().map(Message::WindowClosed)])
    }
}
