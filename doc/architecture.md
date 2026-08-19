# 架构设计

## 分层架构

```
┌─────────────────────────────────────────┐
│              UI Layer (ui/)              │
│  ┌──────────┐  ┌────────────────────┐   │
│  │  panel   │  │      popup         │   │
│  │ 概览面板  │  │  悬浮便签浮窗        │   │
│  └──────────┘  └────────────────────┘   │
├─────────────────────────────────────────┤
│          Data Core Layer (core/)         │
│  ┌──────────┐ ┌──────────┐ ┌────────┐  │
│  │   tag    │ │ storage  │ │matcher │  │
│  │ 数据结构  │ │ SQLite   │ │窗口匹配 │  │
│  └──────────┘ └──────────┘ └────────┘  │
├─────────────────────────────────────────┤
│        System Service Layer (sys/)       │
│  ┌──────────┐  ┌────────────────────┐   │
│  │  window  │  │     overlay        │   │
│  │ 窗口监听  │  │  透明覆盖层绘制      │   │
│  └──────────┘  └────────────────────┘   │
│       windows-rs (Win32 API)            │
└─────────────────────────────────────────┘
```

## 依赖方向

```
ui → core → sys
```

禁止反向依赖。`sys` 层不感知 `core` 和 `ui`，`core` 层不感知 `ui`。

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

### core/ — 数据核心层

负责业务逻辑与数据管理，不涉及 UI 渲染。

#### tag.rs
- 定义核心数据结构：
  ```rust
  struct Tag {
      id: u64,
      window_id: WindowId,
      title: String,        // 必填，快速索引
      note: String,         // 选填，长文本描述
      color: Color,         // 标记颜色
      created_at: DateTime,
      updated_at: DateTime,
  }
  ```

#### storage.rs
- SQLite 数据库初始化与迁移
- CRUD 操作封装
- 连接池管理

#### matcher.rs
- 窗口标识匹配逻辑
- 实现窗口重开后的标签继承（按进程路径匹配）
- 处理窗口句柄失效后的重新绑定

### ui/ — 用户界面层

负责所有用户可见的界面元素。

#### panel.rs
- 全局概览面板主窗口
- 列表/网格展示所有已标记窗口
- 搜索过滤功能
- 双击/点击跳转到对应窗口

#### popup.rs
- 悬浮便签浮窗
- 鼠标悬停触发显示
- 提供编辑/删除按钮
- 自动跟随标记位置

### hotkey.rs
- 注册全局热键 (`RegisterHotKey`)
- 处理热键消息，触发概览面板显示/隐藏

## 数据流

### 标记窗口流程
```
1. 用户按下热键 → 触发标记操作
2. sys/window 获取当前活动窗口信息
3. core/matcher 检查是否已有标签
4. ui/popup 显示编辑界面
5. 用户确认 → core/storage 保存标签
6. sys/overlay 在目标窗口上创建覆盖层
```

### 窗口切换流程
```
1. sys/window 检测到 EVENT_SYSTEM_FOREGROUND
2. core/matcher 查找新窗口的标签
3. 如果有标签 → sys/overlay 显示标记并短暂闪烁
4. 如果无标签 → 隐藏覆盖层
```

### 窗口关闭流程
```
1. sys/window 检测到 EVENT_OBJECT_DESTROY
2. core/storage 保存标签数据
3. sys/overlay 销毁覆盖层
4. core/matcher 保留标签以供窗口重开时继承
```

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