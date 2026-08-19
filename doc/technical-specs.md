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

```rust
struct WindowId {
    process_path: String,  // 进程完整路径，如 "C:\Program Files\Google\Chrome\Application\chrome.exe"
    process_name: String,  // 进程名，如 "chrome.exe"
    window_title: String,  // 窗口标题（辅助匹配，非唯一）
}
```

### 匹配策略

1. **精确匹配**：窗口句柄仍然有效 → 直接匹配句柄
2. **进程路径匹配**：窗口重开后 → 按 `process_path` + 标题相似度匹配
3. **用户确认**：无法自动匹配时 → 提示用户手动关联

## 数据存储设计

### SQLite 表结构

```sql
CREATE TABLE tags (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    process_path TEXT NOT NULL,
    process_name TEXT NOT NULL,
    window_title TEXT NOT NULL,
    title       TEXT NOT NULL,
    note        TEXT DEFAULT '',
    color       TEXT DEFAULT '#FFB74D',
    is_active   INTEGER DEFAULT 1,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_process_path ON tags(process_path);
CREATE INDEX idx_title ON tags(title);
```

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
| 目标窗口关闭 | 保存标签数据，销毁覆盖层 |
| 窗口重开 | 按进程路径匹配，恢复标签和覆盖层 |
| 全屏应用 | 检测全屏状态，隐藏覆盖层，仅托盘通知 |
| 多显示器 | 覆盖层跟随窗口在不同显示器间移动 |
| 管理员权限窗口 | 程序也需以管理员权限运行 |
| 程序自身退出 | 销毁所有覆盖层，保存所有数据 |

## 性能目标

- 覆盖层位置同步延迟：< 50ms
- 程序后台 CPU 占用：< 1%
- 程序内存占用：< 50MB
- 支持同时标记窗口数：≥ 50