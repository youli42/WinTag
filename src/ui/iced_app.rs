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

use std::collections::HashSet;

use iced::keyboard::key::{Key, Named};
use iced::widget::{
    button, checkbox, column, combo_box, container, mouse_area, operation, row, scrollable, text,
    text_input, Id,
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
    title_id: Id,
}

/// 行内编辑的字段（标题或备注，D28 双击编辑）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    /// 标签标题
    Title,
    /// 标签备注
    Note,
}

/// 行内编辑会话（D28）：目标窗口 + 编辑的字段 + 当前草稿
struct PanelEdit {
    /// 被编辑标签的目标窗口句柄
    target: isize,
    /// 正在编辑的字段（标题 / 备注）
    field: EditField,
    /// 编辑中的草稿文本（`text_input` 当前值）
    draft: String,
}

/// 拖拽排序会话（D28）：按下手柄的源行 + 当前悬停的目标行（预览插入位）
struct PanelDrag {
    /// 拖押起的源行 target
    from_target: isize,
    /// 当前鼠标悬停的行 target（`None` = 尚未进入任何行）
    over_target: Option<isize>,
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
    /// 当前鼠标悬停的行目标（`None` = 无；供卡片 hover 高亮，D28）
    hovered: Option<isize>,
    /// 当前行内编辑会话（`None` = 未编辑）
    editing: Option<PanelEdit>,
    /// 当前拖拽排序会话（`None` = 未拖拽；D28）
    drag: Option<PanelDrag>,
}

/// iced 应用状态（运行在 iced 线程，单线程访问，无需 `Send`）
///
/// 仅 [`Message`] 需要 `Send`（iced 事件循环要求），状态本体可含 `RefCell`。
pub struct WinTagApp {
    /// 回传 iced 事件的发送器（iced 线程 → 主线程）
    gui_tx: Sender<GuiEvent>,
    /// 主线程命令接收端（crossbeam；`update` 经稳定的 `time::every` 订阅周期性排空，
    /// 避免依赖「单次构建的异步流/订阅」被 iced Tracker 剪除——首个命令后再无响应）。
    cmd_rx: crossbeam_channel::Receiver<IcedCommand>,
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
    /// 空操作（如窗口创建任务完成）
    Noop,
    /// 周期 tick：排空主线程命令队列（`cmd_rx`）并逐条处理
    Pump,
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
    /// Esc（全局键盘：按模态优先级分派——确认窗=取消退出、设置窗=不保存关闭、
    /// 概览面板=关闭、标签弹窗=取消。弹窗输入框 Esc 亦触发）。
    EscapePressed,
    /// 回车（全局键盘：确认窗=确认退出，设置窗=保存；弹窗输入框已用 on_submit）
    EnterPressed,
    /// Tab（全局键盘：仅在标签弹窗打开时切换输入框焦点；`backwards` = Shift+Tab）
    TabPressed { backwards: bool },
    /// 标签弹窗窗口创建完成（`open` 任务完成，接口已建）。此时聚焦标题框——
    /// `operation::focus` 是同步 Widget 操作，若与 `window::open` 同批返回会在
    /// 窗口接口存在之前空转，故延迟到此刻。
    PopupOpened(window::Id),
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
    /// 位置进入某行（mouse_area on_enter，D28 hover 高亮）
    PanelRowEntered(isize),
    /// 位置离开某行（mouse_area on_exit，D28 hover 高亮）
    PanelRowExited,
    /// 双击标题/备注开始行内编辑（D28）
    PanelBeginEdit { target: isize, field: EditField },
    /// 行内编辑输入框内容变更（D28）
    PanelEditInput(String),
    /// 行内编辑提交（回车保存，发 GuiEvent::TagSaved）
    PanelEditCommit,
    /// 行内编辑取消（Esc）
    PanelEditCancel,
    /// 拖拽手柄按下（D28：开始拖拽排序）
    PanelDragStart(isize),
    /// 拖拽中悬停到某行（D28：更新预览插入位）
    PanelDragHover(isize),
    /// 拖拽释放（D28：提交 ReorderTags）
    PanelDragDrop,
    /// 拖拽取消/拖出列表（D28：清空拖拽态）
    PanelDragCancel,
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
        (
            Self {
                gui_tx,
                cmd_rx,
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
            Message::Pump => self.drain_commands(),
            Message::Noop => Task::none(),
            Message::WindowClosed(id) => {
                if self.confirm.as_ref().is_some_and(|c| c.id == id) {
                    self.confirm = None;
                    // Esc/取消按钮经 CancelPressed 走 close_confirm 已发 CancelExit；此处
                    // 兜底覆盖 X 按钮/系统关闭路径（GuiEvent::CancelExit 契约承诺三来源：
                    // 取消按钮 / Esc / 关闭窗口），保证主线程退出意图一致。
                    let _ = self.gui_tx.send(GuiEvent::CancelExit);
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
            // Esc：按模态优先级分派关闭动作。`on_key_press` 订阅不携带窗口 id，
            // 无法从订阅闭包判断焦点窗，故在 update 内按"当前打开的窗口"优先级处理
            // ——确认窗>设置窗>概览面板>标签弹窗，与回车（EnterPressed）同构。
            Message::EscapePressed => {
                if self.confirm.is_some() {
                    let _ = self.gui_tx.send(GuiEvent::CancelExit);
                    return self.close_confirm();
                }
                if self.settings.is_some() {
                    return self.close_settings();
                }
                // 编辑优先（D28）：面板行内编辑中按 Esc 先取消编辑，而非直接关面板
                if self.panel.as_ref().is_some_and(|p| p.editing.is_some()) {
                    if let Some(p) = &mut self.panel {
                        p.editing = None;
                    }
                    return Task::none();
                }
                if self.panel.is_some() {
                    // 关闭面板：window::close 走 WindowClosed → PanelVisibilityChanged(false)
                    // 回报可见性（与 hide_panel 语义一致）
                    if let Some(p) = self.panel.take() {
                        return window::close(p.id);
                    }
                    return Task::none();
                }
                if self.popup.is_some() {
                    if let Some(p) = self.popup.take() {
                        return window::close(p.id);
                    }
                }
                Task::none()
            }
            // 回车：确认窗=确认退出；设置窗=保存；弹窗输入框已用 on_submit 提交，不重复处理。
            Message::EnterPressed => {
                if self.confirm.is_some() {
                    let _ = self.gui_tx.send(GuiEvent::ConfirmExit);
                    return self.close_confirm();
                }
                if self.settings.is_some() {
                    let draft = self.settings.as_ref().map(|s| s.draft).unwrap_or_default();
                    let _ = self.gui_tx.send(GuiEvent::SettingsChanged(draft));
                    return self.close_settings();
                }
                Task::none()
            }
            // Tab：iced 0.14 无默认 Tab 焦点导航（0.13 亦然），须在此显式接线。
            // 0.14 的内建 focusable 仅 text_input/text_editor（按钮不注册 focusable），
            // 故仅在标签弹窗（含标题/备注两个输入框）打开时切换焦点；其余窗口忽略。
            // `focus_next`/`focus_previous` 是线性无环绕：两个输入框在 Tab/Shift+Tab 间切换。
            Message::TabPressed { backwards } => {
                if self.popup.is_some() {
                    return if backwards {
                        operation::focus_previous()
                    } else {
                        operation::focus_next()
                    };
                }
                Task::none()
            }
            // 弹窗创建完成（`open` 任务完成，窗口接口已建）。此刻聚焦标题框 + 置前：
            // `operation::focus` 是同步 Widget 操作，须在接口存在后执行，否则空转。
            Message::PopupOpened(id) => {
                if let Some(p) = &self.popup {
                    return Task::batch([
                        operation::focus(p.title_id.clone()),
                        window::gain_focus(id),
                    ]);
                }
                Task::none()
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
            Message::PanelRowEntered(target) => {
                if let Some(p) = &mut self.panel {
                    p.hovered = Some(target);
                }
                Task::none()
            }
            Message::PanelRowExited => {
                if let Some(p) = &mut self.panel {
                    p.hovered = None;
                }
                Task::none()
            }
            // 双击开始行内编辑：从 rows 找到该 target 的 tag，取对应字段作草稿
            Message::PanelBeginEdit { target, field } => {
                if let Some(p) = &mut self.panel {
                    if let Some(row) = p.rows.iter().find(|r| r.target == target) {
                        let draft = match field {
                            EditField::Title => row.tag.title.clone(),
                            EditField::Note => row.tag.note.clone(),
                        };
                        p.editing = Some(PanelEdit {
                            target,
                            field,
                            draft,
                        });
                    }
                }
                Task::none()
            }
            Message::PanelEditInput(s) => {
                if let Some(p) = &mut self.panel {
                    if let Some(edit) = &mut p.editing {
                        edit.draft = s;
                    }
                }
                Task::none()
            }
            // 提交：合并草稿到对应 tag（纯函数可单测），发 TagSaved + 本地更新 + 清编辑
            Message::PanelEditCommit => {
                if let Some(p) = &mut self.panel {
                    if let Some(edit) = p.editing.take() {
                        if p.rows.iter().any(|r| r.target == edit.target) {
                            let (new_rows, tag) = apply_edit_to_rows(&p.rows, &edit);
                            p.rows = new_rows;
                            let _ = self.gui_tx.send(GuiEvent::TagSaved {
                                target: edit.target,
                                tag,
                            });
                        }
                    }
                }
                Task::none()
            }
            Message::PanelEditCancel => {
                if let Some(p) = &mut self.panel {
                    p.editing = None;
                }
                Task::none()
            }
            // 拖拽排序开始（D28）：记录源行 target（禁止在搜索/编辑时拖拽）
            Message::PanelDragStart(target) => {
                if let Some(p) = &mut self.panel {
                    if p.search.trim().is_empty() && p.editing.is_none() {
                        p.drag = Some(PanelDrag {
                            from_target: target,
                            over_target: None,
                        });
                    }
                }
                Task::none()
            }
            // 拖拽中悬停某行（D28）：更新预览插入位
            Message::PanelDragHover(target) => {
                if let Some(p) = &mut self.panel {
                    if let Some(drag) = &mut p.drag {
                        if drag.from_target != target {
                            drag.over_target = Some(target);
                        }
                    }
                }
                Task::none()
            }
            // 拖拽释放（D28）：计算新顺序 → 本地更新 + 发 ReorderTags 回主线程
            Message::PanelDragDrop => {
                if let Some(p) = &mut self.panel {
                    if let Some(drag) = p.drag.take() {
                        if let Some(over) = drag.over_target {
                            if over != drag.from_target {
                                let order = preview_reorder(&p.rows, &drag);
                                p.rows = reorder_rows(&p.rows, &order);
                                let _ = self.gui_tx.send(GuiEvent::ReorderTags { targets: order });
                            }
                        }
                    }
                }
                Task::none()
            }
            // 拖拽取消/拖出列表（D28）：清空拖拽态
            Message::PanelDragCancel => {
                if let Some(p) = &mut self.panel {
                    p.drag = None;
                }
                Task::none()
            }
            Message::PanelExitPressed => {
                let _ = self.gui_tx.send(GuiEvent::PanelExit);
                Task::none()
            }
        }
    }

    /// 排空主线程命令队列（crossbeam `try_recv`），逐条经 [`Self::handle_command`] 处理
    ///
    /// 由稳定的 `iced::time::every` 订阅周期触发；命令订阅不依赖「单次构建的异步流」，
    /// 因此不会被 iced Tracker 剪除——首个命令后仍持续可用。
    fn drain_commands(&mut self) -> Task<Message> {
        let mut tasks: Vec<Task<Message>> = Vec::new();
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            tasks.push(self.handle_command(cmd));
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            Task::batch(tasks)
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
            hovered: None,
            editing: None,
            drag: None,
        });
        Task::batch([open.map(|_| Message::Noop), window::gain_focus(id)])
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
        // 已显示：置前聚焦（避免重复开窗，热键触达可见）
        if let Some(c) = &self.confirm {
            return window::gain_focus(c.id);
        }
        let message = format!("确定退出？将丢弃 {count} 个标签/便签");
        let (id, open) = window::open(window::Settings {
            position: window::Position::Centered,
            size: Size::new(CONFIRM_W, CONFIRM_H),
            resizable: false,
            ..window::Settings::default()
        });
        self.confirm = Some(ConfirmState { id, message });
        Task::batch([open.map(|_| Message::Noop), window::gain_focus(id)])
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
        // 已显示：置前聚焦（避免重复开窗，热键触达可见）
        if let Some(sw) = &self.settings {
            return window::gain_focus(sw.id);
        }
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
        Task::batch([open.map(|_| Message::Noop), window::gain_focus(id)])
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
            // 同目标且旧弹窗存活：复用并聚焦标题框 + 置前（不新建不销毁）。
            // 缺 gain_focus 时弹窗被其他窗口遮挡，用户点角标无任何视觉反馈。
            PopupPlan::Reuse(_) => {
                if let Some(p) = &self.popup {
                    Task::batch([
                        operation::focus(p.title_id.clone()),
                        window::gain_focus(p.id),
                    ])
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
        let title_id = Id::unique();
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
        Task::batch([open.map(Message::PopupOpened), window::gain_focus(id)])
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
                return panel_view(panel, self.dark);
            }
        }
        // 尚未创建的实际窗口：渲染空容器（iced 要求每窗口都有 view）
        container(text("")).into()
    }

    /// 主题（按当前明暗状态选取 iced 内建主题，经 `ApplyTheme` 热更新）
    pub fn theme(&self, _window: window::Id) -> Option<Theme> {
        if self.dark {
            Some(Theme::Dark)
        } else {
            Some(Theme::Light)
        }
    }

    /// 订阅：合并「主线程命令周期轮询」「窗口关闭事件」与「Esc/Enter 键盘」
    pub fn subscription(&self) -> Subscription<Message> {
        // 命令经稳定的 `time::every` tick 轮询 `cmd_rx`，避免依赖一次性的异步订阅流
        //（会被 iced Tracker 剪除而失效）。tick 也顺带保证零窗口时事件循环持续运转。
        let pump_sub =
            iced::time::every(std::time::Duration::from_millis(60)).map(|_| Message::Pump);
        // 键盘订阅：0.14 移除了 `keyboard::on_key_press`，改用 `event::listen_with`。
        // 它投递 **captured + ignored** 全部键盘事件（`keyboard::listen` 只报 ignored，
        // 会在 text_input 聚焦时漏掉按键），并附带 `window::Id`。
        // 注意：listen_with 的 f 必须是 **裸 fn 指针**（无捕获闭包无法类型检查），
        // 故用自由函数 [`keyboard_event`]。
        let key_sub = iced::event::listen_with(keyboard_event);
        Subscription::batch([
            pump_sub,
            window::close_events().map(Message::WindowClosed),
            key_sub,
        ])
    }
}

/// `event::listen_with` 的回调（0.14 要求**裸 fn 指针**，无捕获）。
///
/// 把键盘事件映射为高层消息：Esc → [`Message::EscapePressed`]、Enter → [`Message::EnterPressed`]。
/// 用 `event::listen_with`（而非 `keyboard::listen`）是因为后者只投递 `Ignored` 事件，
/// 会在 text_input 聚焦时漏掉被控件消费的按键；前者同时投递 Captured + Ignored。
///
/// Enter 过滤 `repeat`（长按自动重复不重复触发确认/保存），并**不**处理弹窗场景：
/// 弹窗标题/备注框的 Enter 由 `on_submit(PopupSavePressed)` 处理，若在此也响应
/// `EnterPressed` 会双触发（弹窗 Save + 全局 Enter），故弹窗分支不在此映射。
fn keyboard_event(
    event: iced::event::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::event::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key,
            modifiers,
            repeat,
            ..
        }) => match key {
            Key::Named(Named::Escape) => Some(Message::EscapePressed),
            Key::Named(Named::Enter) if !repeat => Some(Message::EnterPressed),
            Key::Named(Named::Tab) if !repeat => Some(Message::TabPressed {
                backwards: modifiers.shift(),
            }),
            _ => None,
        },
        _ => None,
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
            checkbox(sw.draft.show_badge_title)
                .label("角标显示标题")
                .on_toggle(Message::SettingsTitleToggled),
            checkbox(sw.draft.badge_always_top)
                .label("角标始终置顶")
                .on_toggle(Message::SettingsTopToggled),
            checkbox(sw.draft.show_balloon)
                .label("气泡提示")
                .on_toggle(Message::SettingsBalloonToggled),
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
///
/// 视觉对齐 HTML demo（Win11 暗色紧凑版）：卡片式行、hover 高亮、chevron、图标按钮、
/// 标题|窗口 合并 + CJK 双宽省略号。`dark` 决定调色板（面板无独立 dark，取应用主题）。
fn panel_view(panel: &PanelState, dark: bool) -> Element<'_, Message> {
    let palette = crate::ui::panel_style::panel_palette(dark);
    let search = text_input("搜索标题/备注/窗口/进程", &panel.search)
        .on_input(Message::PanelSearchChanged)
        .width(Length::Fill);

    let rows = filter_rows(&panel.rows, &panel.search);
    let list: Element<'static, Message> = if rows.is_empty() {
        column![text("无匹配标签")
            .size(14)
            .style(text_style(palette.subtle))]
        .padding(8)
        .into()
    } else {
        // keyed::Column 以 target 为 key 保行内状态（重排/刷新后避免串位）。
        // 显式注解 Key=isize，避免 fold 类型推断歧义。
        let col: iced::widget::keyed::Column<'static, isize, Message> =
            iced::widget::keyed::Column::new()
                .spacing(4)
                .width(Length::Fill);
        rows.iter()
            .fold(col, |col, row| {
                col.push(row.target, panel_row(panel, row, dark))
            })
            .into()
    };

    let content = column![
        search,
        row![
            button(text("全部展开")).on_press(Message::PanelExpandAll),
            button(text("全部收起")).on_press(Message::PanelCollapseAll),
        ]
        .spacing(8),
        // 列表级鼠标区：释放=提交拖拽；离开列表=取消拖拽（D28）
        mouse_area(scrollable(list).width(Length::Fill).height(Length::Fill))
            .on_release(Message::PanelDragDrop)
            .on_exit(Message::PanelDragCancel),
        row![button(text("退出")).on_press(Message::PanelExitPressed)],
    ]
    .spacing(12)
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill);

    content.into()
}

/// 次要文本（`text()`）样式辅助：设语义色（D28）
///
/// `iced::widget::text::Style` 仅含 `color` 字段，无 `..Default` 需要。
fn text_style(color: iced::Color) -> impl Fn(&Theme) -> iced::widget::text::Style + 'static {
    move |_theme| iced::widget::text::Style { color: Some(color) }
}

/// 单个标签行：`标题 | 窗口` 合并 + chevron 展开 + hover 高亮 + 展开区图标按钮
///
/// 文案克隆为自持 `'static` 字符串；行数据由调用方持有。`mouse_area` 实现 hover
/// 上报与单击置前；`dark` 取调色板。返回 `Element<'static, Message>`。
fn panel_row(panel: &PanelState, row: &TagRow, dark: bool) -> Element<'static, Message> {
    let palette = crate::ui::panel_style::panel_palette(dark);
    let expanded = panel.expanded.contains(&row.target);
    let hovered = panel.hovered == Some(row.target);

    // 判断标题是否处于行内编辑（D28：双击标题进入）
    let editing_title = panel
        .editing
        .as_ref()
        .is_some_and(|e| e.target == row.target && e.field == EditField::Title);

    // 标题|窗口名合并：demo 用一行标题 + 长路径截断；这里 title 截断，窗口名拼接
    let display_title = crate::ui::panel_style::truncate_units(&row.tag.title, 14);
    let header_title = format!("{display_title} | {}", row.tag.window_title);

    let chevron = text(if expanded {
        crate::ui::panel_style::CHEVRON_DOWN
    } else {
        crate::ui::panel_style::CHEVRON_RIGHT
    })
    .size(12)
    .style(text_style(palette.subtle));

    // 标题区：编辑态用 text_input（不包 mouse_area，避免鼠标区分发点击/双击中断输入），
    // 否则普通文本 + mouse_area（hover 上报 + 单击置前 + 双击进编辑）。
    let title_body: Element<'static, Message> = if editing_title {
        let draft = panel
            .editing
            .as_ref()
            .map(|e| e.draft.clone())
            .unwrap_or_default();
        text_input("标题", &draft)
            .id(format!("edit-{}", row.target))
            .on_input(Message::PanelEditInput)
            .on_submit(Message::PanelEditCommit)
            .width(Length::Fill)
            .into()
    } else {
        let dragging = panel.drag.is_some();
        // 拖拽手柄（⋮⋮）：按下即开始拖拽（D28）
        let handle = mouse_area(
            container(text(crate::ui::panel_style::DRAG_HANDLE).style(text_style(palette.subtle)))
                .padding(2),
        )
        .on_press(Message::PanelDragStart(row.target));
        // 行主体：hover 上报；非拖拽时单击置前+双击编辑；拖拽中进入本行上报预览位
        let main = mouse_area(
            row![
                container(chevron).padding(2),
                handle,
                text(header_title)
                    .size(13)
                    .width(Length::Fill)
                    .style(text_style(palette.text)),
            ]
            .spacing(2)
            .width(Length::Fill),
        )
        .on_enter(if dragging {
            Message::PanelDragHover(row.target)
        } else {
            Message::PanelRowEntered(row.target)
        })
        .on_exit(Message::PanelRowExited)
        .on_double_click(if dragging {
            Message::PanelDragStart(row.target)
        } else {
            Message::PanelBeginEdit {
                target: row.target,
                field: EditField::Title,
            }
        });
        let main = if dragging {
            main.on_press(Message::PanelDragHover(row.target))
        } else {
            main.on_press(Message::PanelRowActivated(row.target))
        };
        main.into()
    };

    // 卡片容器：hover 时用 hover 底色 + 边框，否则卡片底
    let card = container(title_body)
        .width(Length::Fill)
        .style(move |_theme| {
            let bg = if hovered { palette.hover } else { palette.card };
            iced::widget::container::Style {
                background: Some(iced::Background::Color(bg)),
                border: iced::Border::default()
                    .rounded(iced::border::Radius::from(6))
                    .color(palette.border)
                    .width(if hovered { 1 } else { 0 }),
                ..Default::default()
            }
        });

    if expanded {
        // 展开区：备注（可双击编辑）+ 图标按钮行（置前▲ / 编辑✎ / 移除🗑）
        // 备注编辑态（D28）：Note 字段在编辑时换 text_input
        let editing_note = panel
            .editing
            .as_ref()
            .is_some_and(|e| e.target == row.target && e.field == EditField::Note);
        let note: Element<'static, Message> = if editing_note {
            let draft = panel
                .editing
                .as_ref()
                .map(|e| e.draft.clone())
                .unwrap_or_default();
            text_input("备注", &draft)
                .id(format!("edit-note-{}", row.target))
                .on_input(Message::PanelEditInput)
                .on_submit(Message::PanelEditCommit)
                .width(Length::Fill)
                .into()
        } else {
            let txt = if row.tag.note.is_empty() {
                text("（无）").size(12).style(text_style(palette.subtle))
            } else {
                text(row.tag.note.clone())
                    .size(12)
                    .style(text_style(palette.subtle))
            };
            mouse_area(txt)
                .on_double_click(Message::PanelBeginEdit {
                    target: row.target,
                    field: EditField::Note,
                })
                .into()
        };
        let actions = row![
            button(text(crate::ui::panel_style::ICON_TOP))
                .style(icon_button_style(palette))
                .on_press(Message::PanelRowActivated(row.target)),
            button(text(crate::ui::panel_style::ICON_EDIT))
                .style(icon_button_style(palette))
                .on_press(Message::PanelRowEdit(row.target)),
            button(text(crate::ui::panel_style::ICON_DELETE))
                .style(icon_button_style(palette))
                .on_press(Message::PanelRowRemove(row.target)),
        ]
        .spacing(8);
        column![card, note, actions]
            .spacing(6)
            .padding(2)
            .width(Length::Fill)
            .into()
    } else {
        card.into()
    }
}

/// 图标按钮样式（紧凑：透明底 + 边框 + 语义色文本，D28）
///
/// iced 0.14 的按钮样式闭包签名为 `Fn(&Theme, button::Status) -> Style`（带 Status 态）。
fn icon_button_style(
    palette: crate::ui::panel_style::PanelPalette,
) -> impl Fn(&Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'static {
    move |_theme, _status| iced::widget::button::Style {
        background: Some(iced::Background::Color(iced::Color::TRANSPARENT)),
        text_color: palette.subtle,
        border: iced::Border::default()
            .rounded(iced::border::Radius::from(4))
            .color(palette.border)
            .width(1),
        ..Default::default()
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

/// 把行内编辑草稿合并到对应标签行（纯函数，可单测，D28）
///
/// 返回 `(更新后的 rows, 完整 Tag)`：把 `edit.target` 对应 tag 的指定字段
/// 替换为 `edit.draft`；若 target 不存在则原样返回（调用方已过滤，此处兜底）。
fn apply_edit_to_rows(rows: &[TagRow], edit: &PanelEdit) -> (Vec<TagRow>, Tag) {
    let mut new_rows = rows.to_vec();
    let mut committed: Option<Tag> = None;
    for row in new_rows.iter_mut() {
        if row.target == edit.target {
            match edit.field {
                EditField::Title => row.tag.title = edit.draft.clone(),
                EditField::Note => row.tag.note = edit.draft.clone(),
            }
            committed = Some(row.tag.clone());
            break;
        }
    }
    match committed {
        Some(tag) => (new_rows, tag),
        // target 不存在：返回原 rows 与第一行 tag（调用方已保证存在，此处兜底）
        None => {
            let tag = rows.first().map(|r| r.tag.clone()).unwrap_or_else(|| Tag {
                title: String::new(),
                note: String::new(),
                color: TagColor::Orange,
                window_title: String::new(),
                process_name: String::new(),
            });
            (new_rows, tag)
        }
    }
}

/// 计算拖放后的标签新顺序（纯函数，可单测，D28）
///
/// 把 `drag.from_target` 移到「`over_target` 之后」的位置（若 over_target 为 None
/// 则原序不动）。返回完整 target 序列（保持其余行相对顺序）。
fn preview_reorder(rows: &[TagRow], drag: &PanelDrag) -> Vec<isize> {
    let order: Vec<isize> = rows.iter().map(|r| r.target).collect();
    let Some(over) = drag.over_target else {
        return order.clone();
    };
    let from = drag.from_target;
    let Some(from_pos) = order.iter().position(|&t| t == from) else {
        return order.clone();
    };
    let Some(over_pos) = order.iter().position(|&t| t == over) else {
        return order.clone();
    };
    if from_pos == over_pos {
        return order.clone();
    }
    // 移除 from，再在 over_pos（调整后的索引）之后插入
    let mut new_order: Vec<isize> = order.iter().copied().filter(|&t| t != from).collect();
    let insert_at = new_order
        .iter()
        .position(|&t| t == over)
        .map(|pos| pos + 1)
        .unwrap_or(new_order.len());
    new_order.insert(insert_at, from);
    new_order
}

/// 按 target 顺序重排 rows（纯函数，可单测，D28）
///
/// 仅重新排序 `rows`，不动其内容；`order` 中缺失的 target 追加到尾部兜底。
fn reorder_rows(rows: &[TagRow], order: &[isize]) -> Vec<TagRow> {
    let mut result: Vec<TagRow> = Vec::with_capacity(rows.len());
    let mut by_target: std::collections::HashMap<isize, TagRow> =
        rows.iter().map(|r| (r.target, r.clone())).collect();
    for target in order {
        if let Some(row) = by_target.remove(target) {
            result.push(row);
        }
    }
    // order 缺失项追加尾部（保持原 relative 顺序）
    for row in rows {
        if by_target.contains_key(&row.target) {
            result.push(row.clone());
        }
    }
    result
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

    /// D28 apply_edit_to_rows：编辑 title 字段，全部行更新并带回完整 tag
    #[test]
    fn apply_edit_title_updates_row() {
        let rows = vec![row("旧标题", "备注", 1), row("另一", "备注2", 2)];
        let edit = PanelEdit {
            target: 1,
            field: EditField::Title,
            draft: "新标题".to_string(),
        };
        let (new_rows, tag) = apply_edit_to_rows(&rows, &edit);
        assert_eq!(new_rows[0].tag.title, "新标题");
        assert_eq!(new_rows[0].tag.note, "备注"); // 其他字段不变
        assert_eq!(tag.title, "新标题");
        assert_eq!(new_rows[1].tag.title, "另一"); // 其他行不变
    }

    /// D28 apply_edit_to_rows：编辑 note 字段
    #[test]
    fn apply_edit_note_updates_row() {
        let rows = vec![row("标题", "旧备注", 1)];
        let edit = PanelEdit {
            target: 1,
            field: EditField::Note,
            draft: "新备注".to_string(),
        };
        let (new_rows, tag) = apply_edit_to_rows(&rows, &edit);
        assert_eq!(new_rows[0].tag.note, "新备注");
        assert_eq!(tag.note, "新备注");
        assert_eq!(tag.title, "标题");
    }

    /// D28 apply_edit_to_rows：target 不存在时原样返回（兜底）
    #[test]
    fn apply_edit_unknown_target_falls_back() {
        let rows = vec![row("标题", "备注", 1)];
        let edit = PanelEdit {
            target: 99,
            field: EditField::Title,
            draft: "x".to_string(),
        };
        let (new_rows, tag) = apply_edit_to_rows(&rows, &edit);
        assert_eq!(new_rows[0].tag.title, "标题"); // 不被修改
        assert_eq!(tag.title, "标题");
    }

    // ---------- D28 preview_reorder / reorder_rows ----------

    /// preview_reorder：把 from 移到 over 之后（下行）
    #[test]
    fn preview_reorder_move_down() {
        let rows = vec![row("a", "", 1), row("b", "", 2), row("c", "", 3)];
        let drag = PanelDrag {
            from_target: 1,
            over_target: Some(2),
        };
        assert_eq!(preview_reorder(&rows, &drag), vec![2, 1, 3]);
    }

    /// preview_reorder：把 from 移到 over 之后（上移）且 over==from 时不变
    #[test]
    fn preview_reorder_move_up_and_same() {
        let rows = vec![row("a", "", 1), row("b", "", 2), row("c", "", 3)];
        // 1 移到 3 之后 -> [2,3,1]
        let drag = PanelDrag {
            from_target: 1,
            over_target: Some(3),
        };
        assert_eq!(preview_reorder(&rows, &drag), vec![2, 3, 1]);
        // over == from：原序
        let same = PanelDrag {
            from_target: 2,
            over_target: Some(2),
        };
        assert_eq!(preview_reorder(&rows, &same), vec![1, 2, 3]);
    }

    /// preview_reorder：over_target 为 None 或 from 不存在时原序不变
    #[test]
    fn preview_reorder_noop() {
        let rows = vec![row("a", "", 1), row("b", "", 2)];
        let none = PanelDrag {
            from_target: 1,
            over_target: None,
        };
        assert_eq!(preview_reorder(&rows, &none), vec![1, 2]);
        let unknown = PanelDrag {
            from_target: 99,
            over_target: Some(1),
        };
        assert_eq!(preview_reorder(&rows, &unknown), vec![1, 2]);
    }

    /// reorder_rows：按顺序重排 rows（内容不变）
    #[test]
    fn reorder_rows_applies_order() {
        let rows = vec![row("a", "", 1), row("b", "", 2), row("c", "", 3)];
        let reordered = reorder_rows(&rows, &[2, 3, 1]);
        assert_eq!(
            reordered.iter().map(|r| r.target).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
        assert_eq!(reordered[0].tag.title, "b");
    }

    /// reorder_rows：order 缺失的 target 追加到尾部
    #[test]
    fn reorder_rows_append_missing() {
        let rows = vec![row("a", "", 1), row("b", "", 2)];
        let reordered = reorder_rows(&rows, &[1]);
        assert_eq!(
            reordered.iter().map(|r| r.target).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}
