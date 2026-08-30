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
├── main.rs          # 入口，消息循环、热键分发、覆盖层管理
├── build.rs         # 构建脚本：嵌入 comctl32 v6 视觉样式 manifest（D11）
├── sys/             # 底层系统服务层 — Win32 API 调用
│   ├── mod.rs
│   ├── window.rs    # 窗口检测、句柄捕获、事件监听
│   ├── badge.rs     # 角标/标题条软件光栅渲染纯函数（SDF 圆边三角形 + 圆角矩形 + 标题截断，D11/D12）
│   └── overlay.rs   # 透明覆盖层绘制与同步（UpdateLayeredWindow 逐像素 alpha，BGRA 字节序，D17；D12 起含可选圆角标题条，开关经 set_show_title 注入；D14 首绘异步化 + 每次同步重申置顶；D16 单击经 WM_APP_EDIT_TAG 请求编辑；D18 tooltip 分字体 DT_CALCRECT 量测、DrawTextW 返回值推进备注行）
├── core/            # 核心数据管理层
│   ├── mod.rs
│   ├── tag.rs       # 标签数据结构定义（内存中，无持久化）
│   ├── matcher.rs   # 窗口句柄匹配逻辑
│   └── settings.rs  # 配置数据模型与 TOML 持久化（%APPDATA%\WinTag\config.toml；含 show_badge_title，R6）
├── ui/              # 用户界面层
│   ├── mod.rs
│   ├── panel.rs     # 全局概览面板（DarkMode_Explorer；D12 改 SysTreeView32 可展开树形列表 + 单击置前目标窗口；D13 标签变更自动刷新 WM_APP_TAGS_CHANGED + MIN_W 300；D14 默认纵向 400×640）；D15 根项「标题 | 窗口名称」同行、展开显示完整多行备注
│   ├── popup.rs     # 悬浮便签浮窗（布局重排 + 自绘按钮，D11；Tab 循环/回车保存/Shift+回车换行子类化转发，D14；颜色色块行 + 动态长度读取，D16；单例复用 + 光标附近定位，D17）
│   ├── button.rs    # 自绘圆角按钮模块（BS_OWNERDRAW + WM_DRAWITEM，D11）
│   ├── layout.rs    # DPI 缩放辅助（dp()，D11）
│   ├── theme.rs     # 暗色主题与圆角 + 全局字体 + 扩展调色板（DWM + WM_CTLCOLOR + lfMessageFont，D11；统一主题管理器 sync_window_theme/apply_control_theme + 下拉框 DarkMode_Explorer 变体，D17）
│   └── settings.rs  # 设置页面窗口（主题/圆角选择、角标显示标题开关、保存、WM_APP_THEME_CHANGED 广播；下拉框 CBS_OWNERDRAWFIXED 自绘 + WM_CTLCOLOR* 以 wParam 为 HDC，D18）
└── hotkey.rs        # 全局热键注册
tests/
└── smoke.rs         # 冒烟测试（30 个用例）
```

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
- 全局状态通过 `OnceLock<Arc<Mutex<...>>>` 在 WndProc 和主循环间共享

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
- sys 层不反向依赖 ui：覆盖层 tooltip 配色经 `sys::overlay::set_tooltip_theme(bg, fg)` 由主线程注入（镜像 D6 的 `set_tag_store` 注入模式，见 [决策记录 D6](./doc/decision-records.md)）
- 设置页保存后经 `WM_APP_THEME_CHANGED`（`WM_APP+5`）广播到主线程，主线程重新应用主题到各窗口，实时生效

### 安全性
- 绝不向其他进程注入代码
- 使用标准 Windows 消息和全局钩子进行跨进程通信
- 程序可能需要管理员权限以覆盖高权限窗口

### 数据管理
- 标签/便签数据仅存在于内存中，程序退出即清除；唯一例外是 UI 偏好（主题/圆角）持久化到配置文件（见"配置持久化"）
- 以窗口句柄（HWND）作为唯一标识，窗口关闭时自动移除标记
- 标签数据无需持久化：系统重启后窗口已非同一窗口，且工作进度已清空，无继承意义

## 当前阶段

项目处于 **MVP 开发中**，核心功能已实现，覆盖层位置同步、暗色主题、设置页与配置持久化均已完成；2026-08-29 完成 UI 全面现代化（D11：comctl32 v6 manifest + 全局字体 + 扩展调色板 + 自绘圆角按钮 + ListView DarkMode_Explorer + 三窗口布局重排 + 覆盖层 UpdateLayeredWindow 圆边三角形角标 + tooltip 圆角分层重绘）；2026-08-30 完成覆盖层标题条（R6/R11：角标旁圆角胶囊显示标签标题、5 字省略号截断、悬停看完整标题与备注、设置页开关）、概览面板树形化（R12：SysTreeView32 可展开列表）与面板单击置前（R13），见 [decision-records.md D12](./doc/decision-records.md)；同日完成面板标签变更自动刷新（D13：`WM_APP_TAGS_CHANGED` 广播经主线程转发、刷新保留展开状态）与面板最小宽度放宽（520 → 300 设计像素），见 [decision-records.md D13](./doc/decision-records.md)；同日完成弹窗键盘语义补全与三项修复（D14：Tab 在标题/备注/按钮间循环切换、备注框回车保存 Shift+回车换行、备注框遮挡按钮行的布局修复与 DPI 基准统一、面板默认纵向 400×640、覆盖层首绘异步化 + 创建后强制重绘 + 每次同步重申 HWND_TOPMOST 以修复新标注角标不显示），见 [decision-records.md D14](./doc/decision-records.md)；同日完成标注流程闭环（D16：角标/标题条单击打开预填编辑弹窗 R5、弹窗 5 色颜色选择行 R16、面板右键菜单置前/编辑/移除与 Esc 关闭 R17、标题/备注动态长度读取修复静默截断），见 [decision-records.md D16](./doc/decision-records.md)；同日完成三项实测缺陷修复（D18：设置页 WM_CTLCOLOR* 误用 lParam 为 HDC、下拉框改 CBS_OWNERDRAWFIXED 自绘推翻 D17-3 子类化方案——Win11 上显示区不走任何配色消息、tooltip 备注裁切根因修复——DrawTextW 不回写矩形 bottom + 默认字体量测行高偏小），见 [decision-records.md D18](./doc/decision-records.md)；边缘情况（全屏降级、托盘图标、窗口激活闪烁反馈）登记于 [doc/decision-records.md](./doc/decision-records.md) 遗留项。详细设计文档见 `doc/` 目录。