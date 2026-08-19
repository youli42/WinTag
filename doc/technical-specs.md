# 技术规格

## 核心技术难点

### 难点一：绘制"穿透"且"置顶"的覆盖层

覆盖层窗口需要满足三个条件：

1. **置顶**：始终在目标窗口之上 — `WS_EX_TOPMOST`
2. **透明穿透**：不拦截鼠标点击 — `WS_EX_TRANSPARENT` + `WS_EX_LAYERED`
3. **位置同步**：跟随目标窗口移动/缩放 — 监听 `EVENT_OBJECT_LOCATIONCHANGE`

#### 关键 API

```rust
// 创建覆盖层窗口
CreateWindowExW(
    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
    ...
)

// 设置透明度和颜色键
SetLayeredWindowAttributes(hwnd, color_key, alpha, LWA_COLORKEY | LWA_ALPHA)

// 或使用 UpdateLayeredWindow 进行更精细的绘制
UpdateLayeredWindow(hwnd, hdc_screen, &ppt_dst, &size, hdc_mem, &ppt_src, ...)
```

#### 实现要点

- 使用 `UpdateLayeredWindow` 而非 `SetLayeredWindowAttributes`，因为前者支持 GDI+ 绘制
- 绘制时使用 `WS_EX_NOACTIVATE` 防止覆盖层抢占焦点
- 目标窗口最小化时隐藏覆盖层，恢复时重新显示

### 难点二：跨进程安全通信

程序是独立进程，但需要操作其他进程的窗口。

#### 安全原则

- **绝不注入代码**：不使用 `CreateRemoteThread`、`WriteProcessMemory` 等
- 使用标准 Windows 消息和全局钩子
- 使用 `SetWinEventHook` 进行被动监听，不修改目标进程行为

#### 窗口事件监听

```rust
// 使用 windows-rs 注册全局钩子
let hook = SetWinEventHook(
    EVENT_SYSTEM_FOREGROUND,
    EVENT_OBJECT_LOCATIONCHANGE,
    None,
    Some(win_event_proc),
    0, 0,
    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
);
```

### 难点三：高 DPI 适配

Windows 缩放比例（125%、150%、200%）会导致坐标偏移。

#### 解决方案

1. 先声明 DPI 感知：
   ```rust
   SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE)
   ```

2. 获取窗口的实际 DPI：
   ```rust
   let dpi = GetDpiForWindow(hwnd);
   let scale = dpi as f32 / 96.0;
   ```

3. 所有坐标和尺寸计算都乘以 scale 因子

## 窗口唯一标识方案

以窗口句柄（HWND）为唯一标识，仅会话内有效。

```rust
// 标签以 HWND 为键，存储在 HashMap 中
type TagStore = HashMap<HWND, Tag>;
```

窗口句柄在进程生命周期内唯一，窗口关闭后句柄失效。不使用进程路径、进程名等持久化标识，因为：
- 系统重启后 HWND 必然不同，无法匹配
- 即使通过进程路径匹配到"同一个窗口"，工作进度已清空，标签继承无意义

### 匹配策略

1. 直接用 HWND 查找 HashMap
2. 窗口关闭时，自动从 HashMap 中移除对应条目
3. 无需持久化匹配或用户确认

## 数据存储

无持久化存储。所有标签数据以 `HashMap<HWND, Tag>` 形式存于内存中，程序退出即清除。无需数据库、无需文件 I/O、无需序列化。

## 覆盖层绘制细节

### 标记外观

- 小圆点，直径约 12px
- 颜色可自定义，默认橙色 `#FFB74D`
- 位置：目标窗口左上角内偏移 (8px, 8px)
- 边框：1px 白色半透明边框增加可见性

### 便签浮窗

- 白色背景，圆角矩形，阴影效果
- 显示标题（粗体）和备注内容
- 底部显示"编辑"和"删除"按钮
- 最大宽度 300px，内容超出可滚动

## 边缘情况处理

| 情况 | 处理方式 |
| :--- | :--- |
| 目标窗口最小化 | 隐藏覆盖层 |
| 目标窗口最大化 | 重新计算覆盖层位置 |
| 目标窗口关闭 | 移除标签，销毁覆盖层 |
| 全屏应用 | 检测全屏状态，隐藏覆盖层，仅托盘通知 |
| 多显示器 | 覆盖层跟随窗口在不同显示器间移动 |
| 管理员权限窗口 | 程序也需以管理员权限运行 |
| 程序自身退出 | 销毁所有覆盖层，丢弃所有数据 |

## 性能目标

- 覆盖层位置同步延迟：< 50ms
- 程序后台 CPU 占用：< 1%
- 程序内存占用：< 50MB
- 支持同时标记窗口数：≥ 50