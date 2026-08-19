# AGENTS.md — WinTag 项目开发约定

## 项目概述

WinTag 是 Windows 平台的外挂式窗口语义增强工具，使用 Rust 开发。通过 Win32 API 监听窗口事件并在目标窗口上绘制透明覆盖层来显示用户自定义标签和便签。所有数据仅存在于当前会话，程序退出即清除，无持久化存储。

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
├── sys/             # 底层系统服务层 — Win32 API 调用
│   ├── mod.rs
│   ├── window.rs    # 窗口检测、句柄捕获、事件监听
│   └── overlay.rs   # 透明覆盖层绘制与同步
├── core/            # 核心数据管理层
│   ├── mod.rs
│   ├── tag.rs       # 标签数据结构定义（内存中，无持久化）
│   └── matcher.rs   # 窗口句柄匹配逻辑
├── ui/              # 用户界面层
│   ├── mod.rs
│   ├── panel.rs     # 全局概览面板
│   └── popup.rs     # 悬浮便签浮窗
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
- 待实现：`EVENT_OBJECT_LOCATIONCHANGE` 事件同步位置、高 DPI 缩放

### 安全性
- 绝不向其他进程注入代码
- 使用标准 Windows 消息和全局钩子进行跨进程通信
- 程序可能需要管理员权限以覆盖高权限窗口

### 数据管理
- 所有数据仅存在于内存中，程序退出即清除
- 以窗口句柄（HWND）作为唯一标识，窗口关闭时自动移除标记
- 无需持久化：系统重启后窗口已非同一窗口，且工作进度已清空，无继承意义

## 当前阶段

项目处于 **MVP 开发中**，核心功能已实现，覆盖层位置同步等细节待完善。详细设计文档见 `doc/` 目录。