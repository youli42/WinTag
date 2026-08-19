# AGENTS.md — WinTag 项目开发约定

## 项目概述

WinTag 是 Windows 平台的外挂式窗口语义增强工具，使用 Rust 开发。通过 Win32 API 监听窗口事件并在目标窗口上绘制透明覆盖层来显示用户自定义标签和便签。

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
├── main.rs          # 入口，初始化各子系统
├── sys/             # 底层系统服务层 — Win32 API 调用
│   ├── mod.rs
│   ├── window.rs    # 窗口检测、句柄捕获、事件监听
│   └── overlay.rs   # 透明覆盖层绘制与同步
├── core/            # 核心数据管理层
│   ├── mod.rs
│   ├── tag.rs       # 标签数据结构定义
│   ├── storage.rs   # SQLite 读写封装
│   └── matcher.rs   # 窗口唯一标识匹配逻辑
├── ui/              # 用户界面层
│   ├── mod.rs
│   ├── panel.rs     # 全局概览面板
│   └── popup.rs     # 悬浮便签浮窗
└── hotkey.rs        # 全局热键注册
```

## 编码规范

- 遵循 Rust 官方命名规范：`snake_case` 变量/函数，`CamelCase` 类型/结构体
- 所有 `unsafe` 块必须添加 `// SAFETY:` 注释说明安全性
- 使用 `anyhow` 或 `thiserror` 进行错误处理，禁止裸 `unwrap()` 在非测试代码中
- 模块间的依赖方向：`ui → core → sys`，禁止反向依赖
- 所有公开 API 必须添加文档注释 (`///`)

## 关键技术点

### 透明覆盖层
- 使用 `WS_EX_LAYERED` + `WS_EX_TRANSPARENT` 创建穿透式透明窗口
- 使用 `WS_EX_TOPMOST` 确保覆盖层在目标窗口之上
- 监听 `EVENT_OBJECT_LOCATIONCHANGE` 事件同步位置，而非轮询
- 必须处理高 DPI 缩放 (`GetDpiForWindow`)

### 安全性
- 绝不向其他进程注入代码
- 使用标准 Windows 消息和全局钩子进行跨进程通信
- 程序可能需要管理员权限以覆盖高权限窗口

### 数据持久化
- 使用 SQLite 存储标签数据
- 窗口关闭时自动保存，程序退出时无感保存
- 通过进程路径匹配实现窗口重开后的标签继承

## 当前阶段

项目处于 **规划阶段**，尚未开始编码。详细设计文档见 `doc/` 目录。