# AGENTS.md — WinTag 项目开发约定

## 项目概述

WinTag 是 Windows 平台的外挂式窗口语义增强工具，使用 Rust 开发。通过 Win32 API 监听窗口事件并在目标窗口上绘制透明覆盖层来显示用户自定义标签和便签。标签与便签数据仅存在于当前会话，程序退出即清除；UI 偏好（主题/圆角）持久化到 `%APPDATA%\WinTag\config.toml`（D9 授权打破纯会话原则，见 [doc/decision-records.md D10](./doc/decision-records.md)）。

## 构建与检查命令

```pwsh
# 编译
cargo build

# 编译（Release）
cargo build --release

# 运行
cargo run

# 测试
cargo test

# 代码检查
cargo clippy -- -D warnings

# 格式化
cargo fmt -- --check

# 格式化并修复
cargo fmt
```

## 项目结构约定

```
src/
├── lib.rs           # 库入口，公开模块
├── main.rs          # 入口，Windows 子系统 + 单实例保护 + 托盘常驻，消息循环、热键分发、覆盖层管理；D27 起启动 wintag-gui 线程（iced），pump_background_events 收拢 tray×2 + iced×1 的 try_recv；request_exit 改发 IcedCommand::ShowConfirm
├── build.rs         # 构建脚本：嵌入 comctl32 v6 视觉样式 manifest（D11）；D26 起经 winresource 嵌入 assets/icon.ico（ID=1）
├── common/          # 系统级共享工具层（叶子模块，D7）：宽字符串转换、窗口用户数据读写、自定义消息常量
│   └── mod.rs       # 自定义消息：WM_APP+1..7 已占（OVERLAY/DESTROY/WINEVENT/OPEN_SETTINGS/THEME_CHANGED/TAGS_CHANGED/EDIT_TAG）；D24 增 WM_APP_EXIT=WM_APP+9；D26 删 WM_APP_TRAY（托盘事件改走 tray-icon channel）
├── sys/             # 底层系统服务层 — Win32 API 调用
│   ├── mod.rs
│   ├── native_prefs.rs  # 原生层偏好注入收敛（D27 G1）：NativePrefs 纯值（show_title/badge_always_top/tooltip_theme/balloon_enabled）+ set_native_prefs/native_prefs（OnceLock<Mutex>，可热更新）
│   ├── window.rs    # 窗口检测、句柄捕获、事件监听
│   ├── badge.rs     # 角标/标题条软件光栅渲染纯函数（SDF 圆边三角形 + 圆角矩形 + 标题截断，D11/D12）
│   ├── overlay.rs   # 透明覆盖层绘制与同步（UpdateLayeredWindow 逐像素 alpha，BGRA 字节序，D17；D12 起含可选圆角标题条，开关经 native_prefs 注入；D14 首绘异步化 + 每次同步重申置顶；D16 单击经 WM_APP_EDIT_TAG 请求编辑；D18 tooltip 分字体 DT_CALCRECT 量测、DrawTextW 返回值推进备注行；D19 置顶可选化——badge_always_top 关闭时 insert-after 目标窗口跟随其 z 序；D20 修正——SetWindowPos 的 hWndInsertAfter 会把窗口排到其背后，须插到目标前邻 GW_HWNDPREV + 对齐 z 带，tooltip 同逻辑；D27 G1 改读 native_prefs）
│   └── tray.rs      # 系统托盘（D26 迁至 tray-icon(tauri)）：TrayIconBuilder + Menu(MenuItem) + 嵌入资源图标；事件经 TrayIconEvent/MenuEvent crossbeam channel 由主循环 try_recv 轮询；左键单击=打开概览面板、右键四项菜单、菜单 id 解码；TrayCommand 纯逻辑层 + icon_event_to_command/menu_id_to_command 纯映射（零 Win32，单测）；show_balloon 改 notify-rust(Windows TOAST)+should_show_balloon+balloon_enabled（D27 G1 读 native_prefs）；图标取自 assets/icon.ico（winresource ID=1，Icon::from_resource）；main 接 ui，保持 ui→core→sys；托盘须与消息泵同线程建（主线程 GetMessageW 满足），事件无 WM_APP_TRAY
├── core/            # 核心数据管理层
│   ├── mod.rs
│   ├── tag.rs       # 标签数据结构定义（内存中，无持久化；规划中加 group 字段，D23）
│   ├── matcher.rs   # 窗口句柄匹配逻辑
│   ├── settings.rs  # 配置数据模型与 TOML 持久化（%APPDATA%\WinTag\config.toml；含 show_badge_title，R6；badge_always_top，R19/D19；show_balloon，D24；规划中加 hotkeys/panel_dock，D22/D23）
│   └── hotkey_config.rs  # （规划中，D23）自定义快捷键数据模型（纯 serde，HotkeyMap）
├── ui/              # 用户界面层（D27：四窗迁至 iced）
│   ├── mod.rs
│   ├── iced_proto.rs  # 纯协议层（叶子，无 iced/Win32 依赖）：IcedCommand（主→iced：ShowConfirm/CloseConfirm/OpenSettings/ApplyTheme/EditTag/ShowPanel/HidePanel/RefreshTags）与 GuiEvent（iced→主：ConfirmExit/CancelExit/SettingsChanged/TagSaved/PanelVisibilityChanged/ActivateWindow/EditTagRequested/RemoveTag/PanelExit）；TagRow；plan_popup_action（纯函数，单测）
│   ├── iced_app.rs  # iced::daemon 多窗口应用（独立线程 wintag-gui）：confirm/settings/popup/panel 四窗；crossbeam(同步)→futures(异步) 桥接订阅 + window::close_events + Esc 键；每窗 state + 纯函数视图（filter_rows 单测）
│   ├── geo.rs       # 主线程窗口定位辅助（D27 G4，自 layout/popup 移入）：dp/scale_px（DPI 缩放）+ clamp_to_work（工作区钳制）+ POPUP_LOGICAL_W/H
│   └── theme.rs     # 暗色主题与圆角 + 全局字体 + 扩展调色板（DWM + WM_CTLCOLOR + lfMessageFont，D11；统一主题管理器 sync_window_theme/apply_control_theme + 下拉框 DarkMode_Explorer 变体，D17；D27 起纯调色板 light/dark_colors/blend 供覆盖层注入 + iced 主题映射，DWM apply_* 保留给原生覆盖层）
└── hotkey.rs        # 全局热键注册（规划中支持自定义快捷键，D23）
tests/
└── smoke.rs         # 冒烟测试（30 个用例）
```

> D27 起 `ui/` 不再含手写 Win32 窗口类（panel/popup/settings/confirm/button/layout 已删除）；四窗全部由
> `iced_app.rs` 在独立线程承担，主线程 Win32 消息泵 + `sys/overlay` + `sys/tray` + `hotkey` 保持纯 Win32 不变。

## 编码规范

- 遵循 Rust 官方命名规范：`snake_case` 变量/函数，`CamelCase` 类型/结构体
- 所有 `unsafe` 块必须添加 `// SAFETY:` 注释说明安全性
- 使用 `anyhow` 或 `thiserror` 进行错误处理，禁止裸 `unwrap()` 在非测试代码中
- 模块间的依赖方向：`ui → core → sys`，禁止反向依赖
- 所有公开 API 必须添加文档注释 (`///`)

## 架构要点

### 消息循环
- 主线程创建隐藏窗口（`WinTagHiddenWnd`）用于接收热键和覆盖层管理消息
- 热键通过 `RegisterHotKey` 挂载到隐藏窗口
- 覆盖层创建/销毁通过 `PostMessage(WM_CREATE_OVERLAY / WM_DESTROY_OVERLAY)` 发送到隐藏窗口
- `GetMessageW(None, ...)` 处理本线程所有窗口消息（包括覆盖层的 `WM_PAINT`）
- 主线程消息泵返回后经 `pump_background_events(hwnd, gui_rx)` 非阻塞 `try_recv` 排空 tray×2 + iced×1 三通道并分发
- 全局状态通过 `OnceLock<Arc<Mutex<...>>>` 在 WndProc 和主循环间共享

### UI 层（iced 独立线程，D27）
- 四个 GUI 窗口（confirm/settings/popup/panel）由 `ui::iced_app`（`iced::daemon` 多窗口）在独立线程 `wintag-gui` 承担，`Application::run` 阻塞该线程，主线程 Win32 消息泵不受影响
- 主线程 ↔ iced 线程经一对 crossbeam 通道双向通信：主线程发 `ui::iced_proto::IcedCommand`、回 `GuiEvent`；内桥接线程把同步 crossbeam 转成 iced 异步订阅流
- 总线最小化铁律（见 `doc/iced-migration.md`）：对外通道仅 3 条（Win32 消息泵、tray 两接收器、iced 收发一对），**不新增 channel**；托盘不绕 iced；iced 不拥有 overlay/tray/hotkey
- `TagStore`/`Settings` 以 `Arc<Mutex>` 共享，主线程为唯一权威；iced 只收 `RefreshTags` 快照/`OpenSettings` 时读 `global_settings`

### 透明覆盖层
- 使用 `WS_EX_LAYERED` + `WS_EX_TRANSPARENT` 创建穿透式透明窗口
- 使用 `WS_EX_TOPMOST` 确保覆盖层在目标窗口之上
- 覆盖层在主线程创建，由主消息循环处理绘制消息
- 位置同步已实现：`EVENT_OBJECT_LOCATIONCHANGE` 事件驱动 + 500ms 兜底轮询；高 DPI 适配已实现（`SetProcessDpiAwarenessContext` Per-Monitor V2，见 [决策记录 D5](./doc/decision-records.md)）

### 配置持久化
- UI 偏好（主题/圆角）持久化到 `%APPDATA%\WinTag\config.toml`（TOML，`serde` 序列化），由 `src/core/settings.rs` 提供 `load`/`save`；任何加载失败（缺失/损坏/缺字段）回退默认配置并打印警告，绝不 panic
- 配置持久化打破"纯会话"原则，由 [决策记录 D9](./doc/decision-records.md) 授权、D10 落地：仅 UI 偏好持久化，标签/便签数据仍纯会话内
- 全局设置经 `OnceLock<Arc<Mutex<Settings>>>` 单例 + 主线程注入，与 `GLOBAL_TAG_STORE` 模式一致

### 主题注入（ui → sys）
- 主题调色板由 `ui::theme::set_theme(ThemeColors)` 写入全局状态，各窗口在 `WM_CTLCOLOR*` 时经 `theme_colors()` 读取同一调色板
- sys 层不反向依赖 ui：原生层偏好（覆盖层 tooltip 配色 / 角标标题 / 置顶 / 托盘气泡）经 `sys::native_prefs::set_native_prefs(NativePrefs)` 由主线程注入（D27 G1 收敛，替代原 `set_*` 散落 setter；`set_tag_store`/`set_message_target` 数据与传输另保留）
- 设置页保存后经 `WM_APP_THEME_CHANGED`（`WM_APP+5`）广播到主线程，主线程重新应用主题到各窗口并补发 `IcedCommand::ApplyTheme`，实时生效

### 安全性
- 绝不向其他进程注入代码
- 使用标准 Windows 消息和全局钩子进行跨进程通信
- 程序可能需要管理员权限以覆盖高权限窗口

### 数据管理
- 标签/便签数据仅存在于内存中，程序退出即清除；唯一例外是 UI 偏好（主题/圆角）持久化到配置文件（见"配置持久化"）
- 以窗口句柄（HWND）作为唯一标识，窗口关闭时自动移除标记
- 标签数据无需持久化：系统重启后窗口已非同一窗口，且工作进度已清空，无继承意义

## 当前阶段

项目处于 **MVP 开发中**，核心功能已实现，覆盖层位置同步、暗色主题、设置页与配置持久化均已完成；2026-08-29 完成 UI 全面现代化（D11：comctl32 v6 manifest + 全局字体 + 扩展调色板 + 自绘圆角按钮 + ListView DarkMode_Explorer + 三窗口布局重排 + 覆盖层 UpdateLayeredWindow 圆边三角形角标 + tooltip 圆角分层重绘）；2026-08-30 完成覆盖层标题条（R6/R11：角标旁圆角胶囊显示标签标题、5 字省略号截断、悬停看完整标题与备注、设置页开关）、概览面板树形化（R12：SysTreeView32 可展开列表）与面板单击置前（R13），见 [decision-records.md D12](./doc/decision-records.md)；同日完成面板标签变更自动刷新（D13：`WM_APP_TAGS_CHANGED` 广播经主线程转发、刷新保留展开状态）与面板最小宽度放宽（520 → 300 设计像素），见 [decision-records.md D13](./doc/decision-records.md)；同日完成弹窗键盘语义补全与三项修复（D14：Tab 在标题/备注/按钮间循环切换、备注框回车保存 Shift+回车换行、备注框遮挡按钮行的布局修复与 DPI 基准统一、面板默认纵向 400×640、覆盖层首绘异步化 + 创建后强制重绘 + 每次同步重申 HWND_TOPMOST 以修复新标注角标不显示），见 [decision-records.md D14](./doc/decision-records.md)；同日完成标注流程闭环（D16：角标/标题条单击打开预填编辑弹窗 R5、弹窗 5 色颜色选择行 R16、面板右键菜单置前/编辑/移除与 Esc 关闭 R17、标题/备注动态长度读取修复静默截断），见 [decision-records.md D16](./doc/decision-records.md)；同日完成三项实测缺陷修复（D18：设置页 WM_CTLCOLOR* 误用 lParam 为 HDC、下拉框改 CBS_OWNERDRAWFIXED 自绘推翻 D17-3 子类化方案——Win11 上显示区不走任何配色消息、tooltip 备注裁切根因修复——DrawTextW 不回写矩形 bottom + 默认字体量测行高偏小），见 [decision-records.md D18](./doc/decision-records.md)；2026-09-01 完成托盘常驻化（D24：Windows 子系统 + 单实例保护 + 默认托盘常驻零窗口启动 + 启动气泡可开关 + `--no-tray` 禁用 + 有标签退出弹定制主题确认窗 + 托盘右键四项菜单/左键与气泡打开面板 + 面板底部"退出"按钮 + 设置页"气泡提示"开关），见 [decision-records.md D24](./doc/decision-records.md)；同日完成底部控件客户区裁切修复（D25：`ui::layout` 新增 `TITLEBAR_H`/`client_height()` 统一从窗口外高抵扣标题栏边框，`confirm`/`popup`/`settings` 底部按钮行改用客户区高度定位，修复退出确认窗按钮溢出裁切），见 [decision-records.md D25](./doc/decision-records.md)；同轮完成托盘底层迁移（D26：托盘自 `Shell_NotifyIconW` 手写迁至 `tray-icon`(tauri)——`TrayIconBuilder`+`Menu(MenuItem)`+嵌入资源图标，事件经 `TrayIconEvent`/`MenuEvent` crossbeam channel 由主循环 `try_recv` 轮询分发；气泡由 `NIF_INFO`+`NIM_MODIFY` 迁至 `notify-rust`（Windows TOAST）；`TaskbarCreated` 重注册由 tray-icon 内部接管、删除 `WM_APP_TRAY` 常量；图标取自新增 `assets/icon.ico`（winresource ID=1，`Icon::from_resource`）），见 [decision-records.md D26](./doc/decision-records.md)；边缘情况（全屏降级、窗口激活闪烁反馈）登记于 [doc/decision-records.md](./doc/decision-records.md) 遗留项。

**2026-09-01 D27 落地（GUI 全面迁至 iced）**：四个 GUI 窗口（confirm/settings/popup/panel）迁至 iced（`tiny-skia` 软件渲染器 + 独立线程 `wintag-gui` + crossbeam 通道，分阶段 G0-G4），删除手写 Win32 UI（`ui/{button,layout,panel,popup,settings,confirm}.rs`），`ui/` 收敛为 `iced_app.rs`/`iced_proto.rs`/`geo.rs`/`theme.rs`；原生层注入收敛为 `sys::native_prefs::NativePrefs` + 单一 `pump_background_events` dispatch（托盘/覆盖层/热键保持纯 Win32）。见 [decision-records.md D27](./doc/decision-records.md) 与 [doc/iced-migration.md](./doc/iced-migration.md)。

**2026-09-02 D28 落地（概览面板 Win11 暗色紧凑视觉对齐 + 行内编辑 + 拖拽排序）**：面板视觉对齐用户 HTML demo——新增 `ui/panel_style.rs` 叶子（`PanelPalette` Win11 暗色/亮色 + 图标字形常量 + `truncate_units` CJK 双宽省略号纯函数）；`panel_row` 改「拖拽手柄 + chevron + 标题|窗口 合并（省略号）+ hover 高亮 + 展开区图标按钮（置前▲/编辑✎/移除🗑）」；列表改用 `keyed::Column`（key=`target`）；行内双击编辑（`PanelState.editing` 状态驱动换 `text_input`，Enter 保存复用 `GuiEvent::TagSaved`、Esc 编辑优先取消）；拖拽手柄排序（`mouse_area` 组合：手柄起拖 / 行上报预览位 / 列表级提交与取消，drop 经 `preview_reorder` 发新增 `GuiEvent::ReorderTags{targets}` 回主线程写 `TAG_ORDER`）。默认排序改按标题字母/拼音升序（`main.rs` 维护 `TAG_ORDER`）。不用右键菜单（用图标按钮替代）。见 [decision-records.md D28](./doc/decision-records.md)。


**扩展规划（2026-08-31，只读调研，未实施）**：功能增强与使用性问题修复已调研并归档——① ~~系统托盘（D22，v1 系统图标可接受）~~（✅ 已随 D24 落地）；② 标签分组/工作区（D23，**纯会话不持久化**，分组控件置于新建标签弹窗标题下方，可输入或选择已有分组）；③ 统计图表（D23，形态待确认，默认按分组计数柱状图）；④ 自定义快捷键（D23，设置页"快捷键"分页录制）；⑤ 面板增强（默认展开、一键展开收起、贴边隐藏、纯键盘操作，D21）；⑥ 标签跟随移动修复（D21，MOVESIZESEND 最终同步 + 轮询加速）。详见 [doc/issues-and-requirements.md 四、扩展规划](./doc/issues-and-requirements.md) 与 [doc/decision-records.md D21-D23](./doc/decision-records.md)。详细设计文档见 `doc/` 目录。