//! iced 图形界面应用（D27：四个 GUI 窗口迁至 iced）
//!
//! 本模块运行在**独立线程**（`main.rs` 以 `std::thread` 启动），用 [`iced::daemon`]
//! 的多窗口模型承担 `confirm` / `settings` / `popup` / `panel` 四个窗口。阶段 G0
//! 落地退出确认窗、G2 迁移设置窗、G3 迁移标签编辑弹窗，G4 收尾概览面板。
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
use std::collections::HashSet;

use iced::futures::StreamExt;
use iced::keyboard::key::{Key, Named};
use iced::widget::{
    button, checkbox, column, combo_box, container, row, scrollable, text, text_input,
};
use iced::window;
use iced::{Element, Length, Size, Subscription, Task, Theme};

use crossbeam_channel::{Receiver, Sender};

use crate::core::settings::{CornerPreference, Settings, ThemeMode};
use crate::core::tag::{Tag, TagColor};
use crate::ui::iced_proto::{plan_popup_action, GuiEvent, IcedCommand, PopupPlan, TagRow};

/// 退出确认窗口的目标尺寸（逻辑像素，iced 自管 DPI）
const CONFIRM_W: f32 = 380.0;
const CONFIRM_H: f32 = 160.0;
/// 设置窗口目标尺寸
const SETTINGS_W: f32 = 420.0;
const SETTINGS_H: f32 = 480.0;
/// 标签弹窗目标尺寸（与 Win32 版 `popup` 的 420×320 一致）
const POPUP_W: f32 = 420.0;
const POPUP_H: f32 = 320.0;
/// 概览面板目标尺寸（与 Win32 版 R14 的 400×640 一致）
const PANEL_W: f32 = 400.0;
const PANEL_H: f32 = 640.0;

/// 五色选项（顺序与 Win32 版一致）
const TAG_COLORS: [(TagColor, &str); 5] = [
    (TagColor::Orange, "橙"),
    (TagColor::Blue, "蓝"),
    (TagColor::Green, "绿"),
    (TagColor::Red, "红"),
    (TagColor::Purple, "紫"),
];

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

/// 标签编辑弹窗的状态
struct PopupWindow {
    /// 窗口 id
    id: window::Id,
    /// 目标窗口句柄（isize）
    target: isize,
    /// 可编辑标题
    title: String,
    /// 可编辑备注
    note: String,
    /// 当前选中颜色
    color: TagColor,
    /// 目标窗口标题（只读展示，随保存带出）
    window_title: String,
    /// 目标窗口进程名（只读展示，随保存带出）
    process_name: String,
    /// 标题输入框 id（用于打开时聚焦）
    title_id: text_input::Id,
}

/// 概览面板的状态
struct PanelState {
    /// 窗口 id
    id: window::Id,
    /// 标签行列表（主线程经 `RefreshTags` 下发）
    rows: Vec<TagRow>,
    /// 搜索过滤关键字（空串 = 不过滤）
    search: String,
    /// 已展开（显示备注）的目标窗口句柄集合
    expanded: HashSet<isize>,
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
    /// 当前显示的标签弹窗（`None` = 未显示）
    popup: Option<PopupWindow>,
    /// 当前显示的概览面板（`None` = 未显示）
    panel: Option<PanelState>,
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
    // ---- 标签弹窗交互 ----
    /// 标题输入框内容变更
    PopupTitleChanged(String),
    /// 备注输入框内容变更
    PopupNoteChanged(String),
    /// 颜色块选择
    PopupColorSelected(TagColor),
    /// 点击"保存"（发出 [`GuiEvent::TagSaved`] 并关闭窗口）
    PopupSavePressed,
    /// 取消（点击"取消" / Esc / 关闭窗口）
    PopupCancelPressed,
    // ---- 概览面板交互 ----
    /// 搜索框内容变更
    PanelSearchChanged(String),
    /// 展开/收起某行的备注
    PanelToggleExpand(isize),
    /// 全部展开
    PanelExpandAll,
    /// 全部收起
    PanelCollapseAll,
    /// 行点击（激活/置前目标窗口）
    PanelRowActivated(isize),
    /// 行"编辑"按钮
    PanelRowEdit(isize),
    /// 行"移除"按钮
    PanelRowRemove(isize),
    /// 面板底部"退出"
    PanelExitPressed,
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
                popup: None,
                panel: None,
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
        } else if self.popup.as_ref().is_some_and(|c| c.id == window) {
            "标记窗口".to_string()
        } else if self.panel.as_ref().is_some_and(|c| c.id == window) {
            "WinTag 概览".to_string()
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
                if self.popup.as_ref().is_some_and(|c| c.id == id) {
                    self.popup = None;
                }
                if self.panel.as_ref().is_some_and(|c| c.id == id) {
                    self.panel = None;
                    let _ = self.gui_tx.send(GuiEvent::PanelVisibilityChanged(false));
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
            // ---- 标签弹窗 ----
            Message::PopupTitleChanged(s) => {
                if let Some(p) = &mut self.popup {
                    p.title = s;
                }
                Task::none()
            }
            Message::PopupNoteChanged(s) => {
                if let Some(p) = &mut self.popup {
                    p.note = s;
                }
                Task::none()
            }
            Message::PopupColorSelected(c) => {
                if let Some(p) = &mut self.popup {
                    p.color = c;
                }
                Task::none()
            }
            Message::PopupSavePressed => {
                let saved_id = if let Some(p) = &self.popup {
                    let tag = Tag {
                        title: p.title.clone(),
                        note: p.note.clone(),
                        color: p.color,
                        window_title: p.window_title.clone(),
                        process_name: p.process_name.clone(),
                    };
                    let _ = self.gui_tx.send(GuiEvent::TagSaved {
                        target: p.target,
                        tag,
                    });
                    Some(p.id)
                } else {
                    None
                };
                match saved_id {
                    Some(id) => {
                        self.popup = None;
                        window::close(id)
                    }
                    None => Task::none(),
                }
            }
            Message::PopupCancelPressed => {
                if let Some(p) = self.popup.take() {
                    window::close(p.id)
                } else {
                    Task::none()
                }
            }
            // ---- 概览面板 ----
            Message::PanelSearchChanged(s) => {
                if let Some(p) = &mut self.panel {
                    p.search = s;
                }
                Task::none()
            }
            Message::PanelToggleExpand(target) => {
                if let Some(p) = &mut self.panel {
                    if !p.expanded.remove(&target) {
                        p.expanded.insert(target);
                    }
                }
                Task::none()
            }
            Message::PanelExpandAll => {
                if let Some(p) = &mut self.panel {
                    p.expanded = p.rows.iter().map(|r| r.target).collect();
                }
                Task::none()
            }
            Message::PanelCollapseAll => {
                if let Some(p) = &mut self.panel {
                    p.expanded.clear();
                }
                Task::none()
            }
            Message::PanelRowActivated(target) => {
                let _ = self.gui_tx.send(GuiEvent::ActivateWindow { target });
                Task::none()
            }
            Message::PanelRowEdit(target) => {
                let _ = self.gui_tx.send(GuiEvent::EditTagRequested { target });
                Task::none()
            }
            Message::PanelRowRemove(target) => {
                let _ = self.gui_tx.send(GuiEvent::RemoveTag { target });
                Task::none()
            }
            Message::PanelExitPressed => {
                let _ = self.gui_tx.send(GuiEvent::PanelExit);
                Task::none()
            }
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
            IcedCommand::EditTag {
                target,
                position,
                tag,
            } => self.open_popup(target, position, tag),
            IcedCommand::ShowPanel => self.show_panel(),
            IcedCommand::HidePanel => self.hide_panel(),
            IcedCommand::RefreshTags { rows } => self.refresh_panel(rows),
        }
    }

    /// 显示概览面板；已显示则置前聚焦
    fn show_panel(&mut self) -> Task<Message> {
        if let Some(p) = &self.panel {
            return window::gain_focus(p.id);
        }
        let (id, open) = window::open(window::Settings {
            position: window::Position::Centered,
            size: Size::new(PANEL_W, PANEL_H),
            min_size: Some(Size::new(300.0, 360.0)),
            ..window::Settings::default()
        });
        self.panel = Some(PanelState {
            id,
            rows: Vec::new(),
            search: String::new(),
            expanded: HashSet::new(),
        });
        open.map(|_| Message::Noop)
    }

    /// 隐藏概览面板（关闭窗口；其后 WindowClosed 会上报 PanelVisibilityChanged(false)）
    fn hide_panel(&mut self) -> Task<Message> {
        if let Some(p) = self.panel.take() {
            window::close(p.id)
        } else {
            Task::none()
        }
    }

    /// 刷新面板列表：替换标签快照，并只保留仍存在的目标在展开集合中
    fn refresh_panel(&mut self, rows: Vec<TagRow>) -> Task<Message> {
        if let Some(p) = &mut self.panel {
            p.rows = rows;
            let live: HashSet<isize> = p.rows.iter().map(|r| r.target).collect();
            p.expanded.retain(|t| live.contains(t));
        }
        Task::none()
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

    /// 打开/复用标签编辑弹窗（G3，单例语义由 `plan_popup_action` 决策）
    fn open_popup(&mut self, target: isize, position: (i32, i32), tag: Tag) -> Task<Message> {
        match plan_popup_action(
            self.popup.as_ref().map(|p| (p.target, 0)),
            target,
            self.popup.is_some(),
        ) {
            // 同目标且旧弹窗存活：复用并聚焦标题框（不新建不销毁）
            PopupPlan::Reuse(_) => {
                if let Some(p) = &self.popup {
                    text_input::focus::<Message>(p.title_id.clone())
                } else {
                    Task::none()
                }
            }
            // 异目标且旧弹窗存活：销毁旧弹窗后新建
            PopupPlan::Replace(_) => {
                let old = self.popup.take();
                let open = self.open_popup_fresh(target, position, tag);
                if let Some(old) = old {
                    Task::batch([window::close(old.id), open])
                } else {
                    open
                }
            }
            // 首次 / 旧弹窗已销毁：直接新建
            PopupPlan::Fresh => self.open_popup_fresh(target, position, tag),
        }
    }

    /// 新建标签弹窗（按主线程算好的位置 + 定尺寸，并聚焦标题框）
    fn open_popup_fresh(&mut self, target: isize, position: (i32, i32), tag: Tag) -> Task<Message> {
        let title_id = text_input::Id::unique();
        let (id, open) = window::open(window::Settings {
            position: window::Position::Specific(iced::Point::new(
                position.0 as f32,
                position.1 as f32,
            )),
            size: Size::new(POPUP_W, POPUP_H),
            resizable: false,
            ..window::Settings::default()
        });
        self.popup = Some(PopupWindow {
            id,
            target,
            title: tag.title.clone(),
            note: tag.note.clone(),
            color: tag.color,
            window_title: tag.window_title.clone(),
            process_name: tag.process_name.clone(),
            title_id: title_id.clone(),
        });
        Task::batch([
            open.map(|_| Message::Noop),
            text_input::focus::<Message>(title_id),
        ])
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
        if let Some(p) = &self.popup {
            if p.id == window_id {
                return popup_view(p);
            }
        }
        if let Some(panel) = &self.panel {
            if panel.id == window_id {
                return panel_view(panel);
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

    /// 订阅：合并「主线程指令流」「窗口关闭事件流」与「Esc 取消弹窗」
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
        let esc_sub = iced::keyboard::on_key_press(|key, _modifiers| {
            if key == Key::Named(Named::Escape) {
                Some(Message::PopupCancelPressed)
            } else {
                None
            }
        });
        Subscription::batch([
            cmd_sub,
            window::close_events().map(Message::WindowClosed),
            esc_sub,
        ])
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

/// 标签弹窗视图（窗口/进程只读 + 标题/备注输入 + 五色块 + 保存/取消）
fn popup_view(p: &PopupWindow) -> Element<'_, Message> {
    let title = text_input("标题", &p.title)
        .on_input(Message::PopupTitleChanged)
        .on_submit(Message::PopupSavePressed)
        .id(p.title_id.clone());
    let note = text_input("备注", &p.note)
        .on_input(Message::PopupNoteChanged)
        .on_submit(Message::PopupSavePressed);

    let colors = row![
        color_swatch(TAG_COLORS[0].0, TAG_COLORS[0].1, p.color),
        color_swatch(TAG_COLORS[1].0, TAG_COLORS[1].1, p.color),
        color_swatch(TAG_COLORS[2].0, TAG_COLORS[2].1, p.color),
        color_swatch(TAG_COLORS[3].0, TAG_COLORS[3].1, p.color),
        color_swatch(TAG_COLORS[4].0, TAG_COLORS[4].1, p.color),
    ]
    .spacing(8);

    container(
        column![
            text(format!("{} / {}", p.window_title, p.process_name)).size(12),
            title,
            note,
            colors,
            row![
                button(text("取消")).on_press(Message::PopupCancelPressed),
                button(text("保存")).on_press(Message::PopupSavePressed),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .width(Length::Fill)
        .padding(16),
    )
    .into()
}

/// 单个颜色块按钮（选中项以实心圆点标记，未选中为空心圆点）
fn color_swatch(
    color: TagColor,
    label: &'static str,
    selected: TagColor,
) -> iced::widget::Button<'static, Message> {
    let marker = if color == selected { "●" } else { "○" };
    button(text(format!("{marker} {label}"))).on_press(Message::PopupColorSelected(color))
}

/// 概览面板视图：搜索框 + 标签列表（可展开备注）+ 全部展开/收起 + 底部"退出"
fn panel_view(panel: &PanelState) -> Element<'_, Message> {
    let search = text_input("搜索标题/备注/窗口/进程", &panel.search)
        .on_input(Message::PanelSearchChanged)
        .width(Length::Fill);

    let rows = filter_rows(&panel.rows, &panel.search);
    let list = if rows.is_empty() {
        column![text("无匹配标签").size(14)].padding(8)
    } else {
        column(
            rows.into_iter()
                .map(|row| panel_row(panel, &row))
                .collect::<Vec<_>>(),
        )
        .spacing(4)
    };

    let content = column![
        search,
        row![
            button(text("全部展开")).on_press(Message::PanelExpandAll),
            button(text("全部收起")).on_press(Message::PanelCollapseAll),
        ]
        .spacing(8),
        scrollable(list).width(Length::Fill).height(Length::Fill),
        row![button(text("退出")).on_press(Message::PanelExitPressed)],
    ]
    .spacing(12)
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill);

    content.into()
}

/// 单个标签行：标题|窗口名 点击置前；展开时显示备注 + 置前/编辑/移除按钮
///
/// 全部文案克隆为自持 `'static` 字符串（行数据由调用方持有），按钮消息携带
/// `target` 副本，故返回 `Element<'static, Message>`。
fn panel_row(panel: &PanelState, row: &TagRow) -> Element<'static, Message> {
    let expanded = panel.expanded.contains(&row.target);
    let header = row![
        button(text(if expanded { "▾" } else { "▸" }))
            .on_press(Message::PanelToggleExpand(row.target)),
        button(text(format!(
            "{} | {}",
            row.tag.title, row.tag.window_title
        )))
        .on_press(Message::PanelRowActivated(row.target)),
    ]
    .spacing(4)
    .width(Length::Fill);

    if expanded {
        let actions = row![
            button(text("置前")).on_press(Message::PanelRowActivated(row.target)),
            button(text("编辑")).on_press(Message::PanelRowEdit(row.target)),
            button(text("移除")).on_press(Message::PanelRowRemove(row.target)),
        ]
        .spacing(8);
        let note = if row.tag.note.is_empty() {
            text("（无）").size(12)
        } else {
            text(row.tag.note.clone()).size(12)
        };
        column![header, note, actions]
            .spacing(4)
            .padding(4)
            .width(Length::Fill)
            .into()
    } else {
        header.into()
    }
}

/// 搜索过滤（纯函数，可单测）：匹配标题/备注/窗口名/进程名（大小写不敏感）
fn filter_rows(rows: &[TagRow], query: &str) -> Vec<TagRow> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|r| {
            r.tag.title.to_lowercase().contains(&q)
                || r.tag.note.to_lowercase().contains(&q)
                || r.tag.window_title.to_lowercase().contains(&q)
                || r.tag.process_name.to_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(title: &str, note: &str, target: isize) -> TagRow {
        TagRow {
            target,
            tag: Tag {
                title: title.to_string(),
                note: note.to_string(),
                color: TagColor::Orange,
                window_title: format!("win-{title}"),
                process_name: format!("proc-{title}"),
            },
        }
    }

    /// 空查询返回全部；非空按标题/备注/窗口/进程任一字段匹配（大小写不敏感）
    #[test]
    fn filter_rows_matches_any_field_case_insensitive() {
        let rows = vec![row("记事本", "工作便签", 1), row("浏览器", "阅读", 2)];
        assert_eq!(filter_rows(&rows, "").len(), 2);
        assert_eq!(filter_rows(&rows, "记事").len(), 1);
        assert_eq!(filter_rows(&rows, "阅读").len(), 1);
        assert_eq!(filter_rows(&rows, "WIN-浏览器").len(), 1);
        assert_eq!(filter_rows(&rows, "proc-浏览器").len(), 1);
        assert_eq!(filter_rows(&rows, "不存在").len(), 0);
        // 大小写不敏感
        assert_eq!(filter_rows(&rows, "WIN-记事本").len(), 1);
    }
}
