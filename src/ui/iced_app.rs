//! iced 图形界面应用（D27：四个 GUI 窗口迁至 iced）
//!
//! 本模块运行在**独立线程**（`main.rs` 以 `std::thread` 启动），用 [`iced::daemon`]
//! 的多窗口模型承担 `confirm` / `settings` / `popup` / `panel` 四个窗口。阶段 G0
//! 落地退出确认窗（最小闭环）、G2 迁移设置窗，其余窗口由后续阶段（G3-G4）迁入。
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
use iced::widget::{button, checkbox, column, combo_box, container, row, text};
use iced::window;
use iced::{Element, Length, Size, Subscription, Task, Theme};

use crossbeam_channel::{Receiver, Sender};

use crate::core::settings::{CornerPreference, Settings, ThemeMode};
use crate::ui::iced_proto::{GuiEvent, IcedCommand};

/// 退出确认窗口的目标尺寸（逻辑像素，iced 自管 DPI）
const CONFIRM_W: f32 = 380.0;
const CONFIRM_H: f32 = 160.0;
/// 设置窗口目标尺寸
const SETTINGS_W: f32 = 420.0;
const SETTINGS_H: f32 = 480.0;

/// 退出确认窗的状态
struct ConfirmState {
    /// 窗口 id（`window::open` 返回，随窗口存在而有效）
    id: window::Id,
    /// 提示文本（"确定退出？将丢弃 N 个标签/便签"）
    message: String,
}

/// 设置窗的状态（含可编辑草稿与两个下拉框的持久状态）
struct SettingsWindow {
    /// 窗口 id
    id: window::Id,
    /// 可编辑草稿（`Settings` 全字段为可拷贝标量）
    draft: Settings,
    /// 主题下拉框的持久 widget 状态
    theme_state: combo_box::State<ThemeMode>,
    /// 圆角下拉框的持久 widget 状态
    corner_state: combo_box::State<CornerPreference>,
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
    /// 当前显示的设置窗（`None` = 未显示）
    settings: Option<SettingsWindow>,
    /// 是否暗色主题（启动时解析；经 `ApplyTheme` 热更新）
    dark: bool,
}

/// iced 应用的消息（`Send + Debug + 'static`，iced 事件循环要求）
#[derive(Debug, Clone)]
pub enum Message {
    /// 主线程经跨线程通道送达的指令
    Command(IcedCommand),
    /// 空操作（如窗口创建任务完成）
    Noop,
    /// 窗口被关闭（用户点关闭按钮 / 系统关闭）
    WindowClosed(window::Id),
    /// 点击"退出"确认按钮（确认窗默认动作）
    ConfirmPressed,
    /// 点击"取消"按钮（确认窗）
    CancelPressed,
    // ---- 设置窗交互 ----
    /// 主题下拉框选择变更
    SettingsThemeSelected(ThemeMode),
    /// 圆角下拉框选择变更
    SettingsCornerSelected(CornerPreference),
    /// "角标显示标题"复选框
    SettingsTitleToggled(bool),
    /// "角标始终置顶"复选框
    SettingsTopToggled(bool),
    /// "气泡提示"复选框
    SettingsBalloonToggled(bool),
    /// 点击"保存"（发出 [`GuiEvent::SettingsChanged`] 并关闭窗口）
    SettingsSavePressed,
    /// 点击"取消"（关闭窗口，不保存）
    SettingsCancelPressed,
}

impl WinTagApp {
    /// 创建应用与初始状态（iced 线程启动入口）
    ///
    /// 启动时派生桥接线程：阻塞接收主线程的 [`IcedCommand`] 并转发进
    /// `futures` 无界通道，供 `subscription` 作为异步流消费。初始不显示任何窗口
    /// （`iced::daemon` 在零窗口下保持存活，按需打开窗口）。
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
                settings: None,
                dark,
            },
            Task::none(),
        )
    }

    /// 每窗口标题
    pub fn title(&self, window: window::Id) -> String {
        if self.confirm.as_ref().is_some_and(|c| c.id == window) {
            "退出确认".to_string()
        } else if self.settings.as_ref().is_some_and(|c| c.id == window) {
            "设置".to_string()
        } else {
            "WinTag".to_string()
        }
    }

    /// 状态更新：处理跨线程指令与界面事件，返回驱动窗口/退出流程的 [`Task`]
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Command(cmd) => self.handle_command(cmd),
            Message::Noop => Task::none(),
            Message::WindowClosed(id) => {
                if self.confirm.as_ref().is_some_and(|c| c.id == id) {
                    self.confirm = None;
                }
                if self.settings.as_ref().is_some_and(|c| c.id == id) {
                    self.settings = None;
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
            // ---- 设置窗 ----
            Message::SettingsThemeSelected(v) => {
                if let Some(sw) = &mut self.settings {
                    sw.draft.theme = v;
                }
                Task::none()
            }
            Message::SettingsCornerSelected(v) => {
                if let Some(sw) = &mut self.settings {
                    sw.draft.corner = v;
                }
                Task::none()
            }
            Message::SettingsTitleToggled(v) => {
                if let Some(sw) = &mut self.settings {
                    sw.draft.show_badge_title = v;
                }
                Task::none()
            }
            Message::SettingsTopToggled(v) => {
                if let Some(sw) = &mut self.settings {
                    sw.draft.badge_always_top = v;
                }
                Task::none()
            }
            Message::SettingsBalloonToggled(v) => {
                if let Some(sw) = &mut self.settings {
                    sw.draft.show_balloon = v;
                }
                Task::none()
            }
            Message::SettingsSavePressed => {
                let draft = self.settings.as_ref().map(|s| s.draft).unwrap_or_default();
                let _ = self.gui_tx.send(GuiEvent::SettingsChanged(draft));
                self.close_settings()
            }
            Message::SettingsCancelPressed => self.close_settings(),
        }
    }

    /// 处理主线程指令
    fn handle_command(&mut self, cmd: IcedCommand) -> Task<Message> {
        match cmd {
            IcedCommand::ShowConfirm { count } => self.open_confirm(count),
            IcedCommand::CloseConfirm => self.close_confirm(),
            IcedCommand::OpenSettings => self.open_settings(),
            IcedCommand::ApplyTheme { dark } => {
                self.dark = dark;
                Task::none()
            }
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
        open.map(|_| Message::Noop)
    }

    /// 关闭确认窗（并将其从状态移除；`window::close` 返回关闭任务）
    fn close_confirm(&mut self) -> Task<Message> {
        if let Some(confirm) = self.confirm.take() {
            window::close(confirm.id)
        } else {
            Task::none()
        }
    }

    /// 打开设置窗：读全局设置快照预填表单并创建设置窗口
    fn open_settings(&mut self) -> Task<Message> {
        let cfg = crate::core::settings::global_settings()
            .and_then(|s| s.lock().ok().map(|guard| *guard))
            .unwrap_or_default();
        let (id, open) = window::open(window::Settings {
            position: window::Position::Centered,
            size: Size::new(SETTINGS_W, SETTINGS_H),
            resizable: false,
            ..window::Settings::default()
        });
        self.settings = Some(SettingsWindow {
            id,
            draft: cfg,
            theme_state: combo_box::State::new(vec![
                ThemeMode::System,
                ThemeMode::Light,
                ThemeMode::Dark,
            ]),
            corner_state: combo_box::State::new(vec![
                CornerPreference::Default,
                CornerPreference::Round,
                CornerPreference::SmallRound,
            ]),
        });
        open.map(|_| Message::Noop)
    }

    /// 关闭设置窗
    fn close_settings(&mut self) -> Task<Message> {
        if let Some(sw) = self.settings.take() {
            window::close(sw.id)
        } else {
            Task::none()
        }
    }

    /// 按窗口渲染界面
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
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(16),
                )
                .into();
            }
        }
        if let Some(sw) = &self.settings {
            if sw.id == window_id {
                return settings_view(sw);
            }
        }
        // 尚未创建的实际窗口：渲染空容器（iced 要求每窗口都有 view）
        container(text("")).into()
    }

    /// 主题（按当前明暗状态选取 iced 内建主题，经 `ApplyTheme` 热更新）
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

/// 设置窗视图（主题/圆角下拉 + 三个复选框 + 保存/取消）
fn settings_view(sw: &SettingsWindow) -> Element<'_, Message> {
    let theme = combo_box::ComboBox::new(
        &sw.theme_state,
        "主题",
        Some(&sw.draft.theme),
        Message::SettingsThemeSelected,
    )
    .width(Length::Fill);
    let corner = combo_box::ComboBox::new(
        &sw.corner_state,
        "圆角",
        Some(&sw.draft.corner),
        Message::SettingsCornerSelected,
    )
    .width(Length::Fill);

    container(
        column![
            text("主题"),
            theme,
            text("圆角"),
            corner,
            checkbox("角标显示标题", sw.draft.show_badge_title)
                .on_toggle(Message::SettingsTitleToggled),
            checkbox("角标始终置顶", sw.draft.badge_always_top)
                .on_toggle(Message::SettingsTopToggled),
            checkbox("气泡提示", sw.draft.show_balloon).on_toggle(Message::SettingsBalloonToggled),
            row![
                button(text("取消")).on_press(Message::SettingsCancelPressed),
                button(text("保存")).on_press(Message::SettingsSavePressed),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .width(Length::Fill)
        .padding(16),
    )
    .into()
}
