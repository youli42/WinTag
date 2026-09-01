# 架构设计

## 分层架构

```
┌──────────────────────────────────────────┐
│         UI Layer (ui/) — iced 线程        │
│  ┌──────────┬──────────┬───────────────┐ │
│  │  panel   │  popup   │ settings/confirm│
│  │ 概览面板  │ 标签弹窗  │ 设置/退出确认   │ │
│  │ (iced)   │ (iced)   │ (iced)        │ │
│  └──────────┴──────────┴───────────────┘ │
│  iced_proto.rs（IcedCommand / GuiEvent）  │
├──────────────────────────────────────────┤
│          Data Core Layer (core/)          │
│  ┌──────────┐  ┌────────────────────┐    │
│  │   tag    │  │      matcher       │    │
│  │ 数据结构  │  │  窗口句柄匹配       │    │
│  └──────────┘  └────────────────────┘    │
├──────────────────────────────────────────┤
│        System Service Layer (sys/)        │
│  ┌──────────┐  ┌────────────────────┐    │
│  │  window  │  │     overlay        │    │
│  │ 窗口监听  │  │  透明覆盖层绘制      │    │
│  │  win_event│  │     （角标/title）  │    │
│  └──────────┘  └────────────────────┘    │
│       windows-rs (Win32 API)             │
└──────────────────────────────────────────┘
```

说明：D27 起 `ui/` 的四个 GUI 窗口（panel/popup/settings/confirm）改由 **iced** 在独立线程承担；`sys/overlay`、`sys/tray`、`sys/win_event`、`hotkey` 与 `main.rs` 的 Win32 消息泵**保持纯 Win32 不变**（在外部窗口上打标的核心能力无法用 iced 表达）。

## 线程模型（D27）

```
主线程（不变）：GetMessageW 消息泵
 ├─ 隐藏窗口 WndProc（热键/覆盖层/WinEvent/退出流）
 ├─ sys::win_event / sys::overlay / hotkey / sys::tray（原生，仅与主线程对话）
 ├─ pump_background_events(hwnd)：单一阶段收拢 tray×2 + iced×1 的 try_recv
 └─ reapply_theme：原生覆盖层经 NativePrefs 重注入 + 发 IcedCommand::ApplyTheme

GUI 线程（新增 std::thread）：iced_app.run()
 ├─ 多窗口 window0=panel / window1=settings / window2=popup / window3=confirm
 ├─ subscription：from_main_rx.unfold() → IcedCommand
 ├─ update 产出 GuiEvent → 发送器回主线程
 └─ theme：ThemeMode+system_dark → iced Theme（继承现有橙强调色）
```

跨线程通信经 `src/ui/iced_proto.rs` 的纯协议层（`IcedCommand` 主→iced、`GuiEvent` iced→主）。**对外通道全项目仅 3 条且不可再减**：Win32 消息泵（原生覆盖层/热键/托盘必需）、tray-icon 两接收器（crate 强制）、iced 收发一对（跨线程必需）。**托盘不绕 iced、iced 不拥有 overlay/tray/hotkey**；后续窗口都复用同一对 `IcedCommand`/`GuiEvent`，不新增 channel。`TagStore`/`Settings` 以 `Arc<Mutex>` 共享。

### 原生层注入收敛（D27）

为消除 sys 层「改一条偏好要同步改 N 个 setter」的漂移，原生层注入由 7 个 `set_*` 收敛为两处、各注入一次：

- **`NativePrefs`**（纯值结构）：`show_title` / `badge_always_top` / `tooltip_theme` / `balloon_enabled`，主线程于启动与 `reapply_theme` 时一次性写入；替代 `set_show_title`/`set_badge_always_top`/`set_tooltip_theme`/`set_balloon_enabled`。
- **`NativeBridge`**（回传能力）：封装覆盖层「上报 edit_tag / 激活窗口」的事件出口，替代 `set_message_target`/`set_tag_store` 的单向注入；由主线程持有、按需向 iced 线程转发。

## 依赖方向

```
ui → core → sys
```

禁止反向依赖。`sys` 层不感知 `core` 和 `ui`，`core` 层不感知 `ui`。`ui/iced_proto.rs` 为叶子纯协议层（无 iced/Win32 依赖），供主线程与 iced 线程共用。

## 模块职责

### sys/ — 系统服务层

负责与 Windows 操作系统交互，封装所有 Win32 API 调用。

#### window.rs
- 使用 `SetWinEventHook` 监听窗口事件
  - `EVENT_SYSTEM_FOREGROUND`：活动窗口切换
  - `EVENT_OBJECT_LOCATIONCHANGE`：窗口位置/大小变化
  - `EVENT_OBJECT_DESTROY`：窗口关闭
- 获取窗口属性：标题 (`GetWindowText`)、进程名、进程ID、窗口句柄
- 生成窗口唯一标识符 (WindowId)

#### overlay.rs
- 创建透明覆盖窗口 (`WS_EX_LAYERED` + `WS_EX_TRANSPARENT` + `WS_EX_TOPMOST`)
- 使用 `UpdateLayeredWindow` 绘制标记内容
- 同步覆盖层位置与目标窗口 (`SetWindowPos`)
- 处理高 DPI 缩放 (`GetDpiForWindow`)
- 管理覆盖层生命周期（创建/销毁/隐藏）

#### tray.rs（D24 落地，D26 底层迁至 tray-icon）
- 系统托盘图标：`TrayIconBuilder` + `Menu(MenuItem)`（tray-icon/tauri），图标取自嵌入资源 `assets/icon.ico`（winresource ID=1）
- 事件经 `TrayIconEvent`/`MenuEvent` crossbeam channel 由主线程 `try_recv` 轮询分发；左键单击打开概览面板、右键四项菜单（打开概览面板/设置页/快速标记/退出）
- 启动气泡经 `notify-rust`（Windows TOAST），开关 `show_balloon` 由主线程注入
- `TrayCommand` 纯逻辑层（`icon_event_to_command`/`menu_id_to_command`，零 Win32）供单测；main 接 ui，保持 `ui→core→sys` 依赖方向

### core/ — 数据核心层

负责业务逻辑与数据管理，不涉及 UI 渲染。所有数据仅存在于内存中，无持久化。

#### tag.rs
- 定义核心数据结构：
  ```rust
  struct Tag {
      hwnd: HWND,           // 窗口句柄，唯一标识
      title: String,        // 必填，快速索引
      note: String,         // 选填，长文本描述
      color: Color,         // 标记颜色
      group: String,        // 分组，空串=未分组（规划中，D23，纯会话）
  }
  ```

#### hotkey_config.rs（规划中，D23）
- 自定义快捷键数据模型（纯 serde）：`HotkeyAction`/`HotkeyBinding{modifiers,vk}`/`HotkeyMap`
- 绑定"快捷键 ↔ 功能"，`Default` = 现硬编码值（Ctrl+Shift+N/M/S），持久化到配置（UI 偏好）

#### matcher.rs
- 以窗口句柄（HWND）为键的标签查找
- 窗口句柄失效时自动清理对应标签
- 简洁的 HashMap 存储，无状态外溢

### ui/ — 用户界面层（D27：四窗迁至 iced，独立线程）

自 `D27` 起，四个 GUI 窗口改由 **iced**（`tiny-skia` 软件渲染器）在独立线程渲染，不再手写 Win32 窗口类 / `WM_CTLCOLOR*` / `WM_DRAWITEM` / `BS_OWNERDRAW` 自绘 / 子类化。`ui/button.rs`（自绘圆角按钮）与 `ui/layout.rs`（DPI 缩放）整体删除（iced 自管 DPI 与主题）。

#### iced_proto.rs（纯协议层，无 iced/Win32 依赖）
- `IcedCommand`（主线程 → iced 线程）：`ShowPanel` / `HidePanel` / `OpenSettings` / `EditTag{target, position, title, note, color}` / `RefreshTags` / `ApplyTheme{dark, accent}` / `ShowConfirm{count}` / `CloseConfirm`
- `GuiEvent`（iced 线程 → 主线程）：`TagSaved{target, tag}` / `SettingsChanged` / `EditTagRequested` / `ActivateWindow` / `RemoveTag` / `ExpandAll` / `CollapseAll` / `ConfirmExit` / `CancelExit` / `PanelVisibilityChanged`

#### iced_app.rs
- `iced::Application` 实现 + 多窗口管理（panel / settings / popup / confirm）
- `subscription` 经 `from_main_rx.unfold()` 消费 `IcedCommand`，`update` 产出 `GuiEvent` 回主线程

#### panel.rs（iced）
- 概览面板：搜索框 + 标签列表（点击/回车置前目标窗口、展开显示完整备注）+ 全部展开/收起 + 退出按钮 + 右键 置前/编辑/移除

#### popup.rs（iced）
- 标签编辑弹窗：标题/备注输入、5 颜色块、确认/取消；`window::Position::Specific(position)` 光标附近定位 + 预填聚焦标题

#### settings.rs（iced）
- 设置页：主题/圆角下拉框 + 角标显示标题/始终置顶/气泡提示复选框 + 保存/取消；保存发 `GuiEvent::SettingsChanged`

#### confirm.rs（iced）
- 退出确认弹窗：消息文本 + 退出/取消按钮；回车确认、Esc 取消、Tab 循环焦点；确认发 `GuiEvent::ConfirmExit`

#### charts.rs（规划中，D23）
- 统计图表独立窗口（柱状图），纯函数 `bar_layout()` 可单测；默认假设：按分组计数柱状图（与 R19 分组契合，待确认形态）

### hotkey.rs
- 注册全局热键 (`RegisterHotKey`)
- 三个热键：`Ctrl+Shift+M` 打开概览面板，`Ctrl+Shift+N` 快速标记当前窗口，`Ctrl+Shift+S` 打开设置（规划中支持自定义，D23）
- 处理热键消息，触发对应操作

## 数据流

### 标记窗口流程
```
1. 用户按下热键（Ctrl+Shift+N）→ 触发快速标记操作
2. sys/window 获取当前活动窗口 HWND 和标题
3. core/matcher 检查是否已有标签
4. ui/popup 显示编辑界面（预填标题，可修改）
5. 用户确认 → core/tag 存入内存 HashMap
6. sys/overlay 在目标窗口上创建覆盖层
```

### 窗口切换流程
```
1. sys/window 检测到 EVENT_SYSTEM_FOREGROUND
2. core/matcher 查找新窗口 HWND 的标签
3. 如果有标签 → sys/overlay 显示标记并短暂闪烁
4. 如果无标签 → 隐藏覆盖层
```

### 窗口关闭流程
```
1. sys/window 检测到 EVENT_OBJECT_DESTROY
2. core/matcher 移除对应 HWND 的标签
3. sys/overlay 销毁覆盖层
```
（数据随窗口关闭自动清除，无持久化步骤）

## 状态管理

使用消息传递模式（channel）在各模块间通信：

```rust
enum AppEvent {
    WindowActivated(WindowInfo),
    WindowClosed(WindowHandle),
    WindowMoved(WindowHandle, Rect),
    TagCreated(Tag),
    TagUpdated(Tag),
    TagDeleted(u64),
    HotkeyPressed(Hotkey),
}
```

主线程运行事件循环，各模块通过 `mpsc::Sender<AppEvent>` 发送事件。