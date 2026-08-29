# 问题与需求记录

> 记录当前已确认的问题与待规划的需求，供后续开发排期参考。
> 建立日期：2026-08-20。当前处于**快速开发阶段，不承诺向后兼容**（见 [decision-records.md D9](./decision-records.md)），
> 以下问题与需求可能随开发推进随时调整或关闭。

## 一、已确认问题

> 状态说明：问题 5.1、5.2 已修复（见文末[更新记录](#三更新记录)）；问题 2 曾修复但实测未完全生效，**重新开放**（见问题 9.3）；其余问题仍在开放状态。
> 2026-08-21 实测反馈：追加问题 7-10（角标形状/可点击、主题 8 项、弹窗布局）与需求 R6-R10，见文末[更新记录](#三更新记录)。

### 1. 多标记重叠时无法判断当前焦点窗口

- **现象**：所有标记（覆盖层圆点）置于最上层（`WS_EX_TOPMOST`）是对的，但多个窗口重叠时，
  无法判断当前焦点在哪个窗口，会导致**下面的标记覆盖上面的**（Z 序错误）。
- **现状**：覆盖层统一使用 `WS_EX_TOPMOST` 创建与同步，`EVENT_SYSTEM_FOREGROUND` 仅做 `BringToTop` 动作，
  未维护"焦点窗口的标记应高于其他标记"的 Z 序关系。
- **期望**：焦点窗口对应的标记应始终在其它标记之上（按焦点动态调整 Z 序）。

### 2. Manager 窗口表头默认不显示

> **状态**：✅ 已修复（提交 `5fa787c` + D11 `DarkMode_Explorer`）；⚠️ **更正（2026-08-21 实测反馈）**：`LVS_EX_HEADERDRAGDROP` 修复未完全生效；**2026-08-29 二次修复**：D11 经 `SetWindowTheme("DarkMode_Explorer")` + comctl32 v6 manifest 使表头 SysHeader32 暗色可见，问题关闭。

- **现象**：Manager（概览面板）窗口中的表格表头默认不显示，需要点击后才显示。
- **现状**：面板 ListView 未设置显式表头相关样式/状态，表头渲染依赖交互触发。
- **修复**：面板 ListView 设置显式表头样式，表头默认可见；并同步支持暗色主题配色。
- **期望**：表头默认可见。

### 3. 退出时报错 STATUS_CONTROL_C_EXIT (0xc000013a)

- **现象**：退出时提示：
  `error: process didn't exit successfully: target\debug\wintag.exe (exit code: 0xc000013a, STATUS_CONTROL_C_EXIT)`
- **现状**：程序是控制台子系统程序，Ctrl+C 直接触发 CRT 的默认控制台终止处理，未安装
  `SetConsoleCtrlHandler` 优雅退出路径；WinEvent hook 虽由 Drop 注销，但进程仍以非 0 退出码结束。
- **期望**：收到 Ctrl+C（`CTRL_C_EVENT`）时执行清理后以 0 退出码正常退出（或至少消除报错）。

### 4. 悬浮角标内容过长被截断

- **状态**：✅ 已修复（D11：tooltip 宽度自适应 + 圆角分层重绘）
- **现象**：鼠标悬浮在角标（覆盖层圆点）上时，标题/备注过长会被截断，无法看到完整内容。
- **修复**：tooltip 宽度按内容自适应（`DrawTextW` 预量），上限 360px（原固定 300px 截断）；标题/备注分层排版（标题加粗 + 备注正文），圆角矩形 + 1px 边框。

### 5. 操作不便

> **状态**：5.1 与 5.2 均已修复（提交 `b5d80df` fix(popup)）。

- **5.1 弹窗默认焦点不在编辑框**：按下 `Ctrl+Shift+N` 后，焦点不在标记弹窗的编辑框中，需要点击后才能编辑。
  - 现状：`create_popup` 只 `ShowWindow(SW_SHOW)` 未调用 `SetForegroundWindow`，`WM_CREATE` 中的
    `SetFocus(title_edit)` 在窗口未激活时无效（键盘焦点仍留在后台目标窗口）。
  - 修复：弹窗创建后调用 `SetForegroundWindow` 激活并聚焦标题编辑框（提交 `b5d80df`）。
  - 期望：弹窗弹出即激活并聚焦标题编辑框。
- **5.2 回车不能直接保存**：按下回车不会保存，必须鼠标点击"确认"按钮。
  - 现状：弹窗无默认按钮（`BS_DEFPUSHBUTTON`），消息循环未走 `IsDialogMessage`，回车键无绑定动作。
  - 修复：弹窗处理 `WM_KEYDOWN` 的 `VK_RETURN` 分支触发保存，与确认按钮语义一致（提交 `b5d80df`）。
  - 期望：回车触发保存（与确认按钮等价）。

### 6. 无法为管理员权限（提权）窗口配置角标，且无对应报错与错误处理

- **现象**：目标窗口以管理员身份运行（如管理员模式的记事本、终端、VS Code 等）时，无法为其配置角标；且整个过程无任何用户可见的报错或提示，表现为"按了快捷键但什么都没发生"或"确认后角标静默缺失"。
- **现状**：WinTag 默认非提权运行，Windows UIPI（用户界面特权隔离）会拦截对提权窗口的跨特权操作，且现有失败路径全部只落控制台日志、无用户可见反馈：
  - 标记流程第一步 `sys::window::get_foreground_window_info()` 中 `OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)` 对提权进程返回拒绝访问（access denied），整个调用以 `Err` 返回（见 `src/sys/window.rs` 第 74 行起）；
  - `main.rs` 的 `handle_quick_tag` 对上述失败仅 `eprintln!("获取窗口信息失败")`（第 402-404 行），弹窗不出现，用户无感知；
  - 即使绕过首步走到确认分支，`Overlay::create` 失败也仅在隐藏窗口 WndProc 中 `eprintln!("[覆盖层] 创建失败")`（第 212-214 行），角标静默缺失；
  - WinEvent 钩子（`WINEVENT_OUTOFCONTEXT`）收不到提权窗口的事件（UIPI 过滤），即使覆盖层创建成功也无法跟随同步（该层面已登记于 [decision-records.md F4](./decision-records.md)）。
- **期望**：检测到目标窗口为提权窗口时给出明确提示（如"该窗口以管理员身份运行，请以管理员身份运行 WinTag 后重试"）；或提供可用的降级路径；所有失败路径应有用户可见的错误处理，而非仅控制台日志。
- **关联**：[decision-records.md F4 管理员权限窗口兼容](./decision-records.md)——F4 登记"WinEvent 事件被拦截"，本条登记"配置（标记+覆盖层创建）失败路径与缺失的错误处理"。

### 7. 角标仍是方形，未做成三角形贴合角落

- **状态**：✅ 已修复（D11：角标改 `UpdateLayeredWindow` 逐像素 alpha + 软件光栅圆边三角形）
- **现象**：覆盖层角标显示为方形色块，没有做成三角形贴合窗口左上角。
- **修复**：`src/sys/badge.rs` 纯函数软件光栅化贴角圆边三角形（SDF 抗锯齿 + 1px 描边），`overlay.rs` 改用 `UpdateLayeredWindow(ULW_ALPHA)` 提交 32bpp 预乘 RGBA，替代原 `FillRect(DOT_RECT)` 实心方块 + `LWA_COLORKEY` 色键透明。
- **关联**：R2（已完成）

### 8. 无法点击角标进行快捷编辑

- **现象**：点击角标无反应，无法直接进入标记编辑。
- **现状**：覆盖层 `WM_NCHITTEST` 对圆点区域返回 `HTCLIENT`、其余 `HTTRANSPARENT`（overlay.rs 约 415-425 行），但无 `WM_LBUTTONDOWN`/点击处理逻辑；需求 R5"角标可点击编辑"已在需求规划中登记。
- **期望**：点击角标弹出标记编辑弹窗（或直接编辑），需要覆盖层→隐藏窗口→popup 的消息链路。
- **关联**：R5

### 9. 主题与页面问题（实测反馈）

> 2026-08-21 用户实测反馈，一组 8 个子项（9.1-9.8），涉及设置页/概览面板/弹窗的主题化与布局。
> **状态**：✅ 全部已修复（D11：comctl32 v6 manifest + 全局字体 + 扩展调色板 + DarkMode_Explorer + 自绘按钮 + 布局重排）

- **9.1 设置页面所有字体都是白色背景**：✅ 已修复。comctl32 v6 manifest 使控件视觉样式生效，`apply_font_to_children` 注入全局字体，`WM_CTLCOLORSTATIC` 配色收尾。
- **9.2 概览面板的表格背景还是纯白色**：✅ 已修复。ListView `SetWindowTheme("DarkMode_Explorer")` + `NM_CUSTOMDRAW` 行级着色（奇偶交替/选中态）。
- **9.3 概览面板的表头还是默认不显示**：✅ 已修复。`DarkMode_Explorer` 主题使 SysHeader32 表头暗色可见，`LVS_EX_DOUBLEBUFFER` 消除闪烁。
- **9.4 按钮都是亮色主题**：✅ 已修复。`src/ui/button.rs` 自绘圆角按钮（`BS_OWNERDRAW` + `WM_DRAWITEM`），accent/secondary 两档样式，hover/pressed 状态。
- **9.5 侧边进度条还是亮色主题**：✅ 已修复（尽力而为）。`DarkMode_Explorer` 主题使 ListView 滚动条暗化；EDIT 滚动条随 manifest+v6 大部分生效。
- **9.6 设置页面和标记页面的布局无法跟随窗口缩放变化**：✅ 已修复。popup/settings 加 `WM_GETMINMAXINFO` 固定尺寸（防缩放错乱）；panel 修 `WM_SIZE` 的 `SWP_NOMOVE` bug + 布局节奏；全部坐标经 `dp()` DPI 缩放。
- **9.7 黑色主题背景过黑，亮色字体太亮**：✅ 已修复。`dark_colors()` 调整为灰黑档（bg `#202020`、fg `#E6E6E6`、edit/tooltip `#2F2F2F`），对比柔和。
- **9.8 按钮/表格/输入框样式过时（Win7 以前风格）**：✅ 已修复。comctl32 v6 manifest + 自绘圆角按钮 + 扩展调色板 + DarkMode_Explorer，整体现代化（扁平、圆角、现代配色）。

### 10. 标记窗口页面中，标题、备注被挤成纵向排列

- **状态**：✅ 已修复（D11：popup 布局重排 + DPI 缩放 + 固定尺寸）
- **现象**：标记弹窗中标题输入框与备注输入框纵向挤在一起，布局拥挤。
- **修复**：标题行改为标签+输入框同排；窗口/进程信息合并为一行 muted 小字；备注多行框占主体；按钮右下角 accent/secondary 自绘；坐标全部经 `dp()` DPI 缩放；`WM_GETMINMAXINFO` 固定尺寸（防 9.6 缩放错乱）。

## 二、需求规划（未实现）

### R1. 自定义快捷键

- 允许用户自定义全局快捷键（当前 `Ctrl+Shift+N` / `Ctrl+Shift+M` / `Ctrl+Shift+S` 硬编码于 `src/hotkey.rs`）。
- **进展**：配置表结构已预留（`src/core/settings.rs` 的 `Settings` 模型，见 [decision-records.md D10](./decision-records.md)），设置页热键编辑 UI 待后续。

### R2. 标记图标改为三角形

- **状态**：✅ 已完成（D11）
- 角标（覆盖层圆点）改为**左上角三角形**，与标记语义对应（当前为圆形）。
- **实现**：`src/sys/badge.rs` 软件光栅圆边三角形（SDF 抗锯齿 + 1px 描边），`overlay.rs` 改 `UpdateLayeredWindow(ULW_ALPHA)` 逐像素 alpha 提交。

### R3.（未来）同一应用多标签页标记

- 支持对一个应用中的不同标签页分别标记、配置（当前以顶层窗口 HWND 为唯一键，无法区分同窗口内的标签页）。

### R4. Manager 改为可贴边隐藏的托盘窗口

- Manager（概览面板）使用独立表格窗口；期望配置成**可贴边隐藏的托盘窗口**：
  - 默认藏在屏幕侧边，可展开；
  - 在窗口中编辑、定位程序。

### R5. 角标可点击编辑

- 点击角标（覆盖层圆点）可直接进入标记编辑（当前点击穿透 `HTTRANSPARENT`，仅悬停显示 tooltip）。

### R6. 设置开关：角标上显示标题内容

- 在设置页增加开关，控制角标（覆盖层）上是否直接显示标签标题文字（当前角标仅一个色块）。
- **进展**：设置模型 `Settings` 已预留扩展（可加 `show_title_on_badge: bool` 字段，见 `src/core/settings.rs` 约 56 行），UI 与绘制待实现。

### R7. Tab 键在标题/备注输入框间切换焦点

- 标记弹窗中输入时可用 Tab 在标题与备注编辑框间切换焦点。
- **进展**：未实现。弹窗未走 `IsDialogMessage`（对话框键盘导航），Tab 默认不切换焦点；需手动处理 `WM_KEYDOWN` 的 `VK_TAB` 或子类化编辑框转发。

### R8. 备注框回车保存

- 备注输入框中按下回车也应保存（与标题框回车保存一致）。
- **进展**：未实现。注意与 R10（Shift+Enter 换行）联动：回车=保存、Shift+Enter=换行，需统一键盘语义。

### R9.（未来）备注支持 Markdown 语法

- 备注内容支持 Markdown 渲染（未来版本），当前为纯文本。

### R10. 备注中 Shift+Enter 换行而非回车

- 备注输入框默认回车键用于保存（见 R8），换行改用 Shift+Enter。
- **进展**：未实现。备注框是多行 EDIT（`ES_MULTILINE`），回车直接换行；需在子类化/键盘处理中区分 Shift+Enter 与 Enter。

## 三、更新记录

| 日期 | 内容 | 关联提交 |
| :--- | :--- | :--- |
| 2026-08-21 | 实测反馈登记：问题 7-10 与需求 R6-R10 追加（含问题 2 修复未生效更正） | —（文档登记，无代码提交） |
| 2026-08-21 | 问题 2（面板表头默认不显示）修复，表头默认可见并支持暗色主题 | `5fa787c` fix(panel) |
| 2026-08-21 | 问题 5.1（弹窗焦点不在编辑框）修复，弹窗激活并聚焦标题编辑框 | `b5d80df` fix(popup) |
| 2026-08-21 | 问题 5.2（回车不能保存）修复，回车与确认按钮语义一致 | `b5d80df` fix(popup) |
| 2026-08-21 | R1（自定义快捷键）配置表结构预留，设置页热键编辑 UI 待后续 | `170ce92` feat(settings) |
| 2026-08-29 | UI 全面现代化（D11）：comctl32 v6 manifest + 全局字体 + DPI 缩放 + 扩展调色板 + DarkMode_Explorer + 自绘圆角按钮 + 三窗口布局重排 + 覆盖层 UpdateLayeredWindow + 角标圆边三角形 + tooltip 圆角分层。关闭问题 2/4/7/9.1-9.8/10，完成 R2 | `feat(ui)` D11 |
