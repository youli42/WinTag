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
│   ├── badge.rs     # 角标软件光栅渲染纯函数（SDF 圆边三角形，D11）
│   └── overlay.rs   # 透明覆盖层绘制与同步（UpdateLayeredWindow 逐像素 alpha，D11）
├── core/            # 核心数据管理层
│   ├── mod.rs
│   ├── tag.rs       # 标签数据结构定义（内存中，无持久化）
│   ├── matcher.rs   # 窗口句柄匹配逻辑
│   └── settings.rs  # 配置数据模型与 TOML 持久化（%APPDATA%\WinTag\config.toml）
├── ui/              # 用户界面层
│   ├── mod.rs
│   ├── panel.rs     # 全局概览面板（DarkMode_Explorer + NM_CUSTOMDRAW，D11）
│   ├── popup.rs     # 悬浮便签浮窗（布局重排 + 自绘按钮，D11）
│   ├── button.rs    # 自绘圆角按钮模块（BS_OWNERDRAW + WM_DRAWITEM，D11）
│   ├── layout.rs    # DPI 缩放辅助（dp()，D11）
│   ├── theme.rs     # 暗色主题与圆角 + 全局字体 + 扩展调色板（DWM + WM_CTLCOLOR + lfMessageFont，D11）
│   └── settings.rs  # 设置页面窗口（主题/圆角选择、保存、WM_APP_THEME_CHANGED 广播）
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

项目处于 **MVP 开发中**，核心功能已实现，覆盖层位置同步、暗色主题、设置页与配置持久化均已完成；2026-08-29 完成 UI 全面现代化（D11：comctl32 v6 manifest + 全局字体 + 扩展调色板 + 自绘圆角按钮 + ListView DarkMode_Explorer + 三窗口布局重排 + 覆盖层 UpdateLayeredWindow 圆边三角形角标 + tooltip 圆角分层重绘）；边缘情况（全屏降级、托盘图标、窗口激活闪烁反馈）登记于 [doc/decision-records.md](./doc/decision-records.md) 遗留项。详细设计文档见 `doc/` 目录。