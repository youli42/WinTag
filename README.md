# WinTag

外挂式 Windows 窗口语义增强工具 —— 为窗口对象添加临时语义标记，在多窗口并行工作时快速追踪每个窗口正在执行什么任务，将短期记忆外挂在可视化界面上。

## 核心痛点

在 AI 协作场景下，同时开启多个对话窗口形成并行"思维线程"。AI 生成内容需要时间，用户被迫在任务间频繁切换，导致：

- **目标迷失**：回到窗口时忘记当初开启对话的目的
- **任务遗忘**：AI 已输出完毕，但彻底忘了曾开启过这个任务
- **管理混乱**：窗口堆叠时缺乏视觉索引，难以快速定位

## 解决方案

WinTag 在 Windows 窗口管理之上增加一层**元数据图层**，让用户随时了解每个窗口"正在执行什么命令、目的是什么"，通过自定义标签和便签，秒级恢复工作上下文。标签与便签数据仅存在于当前会话，程序退出即清除；UI 偏好（主题/圆角）持久化到配置文件（见下）。

## 核心功能

| 模块 | 功能 | 状态 |
| :--- | :--- | :--- |
| 窗口感知与定位 | 自动检测活动窗口，以窗口句柄唯一标识，仅会话内有效 | ✅ 已完成 |
| 标签与便签UI | 左上角悬浮标记，鼠标悬停展开便签（标题+备注），快捷键快速添加 | ✅ 已完成 |
| 全局概览面板 | 快捷键呼出聚合列表，展示所有已标记窗口，双击跳转，搜索过滤 | ✅ 已完成 |
| 覆盖层位置同步 | 窗口移动/缩放/最小化时覆盖层跟随与显隐（WinEvent 事件驱动 + 500ms 兜底轮询） | ✅ 已完成 |
| 暗色模式 | 跟随系统/浅色/深色三档切换（Win32 原生 DWM 沉浸式暗色 + `WM_CTLCOLOR` 画刷） | ✅ 已完成 |
| 设置页面 | `Ctrl+Shift+S` 呼出设置窗口，主题/圆角选择、实时预览、保存 | ✅ 已完成 |
| 配置持久化 | UI 偏好保存至 `%APPDATA%\WinTag\config.toml`（TOML 格式），启动自动加载 | ✅ 已完成 |
| 圆角 UI | 覆盖层/弹窗/面板窗口圆角（`DWMWA_WINDOW_CORNER_PREFERENCE`，Win11） | ✅ 已完成 |

## 技术栈

- **语言**：Rust
- **GUI 实现**：纯 Win32 原生窗口（windows-rs 0.58，无 egui/eframe）
- **系统 API**：windows-rs (Win32)，含 `SetWinEventHook` 窗口事件监听、`SetProcessDpiAwarenessContext` 高 DPI 感知、DWM 沉浸式暗色与 `DwmSetWindowAttribute` 圆角
- **存储**：标签数据仅会话内内存 HashMap；UI 偏好持久化至 `%APPDATA%\WinTag\config.toml`（`serde` + `toml` 序列化）

## 快速开始

```pwsh
# 编译
cargo build

# 运行
cargo run

# 运行测试
cargo test
```

### 使用方式

| 快捷键 | 功能 |
| :--- | :--- |
| `Ctrl+Shift+N` | 为当前活动窗口添加标签 |
| `Ctrl+Shift+M` | 打开概览面板，查看所有已标记窗口 |
| `Ctrl+Shift+S` | 打开设置页面（主题/圆角） |

标记窗口后，目标窗口左上角会出现橙色圆点覆盖层，提示该窗口已被标记。

## 测试

项目包含 48 个测试（30 个冒烟测试 + 18 个单元测试），覆盖核心数据层、配置持久化、主题解析、热键逻辑与 WinEvent 事件分类：

```pwsh
cargo test
```

测试覆盖：

| 模块 | 测试数 | 内容 |
| :--- | :--- | :--- |
| core::settings | 5 | 默认值、serde 往返、损坏配置回退、保存/加载往返、中文标签 |
| sys::win_event | 6 | 事件分类映射、转发过滤边界 |
| ui::theme | 3 | 跟随系统解析、明暗调色板、画刷缓存 |
| ui::settings | 2 | 主题/圆角下拉索引映射 |
| hotkey | 2 | 热键 ID 往返、设置页热键常量 |
| 冒烟测试 | 30 | 标签/存储/匹配 CRUD、热键解析、窗口信息、完整标记流程、边界值 |

## 项目状态

✅ **MVP 核心功能已完成** — 窗口检测、标记/便签、概览面板、覆盖层位置同步、高 DPI、暗色模式（跟随系统/浅色/深色）、设置页面（`Ctrl+Shift+S`）、配置持久化（`%APPDATA%\WinTag\config.toml`）与圆角 UI（Win11）均已实现。边缘情况（全屏降级、托盘图标、窗口激活闪烁反馈）见 [doc/decision-records.md](./doc/decision-records.md) 的遗留项登记。

## 文档

- [需求规格说明](./doc/requirements.md)
- [架构设计](./doc/architecture.md)
- [技术规格](./doc/technical-specs.md)
- [开发计划](./doc/development-plan.md)
- [问题与需求记录](./doc/issues-and-requirements.md)
- [决策记录](./doc/decision-records.md)

## 许可证

MIT