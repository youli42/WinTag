# iced 迁移执行方案（D27）

> 关联：[决策记录 D27](./decision-records.md)、[架构设计](./architecture.md)。本文件是**执行**层面的可维护方案：里程碑、逐阶段任务、文件级改动、验证门禁与可维护性约定。纯文档 + 代码改动指向，按阶段推进、每阶段可独立验收、可随时回退。

## 0. 目标与边界

**目标**：把 `ui/panel` / `ui/popup` / `ui/settings` / `ui/confirm` 四个手写 Win32 窗口迁至 iced（`tiny-skia` 软件渲染器，独立线程 + 通道通信），让图形界面层样板代码（窗口注册 / `WM_CTLCOLOR*` / `WM_DRAWITEM` / `BS_OWNERDRAW` / 子类化 / DPI 缩放）归零，四窗行为语义不丢失。

**非目标（明确不动）**：`sys/overlay`（透明覆盖层）、`sys/tray`、`sys/win_event`、`sys/window`、`hotkey`、`main.rs` 的 Win32 消息泵与退出流——全部保持纯 Win32。

**总线最小化铁律**（贯穿全程，评审每阶段都核对）：
1. 对外通道全项目仅 3 条：Win32 消息泵、tray-icon 两接收器、iced 收发一对。**禁止新增通道**。
2. **托盘不绕 iced**：托盘只与主线程对话；最新窗口一律复用同一对 `IcedCommand`/`GuiEvent`。
3. iced **不拥有** overlay / tray / hotkey。
4. 原生层注入收敛为 `NativePrefs`（纯值）与 `NativeBridge`（回传能力）两处，各注入一次；维持 `ui → core → sys` 依赖方向。
5. 同一消息只有一个出口：原生 `WM_APP_*` 只负责「进主循环」，出主循环一律走 iced 通道，不双写。

## 1. 关键类型与协议（先定契约，后写实现）

每个阶段都以此契约为蓝本，**先改 `iced_proto.rs` 再写功能**，避免四处漂移。

### 主线程 → iced（`IcedCommand`）

| 变体 | 用途 | 携带 |
|---|---|---|
| `ShowPanel` / `HidePanel` | 概览面板显隐 | — |
| `OpenSettings` | 打开设置页 | — |
| `EditTag` | 打开标签编辑弹窗 | `target: isize`、`position: (i32,i32)`、`title`、`note`、`color: TagColor` |
| `RefreshTags` | 刷新面板列表 | `Vec<TagRow>` |
| `ApplyTheme` | 应用主题 | `dark: bool`、`accent: u32` |
| `ShowConfirm` | 退出确认 | `count: usize` |
| `CloseConfirm` | 关闭确认 | — |

### iced → 主线程（`GuiEvent`）

| 变体 | 用途 | 携带 |
|---|---|---|
| `TagSaved` | 保存标签 | `target: isize`、`tag: Tag` |
| `SettingsChanged` | 保存设置 | `Settings` |
| `EditTagRequested` | 面板请求编辑 | `target: isize` |
| `ActivateWindow` | 面板点击置前 | `target: isize` |
| `RemoveTag` | 面板移除标签 | `target: isize` |
| `ExpandAll` / `CollapseAll` | 展开/收起（供主线程留快照） | — |
| `ConfirmExit` / `CancelExit` | 确认/取消退出 | — |
| `PanelVisibilityChanged` | 面板显隐上报 | `bool` |
| `ReorderTags` | 面板拖拽排序回传（D28） | `targets: Vec<isize>`（完整新顺序） |

### 原生层注入（收敛后）

- `NativePrefs { show_title: bool, badge_always_top: bool, tooltip_theme: (COLORREF, COLORREF), balloon_enabled: bool }` — 纯值，启动 + `reapply_theme` 各写一次。
- `NativeBridge` — 覆盖层事件出口（edit_tag / activate），主线程持有。

## 2. 可维护性约定（开发者在新增窗口/消息时必须遵守）

1. **新增窗口**：在 `iced_app.rs` 注册一个 `window::Id` + `view` + 一条打开/关闭逻辑；**不新建通道**，复用 `IcedCommand`/`GuiEvent`（每个窗口一个变体）。
2. **新增消息**：先在 `iced_proto.rs` 加变体（纯 Rust，含 `#[derive(Debug, Clone)]` 与字段注释），再在主线程 `pump_background_events` 的单张分发表里加一行；**不经全局 static 传递**。
3. **单张分发表**：主线程所有来自通道的分发收敛到 `pump_background_events`（或其中 `match`），不用散落各处的 `try_recv` 块。
4. **依赖方向**：`ui → core → sys`；`iced_proto.rs` 为叶子（无 iced/Win32 依赖）；sys 不 `use crate::ui`。
5. **线程安全**：跨线程只传 `Clone + Send` 类型（`String`/数值/`Tag`/`Settings`/`TagColor`）；`HWND` 等半句柄一律传 `isize`，在主线程还原。
6. **每阶段必跑**：`cargo build` → `cargo clippy -- -D warnings` → `cargo fmt -- --check` → `cargo test`，全绿才算完成。

## 3. 分阶段执行计划

> 每个阶段：**目标 / 任务 / 文件改动 / 验收门禁 / 回退点**。阶段之间可独立提交、独立回退（回退 = 恢复该阶段涉及文件的上一份 git 状态，不改动已完成的前阶段）。

---

### 阶段 G0：骨架与走廊打通（最小闭环，先跑通 confirm）

**目标**：加依赖、建线程 + 通道 + 协议层，用最小无状态窗口（confirm）验证「线程 + 主题 + 通道」闭环。此阶段完成后架构骨架成立，后续全是纯增量。

**任务**
- [ ] `Cargo.toml`：加 `iced`（`tiny-skia` 渲染器，版本按 MSRV/稳定版定稿）、`crossbeam-channel`（显式）。
- [ ] 新建 `src/ui/iced_proto.rs`（协议契约，如上节；本阶段先含 `ShowConfirm`/`CloseConfirm`/`ConfirmExit`/`CancelExit`）。
- [ ] 新建 `src/ui/iced_app.rs`：`iced::Application`；`new` 读标志参数（`crossbeam::Sender<GuiEvent>`、`Receiver<IcedCommand>`）；`subscription` 经 `from_main_rx.unfold();` 消费；`update` 处理 `ShowConfirm/CloseConfirm` 与窗口生命周期；`view` 渲染 confirm；`theme` 用自定义调色板。
- [ ] `src/ui/mod.rs` 注册 `iced_proto` / `iced_app`。
- [ ] `main.rs`：`spawn` iced 线程（`std::thread::Builder::new().name("wintag-gui")`）；新增 `pump_background_events(hwnd)`（本阶段仅 iced 一路 + 原 tray 两路搬入）；`request_exit` 的 `create_confirm` → `tx_iced.send(ShowConfirm{count})`；`GuiEvent::ConfirmExit` → 既有 `WM_APP_EXIT(wParam=1)` 退出流。
- [ ] `ui/confirm.rs`：Win32 实现**保留**至阶段 G4（此阶段新 confirm 与旧 confirm 并存，仅作对照）。

**验收门禁**
- [ ] `cargo run`：有标签时托盘/面板「退出」弹出 iced 确认窗；回车=确认退出、Esc=取消；主题随系统深浅色正确。
- [ ] 覆盖层/热键/托盘行为与迁移前一致（回归）。
- [ ] `cargo build` / `clippy -D warnings` / `fmt --check` / `test` 全绿。
- [ ] 无新增通道（对上述铁律核对）。

**回退点**：恢复 `Cargo.toml` + `main.rs` + 删除 `iced_proto.rs`/`iced_app.rs`。

---

### 阶段 G1：原生层注入收敛（NativePrefs / NativeBridge）

**目标**：先清理「总线坏味」再扩展窗口，避免后续每做一窗都要动 7 个设置器。

**任务**
- [ ] 新 `src/sys/native_prefs.rs`（或并入 `sys/mod.rs`）：`NativePrefs` 纯值 + `set_native_prefs`/`native_prefs`（`OnceLock<Mutex<NativePrefs>>`）。
- [ ] `NativeBridge`：封装 overlay 的 `edit_tag` / `activate` 上报，主线程持有 `Sender<IcedCommand>` 转发。
- [ ] `sys/overlay.rs`：删 `set_show_title`/`set_badge_always_top`/`set_tooltip_theme`/`set_message_target`，改读 `native_prefs()` + bridge；`sys/tray.rs` 删 `set_balloon_enabled`，改读 `native_prefs()`。
- [ ] `main.rs`：`reapply_theme` 与启动段改为一次 `set_native_prefs(NativePrefs{...})`；`set_tag_store` 保留（数据本体）或并入 bridge。
- [ ] 收敛纯函数：`apply_native_prefs(cfg, system_dark) -> NativePrefs` 可单测。
- [ ] 单测：`NativePrefs` 默认值、`apply_native_prefs` 主题映射。

**验收门禁**
- [ ] 设置页改「角标显示标题/始终置顶/气泡提示/主题」后，覆盖层 tooltip 与 z 序即时跟随（行为与迁移前一致）。
- [ ] 无 `set_show_title`/`set_balloon_enabled` 等散落设置器（grep 校验）。
- [ ] 全绿（build/clippy/fmt/test）。

**回退点**：恢复 `sys/overlay.rs`/`sys/tray.rs` 的设置器形态；删除 `native_prefs.rs`。

---

### 阶段 G2：迁移 settings（设置页）

**目标**：把 5 个设置项迁为 iced 控件，保存经 `SettingsChanged` 回流复用既有 `reapply_theme`。

**任务**
- [ ] `iced_proto.rs` 加 `OpenSettings`/`SettingsChanged`。
- [ ] `iced_app.rs`：注册 `window::Id`（settings），`view` 用 `ComboBox`（theme/corner）+ `Checkbox`（show_badge_title / badge_always_top / show_balloon）+ `Button`（保存/取消）；`update` 收 `OpenSettings` 打开窗口、`SettingsChanged` 发出。
- [ ] `ui/settings.rs`：Win32 实现标记删（G4 统一删）；本阶段 `settings_hwnd()`/`toggle_settings` 暂保留供旧路径对照，主线程 `分发` 改发 `OpenSettings`。
- [ ] `main.rs`：删 `ensure_settings_window`/`settings_hwnd` 调用点，改 `tx_iced.send(OpenSettings)`；`SettingsChanged` → 写 `Arc<Mutex<Settings>>` + `save()` + `reapply_theme`（复用现有 `save_and_hide` 的保存段为纯函数）。
- [ ] `ui/theme.rs`：把 `resolve_colors`/`light_colors`/`dark_colors`/`blend` 留作覆盖层注入 + iced 主题映射源；DWM `apply_*` 保留。

**验收门禁**
- [ ] 设置页 5 项可改、保存后立即生效（覆盖层 + 四窗主题同步）、写入 `config.toml`、取消不改动。
- [ ] 热键 / 托盘「打开设置」入口正常（`WM_APP_OPEN_SETTINGS` → iced 窗口）。
- [ ] 全绿。

**回退点**：恢复 `ui/settings.rs` 原实现 + 主线程 `ensure_settings_window` 路径；移除 iced settings 窗口。

---

### 阶段 G3：迁移 popup（标签编辑弹窗）

**目标**：单例复用 + 光标定位 + 预填聚焦 + 颜色块，保存回流写标签并创建覆盖层。

**任务**
- [ ] `iced_proto.rs`：复用 `EditTag`（携带 `target/position/title/note/color`）、`TagSaved`。
- [ ] `iced_app.rs`：`EditTag` → 打开 popup 窗口（`window::Position::Specific(position)`、定尺寸）+ `text_input::Id` + `Command::focus` 聚焦标题；颜色块用 `Row` 内 `Button`（选中加描边）；确认/取消按钮。
- [ ] 单例复用移入 iced 侧：重复 `EditTag` 同目标置前、异目标替换（把 `plan_popup_action` 纯函数从 `ui/popup.rs` 移到 `iced_proto.rs` 或 `iced_app.rs` 复用）。
- [ ] `main.rs`：`handle_quick_tag`/`WM_APP_EDIT_TAG`/托盘 `QuickTag` → 主线程算好 `clamp_to_work` 位置（保留 `ui/popup.rs` 的 `clamp_to_work` 纯函数）后 `tx_iced.send(EditTag{...})`；`TagSaved` → `matcher::upsert_tag` + `PostMessage(WM_CREATE_OVERLAY)` + `PostMessage(WM_APP_TAGS_CHANGED)`（或直接发 `RefreshTags`）。

**验收门禁**
- [ ] 热键快速标记、角标/标题条单击编辑、面板右键编辑三入口均弹 iced 弹窗；光标附近定位、越界钳制；标题预填、颜色块选中记忆；Tab 循环 / Esc 取消 / 回车保存。
- [ ] 保存后覆盖层即时出现/更新；`TagStore` 正确 upsert。
- [ ] 全绿。

**回退点**：恢复 `handle_quick_tag`/`WM_APP_EDIT_TAG` → `ui::popup::create_popup`。

---

### 阶段 G4：迁移 panel 列表 + 清理（收尾）

**目标**：迁移概览面板，删旧 Win32 实现与 `ui/button.rs`/`ui/layout.rs`，完成回归与文档同步。

**任务**
- [ ] `iced_proto.rs`：`RefreshTags`/`PanelVisibilityChanged`/`ActivateWindow`/`RemoveTag`/`EditTagRequested`/`ExpandAll`/`CollapseAll`。
- [ ] 面板：`TextInput`(搜索) + `Scrollable`(`Column` 标签行；根项点击/回车 → `ActivateWindow`，展开显示备注；行内「…」右键 → 置前/编辑/移除）+ 全部展开/收起 + 底部「退出」。搜索过滤为纯函数可单测。
- [ ] `main.rs`：删 `PANEL_HWND`/`toggle_panel` 句柄路径，改「面板可见状态 + `ShowPanel`/`HidePanel`」；`WM_APP_TAGS_CHANGED`/`RefreshTags` → 主线程持标签快照发 `RefreshTags`（仅可见时）；`ActivateWindow` → `SetForegroundWindow`+`SetWindowPos`；`RemoveTag` → 删存储 + 销毁覆盖层；托盘/热键 `TogglePanel` → `ShowPanel`。
- [ ] **删除** `src/ui/button.rs`、`src/ui/layout.rs`；删 `ui/panel.rs`/`ui/popup.rs`/`ui/settings.rs`/`ui/confirm.rs` 的 Win32 实现；`ui/theme.rs` 拆分定稿。
- [ ] 覆盖层/隐藏窗口相关的 `set_*` 复查收敛（G1 已做，此处再核对一遍）。

**验收门禁**
- [ ] 面板：搜索过滤、点击/回车置前、展开显示备注、全部展开/收起、右键置前/编辑/移除、Esc 关闭、Tab 循环、底部退出、最小尺寸 300×360。
- [ ] 三个入口（热键 / 托盘左键/菜单 / 气泡）打开面板；`--no-tray` 下面板退出按钮可用。
- [ ] `cargo run` 全流程回归 + `cargo test` 全绿。
- [ ] 旧 Win32 UI 文件清零（grep 无 `WNDCLASSW` / `CreateWindowExW` 于 `ui/`），托盘/覆盖层/热键仍在 `sys/` + `main.rs`。

**回退点**：恢复 `ui/*.rs` + 主线程 `PANEL_HWND` 路径；保留 G0-G3 成果。

---

### 阶段 G5：回归加固与文档定稿

- [ ] `tests/smoke.rs` 补 iced 侧纯函数单测：`IcedCommand`/`GuiEvent` 序列化（如需要）、搜索过滤、`clamp_to_work`、`NativePrefs` 映射、`plan_popup_action` 复用。
- [ ] 手动验收单四窗键盘语义逐项核对（Tab/Esc/Enter/颜色块/树展开）。
- [ ] `AGENTS.md` 同步 `ui/` 结构（`iced_*` 模块、删除 `button`/`layout`、`theme` 拆分）；架构图/线程模型与 `iced-migration.md` 对齐。
- [ ] `cargo build`/`clippy -D warnings`/`fmt --check`/`test` 全绿。

## 4. 风险与回退一览

| 阶段 | 主要风险 | 对策 / 回退 |
| :--- | :--- | :--- |
| G0 | iced/winit 与 `windows 0.58`/tray-icon 版本共存冲突 | `cargo build` 先行验证；冲突则先升 `windows` 或锁 iced 版本（D26 已并存 `windows-sys 0.61`，无运行时冲突） |
| G0 | winit 在独立线程创建窗口异常 | 复核 `iced`/`winit` Windows 线程模型；必要时仅在 G0 单独脚本验证窗口创建 |
| G1 | 收敛设置器时遗漏某路径（如启动与 `reapply_theme` 双写） | `apply_native_prefs` 纯函数 + 单测；`reapply_theme` 与启动段同一调用点 |
| G2-G4 | 键盘/自绘语义重写回归 | 每阶段独立验收 + 手动核对清单；单测覆盖纯函数 |
| G4 | `WM_APP_TAGS_CHANGED` 快照与 iced 状态不同步 | 主线程为唯一权威 `TagStore`，iced 只收 `RefreshTags`；保存一律经主线程 |
| 任意 | 线程误触 Win32 半句柄 | 跨线程只传 `isize`，主线程还原；代码评审核对 |

## 5. 每个阶段的「完成」定义

一个阶段完成 = 该阶段的**验收门禁全部勾选** + `build/clippy/fmt/test` 全绿 + 无新增通道 + 铁律 1-5 复核通过 + 有明确的回退点。**不做**跨阶段的半成品（如 G2 未验收就先动 G3）。
