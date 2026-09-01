# 问题与需求记录

> 记录当前已确认的问题与待规划的需求，供后续开发排期参考。
> 建立日期：2026-08-20。当前处于**快速开发阶段，不承诺向后兼容**（见 [decision-records.md D9](./decision-records.md)），
> 以下问题与需求可能随开发推进随时调整或关闭。

## 一、已确认问题

> 状态说明：问题 5.1、5.2 已修复（见文末[更新记录](#三更新记录)）；问题 2 曾修复但实测未完全生效，**重新开放**（见问题 9.3）；其余问题仍在开放状态。
> 2026-08-21 实测反馈：追加问题 7-10（角标形状/可点击、主题 8 项、弹窗布局）与需求 R6-R10，见文末[更新记录](#三更新记录)。
> 2026-08-31 规划：新增需求 R18（托盘）、R19（分组/工作区）、R20（图表），并登记问题 18（标签不跟随移动）与问题 19-22（概览面板四项），见[更新记录](#三更新记录)。规划已产出于 `doc/`，归档于文末"四、扩展规划"。**注意：分组为纯会话存储（不持久化），仅 UI 偏好（含热键绑定、面板停靠偏好）持久化**，见 [decision-records.md D21](./decision-records.md)。

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
  `SetConsoleCtrlHandler` 优雅退出路径；WinEvent 钩子虽由 Drop 注销，但进程仍以非 0 退出码结束。
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
- **9.6 设置页面和标记页面的布局无法跟随窗口缩放变化**：✅ 已修复。popup/settings 加 `WM_GETMINMAXINFO` 固定尺寸（防缩放错乱）；panel 修 `WM_SIZE` 的 `SWP_NOMOVE` 缺陷 + 布局节奏；全部坐标经 `dp()` DPI 缩放。
- **9.7 黑色主题背景过黑，亮色字体太亮**：✅ 已修复。`dark_colors()` 调整为灰黑档（bg `#202020`、fg `#E6E6E6`、edit/tooltip `#2F2F2F`），对比柔和。
- **9.8 按钮/表格/输入框样式过时（Win7 以前风格）**：✅ 已修复。comctl32 v6 manifest + 自绘圆角按钮 + 扩展调色板 + DarkMode_Explorer，整体现代化（扁平、圆角、现代配色）。

### 10. 标记窗口页面中，标题、备注被挤成纵向排列

- **状态**：✅ 已修复（D11：popup 布局重排 + DPI 缩放 + 固定尺寸）
- **现象**：标记弹窗中标题输入框与备注输入框纵向挤在一起，布局拥挤。
- **修复**：标题行改为标签+输入框同排；窗口/进程信息合并为一行 muted 小字；备注多行框占主体；按钮右下角 accent/secondary 自绘；坐标全部经 `dp()` DPI 缩放；`WM_GETMINMAXINFO` 固定尺寸（防 9.6 缩放错乱）。

## 二、需求规划（未实现）

### R1. 自定义快捷键

- 允许用户自定义全局快捷键（当前 `Ctrl+Shift+N` / `Ctrl+Shift+M` / `Ctrl+Shift+S` 硬编码于 `src/hotkey.rs`）。
- **进展**：配置表结构已预留（`src/core/settings.rs` 的 `Settings` 模型，见 [decision-records.md D10](./decision-records.md)）；规划已确定方案（见 D21、D23）：`Settings` 新增 `#[serde(default)] pub hotkeys: HotkeyMap`（镜像 `show_badge_title` 的缺省回退，避免旧配置整体失效），设置页新增"快捷键"分页录制 + 冲突检测，保存后经 `WM_APP_HOTKEYS_CHANGED`（`WM_APP+9`）广播触发 `UnregisterHotKey` + 重注册热更新。
- **规划**：`src/core/hotkey_config.rs`（纯 serde 数据模型）、`src/hotkey.rs` 注册改读全局配置、`src/ui/settings.rs` 快捷键分页。见 D23。

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
- **进展**：规划已确定方案（见 D21）：`Settings` 新增 `#[serde(default)] pub panel_dock: DockPref`（`None`/`Left`/`Right`，默认 `Left`）；停靠时为屏幕边缘细条（`MonitorFromPoint`+`GetMonitorInfoW` 取工作区，全高），光标靠近边缘（约 4px 热区，`WM_TIMER` 轮询探测）自动弹出（`WS_EX_TOPMOST|WS_EX_TOOLWINDOW`），`TrackMouseEvent(TME_LEAVE)` 离开自动收起。停靠偏好持久化到配置（UI 偏好，D10 授权范围）。

### R5. 角标可点击编辑

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D16](./decision-records.md)）
- 点击角标/标题条直接进入标记编辑（预填已有标题/备注/颜色）。
- **实现**：覆盖层 `WM_NCHITTEST` 已命中区（角标三角形 + 标题条）返回 HTCLIENT，`WM_LBUTTONDOWN` 经注入的隐藏窗口发送 `WM_APP_EDIT_TAG`（sys 层不反向依赖 ui，镜像 `set_tag_store` 注入模式），主线程校验目标存活且有标签后打开预填编辑弹窗。

### R6. 设置开关：角标上显示标题内容

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D12](./decision-records.md)）
- 在设置页增加开关，控制角标（覆盖层）上是否直接显示标签标题文字。
- **实现**：`Settings.show_badge_title: bool`（默认开，旧配置缺字段经 `serde(default)` 回退显示）+ 设置页"角标显示标题"复选框；覆盖层经 `set_show_title` 注入开关（sys 层不读 core，镜像 `set_tooltip_theme` 注入模式），标题条为角标右侧圆角胶囊（超 5 字省略号截断），开关关闭或无标签时只画角标；`reapply_theme` 统一重注入并强制覆盖层重绘，保存后即时生效。

### R11. 悬停标题条显示完整标题与备注

- **状态**：✅ 已完成（2026-08-30，与 R6 同批）
- 标题条区域可悬停（`WM_NCHITTEST` 命中即 HTCLIENT），悬停触发既有自绘 tooltip 机制，显示**完整**标题与备注（非 5 字截断版）。

### R12. 概览面板改为可展开的树形列表

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D12](./decision-records.md)）
- 原四列报表（标题/备注/窗口/进程）改为 SysTreeView32 可展开列表：每个标签一个根项，点击行首 `[+]` 展开显示备注/窗口/进程三行详情；搜索过滤行为不变。
- **实现**：`refresh_tree` 重建树（根项 `lParam` = 目标窗口句柄，与原 ListView 行 lParam 方案一致）；配色改 `TVM_SETBKCOLOR/SETTEXTCOLOR/SETLINECOLOR`，暗色沿用 `DarkMode_Explorer`。

### R13. 面板内点击置前对应软件

- **状态**：✅ 已完成（2026-08-30，与 R12 同批）
- 单击/双击树形列表根项即把对应窗口置前（临时置顶：最小化先恢复，`SetForegroundWindow` + `HWND_TOP`，切走后不驻留最上层）；命中 `[+]` 展开按钮时不触发置前；目标窗口已关闭时自动清理标签并刷新。

### R15. 面板展开显示完整多行备注

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D15](./decision-records.md)）
- 旧树形结构把备注压成一行子项，多行备注被截断无法完整显示。重构为：根项一行显示"标题 | 窗口名称"，展开后"备注："标签行 + 备注完整内容——TreeView 项为单行，多行备注逐行拆为独立子项；空备注显示占位"（无）"。搜索仍匹配标题/备注/窗口/进程字段。

### R16. 标签颜色可选

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D16](./decision-records.md)）
- 标记弹窗增加颜色行（橙/蓝/绿/红/紫 5 个色块，编辑时预选原颜色），角标/标题条/悬停便签/面板按所选颜色渲染（渲染管线本就按 `tag.color` 取色）。保存逻辑不再固定橙色。

### R17. 面板右键菜单与 Esc 关闭

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D16](./decision-records.md)）
- 树形列表根项右键弹出菜单：置前窗口 / 编辑标签（经 `WM_APP_EDIT_TAG` 打开预填弹窗）/ 移除标签（清存储 + 销毁覆盖层 + 刷新）；`TPM_RETURNCMD` 直接取回选择。
- Esc 关闭面板：搜索框子类化转发 `VK_ESCAPE` 到父窗口统一处理（此前 Esc 被标准 EDIT 类过程吞掉）。

### R18. 系统托盘图标（替代纯命令行常驻）

- 程序常驻系统托盘（`Shell_NotifyIcon`），提供右键菜单：打开设置页 / 打开概览面板 / 退出。
- **规划**（见 D22）：新增 `src/sys/tray.rs`（sys 层，用 `TrayCommand` 枚举处理命令、`main` 接 ui，保持 `ui→core→sys` 依赖方向）；`common::WM_APP_TRAY`（`WM_APP+8`）作托盘回调消息；左键单击打开设置页（复用 `WM_APP_OPEN_SETTINGS` 链路），右键 `TrackPopupMenu`。v1 用系统图标 `LoadIconW(None, IDI_WINLOGO)`（零新增资源），真实 `.ico` 资源随后补（`build.rs` winresource 加 `.icon`）。图标所有权：`Shell` 不接管 `hIcon`，自加载的须在 `NIM_DELETE` 后 `DestroyIcon`，系统图标不销毁。
- **已知限制**：Explorer 崩溃/重启会吞掉托盘图标——用 `RegisterWindowMessageW("TaskbarCreated")` 广播时重新 `NIM_ADD`（约 15 行，v1 先实现，若失败登记为已知限制）。

### R19. 标签分组 / 工作区

- 为标签添加分组选项，支持按分组批量置顶，形成"工作区"。
- **UI（已确认）**：分组控件置于**新建标签弹窗中标题输入框下方**，允许**直接输入或从已有分组下拉选择**；编辑已有标签时预填原分组。
- **数据模型**（见 D23）：`src/core/tag.rs` 的 `Tag` 新增 `pub group: String`（空串 = 未分组）。**纯会话存储，不持久化**（符合 D9 纯会话原则，仅 UI 偏好持久化）——分组名与标签一起随进程退出清空。
- **面板聚类**（见 D23）：`refresh_tree` 按分组聚类——组头根项（`lParam=0` 标记）→ 标签项（`lParam`=目标 HWND）→ 备注子项；组名排序确定（未分组默认置末）。组头右键菜单"置前整个分组"批量恢复 + 置前（遍历 `OVERLAY_STORE` 调 `sync_position`+`refresh`，复用现成批量入口 `main.rs` reapply_theme 段）。

### R20. 统计图表

- 在程序中增加统计图表视图。
- **默认假设（待确认）**：按分组的已标记窗口数**柱状图**（与 R19 分组契合，零新增数据采集）；备选：按颜色/进程统计、或时间线（需给 `Tag` 加 `created_at` 字段）。
- **规划**（见 D23）：新增 `src/ui/charts.rs` 独立窗口；GDI 柱状图（纯函数 `bar_layout()` 可单测的 `FillRect`/`PatBlt` 布局）；刷新链镜像面板（`WM_APP_TAGS_CHANGED` 经主线程仅可见时转发）；从托盘菜单"统计"入口打开。
- **开放**：图表形态待用户拍板（柱状图 / 时间线）。

### R14. 概览面板默认纵向长方形

- **状态**：✅ 已完成（2026-08-30，与 D14 同批）
- 面板初始尺寸 640×480（横向）改为 400×640（纵向），符合标签列表"窄而高"的使用形态；仍可自由缩放（最小 300×360）。

### R7. Tab 键在标题/备注输入框间切换焦点

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D14](./decision-records.md)）
- 标记弹窗中输入时可用 Tab 在标题与备注编辑框间切换焦点。
- **实现**：编辑框与自绘按钮均经子类化拦截 `VK_TAB` 转发父弹窗，弹窗 `WM_KEYDOWN` 按"标题 → 备注 → 确认 → 取消"循环 `SetFocus`（Shift+Tab 反向）；不引入 `IsDialogMessage`，与既有回车/ESC 子类化转发机制同构。

### R8. 备注框回车保存

- **状态**：✅ 已完成（2026-08-30，见 [decision-records.md D14](./decision-records.md)）
- 备注输入框中按下回车也应保存（与标题框回车保存一致）。
- **实现**：子类化过程中备注框裸回车与标题框回车同路径转发父弹窗保存；与 R10 联动——回车=保存、Shift+Enter=换行。

### R9.（未来）备注支持 Markdown 语法

- 备注内容支持 Markdown 渲染（未来版本），当前为纯文本。

### R10. 备注中 Shift+Enter 换行而非回车

- **状态**：✅ 已完成（2026-08-30，与 R8 同批）
- 备注输入框默认回车键用于保存（见 R8），换行改用 Shift+Enter。
- **实现**：子类化过程中备注框 Shift+回车不拦截，透传多行 EDIT 类过程插入换行；裸回车转发父弹窗保存。

### 11. 弹窗按钮被备注输入框遮挡

- **状态**：✅ 已修复（2026-08-30，D14）
- 备注编辑框高度计算漏减标签行高（`note_h` 未扣除 `ctrl_h + 4`），框体下探约 30px 盖住按钮行；顺带修正弹窗布局混用未缩放 `WIN_W` 的 DPI 隐患（非 100% 缩放下控件溢出窗口）。

### 12. 角标默认不显示，设置开关应用后才出现

- **状态**：✅ 已修复（2026-08-30，D14）
- 新标注窗口的角标不显示，进入设置切换"角标显示标题"并保存后才出现。根因：创建瞬间的同步首绘（`UpdateLayeredWindow`）内容未生效，只有设置保存广播触发的 `reapply_theme → refresh()` 重绘才补上；且 `sync_position` 位置去重早退导致 z 序被压后无法重申置顶。修复：首绘异步化 + 创建后强制重绘 + 每次同步重申 `HWND_TOPMOST` + ULW 失败日志。

### 13. 多次点击同一角标会叠出多个编辑弹窗

- **状态**：✅ 已修复（2026-08-30，D17）
- 新增活动弹窗注册表：同一时间至多一个弹窗——同目标请求复用并置前聚焦，异目标请求替换旧弹窗。

### 14. 悬停角标时鼠标光标消失（间歇性）

- **状态**：✅ 已修复（2026-08-30，D17）
- 根因：自定义窗口类 `hCursor` 为 NULL，`DefWindowProc` 处理 `WM_SETCURSOR` 时把光标设为 NULL（即隐藏）；时序依赖导致间歇出现。5 个自注册窗口类统一设置标准箭头光标。

### 15. 标签颜色黄蓝互换

- **状态**：✅ 已修复（2026-08-30，D17）
- 根因：覆盖层光栅函数按 RGBA 写像素，而 `UpdateLayeredWindow` 的 32bpp 位图内存布局为 **BGRA**——R/B 互换使橙色显示为浅蓝、蓝色显示为橙黄（描边/标题条用灰色 R≈B 故未暴露）。三个渲染函数统一改 BGRA 输出并新增字节序单测。

### 16. 设置窗口下拉框配色不跟随主题（白底黑字 / 深底黑字）

- **状态**：✅ 已修复（2026-08-30，D17）
- 根因：下拉列表的 `WM_CTLCOLORLISTBOX`/`WM_CTLCOLORSTATIC` 发给 **COMBOBOX 自己**而非父窗口，父窗口配色分支收不到。下拉框子类化拦截配色消息按主题着色；同时下拉框/复选框/编辑框应用 `DarkMode_Explorer` comctl32 变体。

### 17. 悬停角标时备注显示不完整

- **状态**：⏳ 待修复
- 现象：悬停角标/标题条时便签 tooltip 无法完整显示备注内容。已做部分修复：tooltip 文本动态长度读取（不再 512 字符截断）+ 光标所在显示器工作区钳制（底部放不下翻到光标上方）+ 标题尾部 CR 清理；待用户复测确认现象后继续排查。

### 18. 窗口移动时标签无法跟随移动

- **状态**：🏗️ 规划中（见 [decision-records.md D21](./decision-records.md)）
- 现象：目标窗口移动/拖动时，左上角角标/标题条没有跟随目标平移（或明显滞后）。
- **已定位缺口（按优先级）**：
  1. **提权目标 UIPI 拦截事件**（已登记遗留 F4）：`WINEVENT_OUTOFCONTEXT` 钩子收不到提权进程事件 → 事件路径静默，`z` 序也受限（`GetWindowRect`/`IsWindow` 读取不受 UIPI 阻，轮询可兜底）；
  2. **全局单槽合并错杀**（`win_event.rs:187-200`）：事件风暴期任意 pending `WM_APP_WINEVENT` 会跳过新事件投递，可能滞后目标窗口事件（但 sync 时重读当前 rect 天然收敛，非永久卡死）；
  3. **MOVESIZESTART/END 未利用**（`win_event.rs:60-73`）：`0x000A`/`0x000B` 已在钩子范围但被 `classify` 忽略——无"移动结束最终同步"；
  4. **DWM extended frame bounds 拖拽中可能陈旧**（`overlay.rs:347-355` 优先用之，回退仅限失败非陈旧）；
  5. **静默失败无重试**（`overlay.rs:364-366` w/h≤0 早退、`:427-437` SetWindowPos 失败静默）。
- **规划修复**：`MOVESIZESEND`→新增 `MoveEnd` 动作 → `sync_position_force()`（优先 `GetWindowRect` 避陈旧 DWM）；`MOVESIZESTART`→加速轮询 500→100ms，`MoveEnd` 恢复；非静默日志 + `needs_resync` 标志由 `poll_overlays` 消化。

### 19. 概览面板无默认展开详情

- **状态**：🏗️ 规划中（见 D21）
- 现象：面板树形列表打开时节点默认全部折叠，用户需逐项展开看到备注。
- **规划修复**：`refresh_tree` 快照由"仅展开"泛化为 `(expanded, collapsed)` 集合；纯函数 `root_expand_default()`——新根默认展开、用户折叠过的保持折叠、刷新（`WM_APP_TAGS_CHANGED`）不打断。

### 20. 概览面板无一键展开/收起

- **状态**：🏗️ 规划中（见 D21）
- **规划修复**：面板加"全部展开/全部收起"两个按钮，复用 `refresh_tree` 兄弟链遍历骨架（`panel.rs:983-1019`）。

### 21. 概览面板不能靠边隐藏

- **状态**：🏗️ 规划中（见 D21，对应 R4）
- 现象：面板只能显式开关，不能靠边隐藏、鼠标靠近侧边自动弹出。
- **规划修复**：`Settings.panel_dock`（默认 `Left`）；停靠为屏幕边缘细条，光标靠近边缘自动弹出，`TrackMouseEvent` 离开自动收起。

### 22. 概览面板无法纯键盘操作

- **状态**：🏗️ 规划中（见 D21）
- 现象：面板不能纯键盘操作；Tab/Shift+Tab 无法在控件间选择、回车无法显示选中项。
- **规划修复**：子类化树（镜像 `search_edit_subclass_proc`）转发 Tab/回车；面板 `WM_KEYDOWN` 实现 Tab 循环（复用 `focus_next_control`）+ 回车 `TVGN_CARET`→`activate_target`；`toggle_panel` 显示后补 `SetFocus`。


## 三、更新记录

| 日期 | 内容 | 关联提交 |
| :--- | :--- | :--- |
| 2026-08-21 | 实测反馈登记：问题 7-10 与需求 R6-R10 追加（含问题 2 修复未生效更正） | —（文档登记，无代码提交） |
| 2026-08-21 | 问题 2（面板表头默认不显示）修复，表头默认可见并支持暗色主题 | `5fa787c` fix(panel) |
| 2026-08-21 | 问题 5.1（弹窗焦点不在编辑框）修复，弹窗激活并聚焦标题编辑框 | `b5d80df` fix(popup) |
| 2026-08-21 | 问题 5.2（回车不能保存）修复，回车与确认按钮语义一致 | `b5d80df` fix(popup) |
| 2026-08-21 | R1（自定义快捷键）配置表结构预留，设置页热键编辑 UI 待后续 | `170ce92` feat(settings) |
| 2026-08-29 | UI 全面现代化（D11）：comctl32 v6 manifest + 全局字体 + DPI 缩放 + 扩展调色板 + DarkMode_Explorer + 自绘圆角按钮 + 三窗口布局重排 + 覆盖层 UpdateLayeredWindow + 角标圆边三角形 + tooltip 圆角分层。关闭问题 2/4/7/9.1-9.8/10，完成 R2 | `feat(ui)` D11 |
| 2026-08-30 | 覆盖层标题条（R6/R11）+ 概览面板树形化（R12）+ 单击置前（R13），见 D12；顺带修复重复标注同窗口不刷新、主题切换后覆盖层不重绘两处遗留 | 本轮改动（未提交时待填） |
| 2026-08-30 | 弹窗键盘语义补全（R7/R8/R10：Tab 循环、备注回车保存、Shift+回车换行）+ 按钮遮挡修复（问题 11）+ 面板默认纵向（R14）+ 角标首绘/置顶修复（问题 12），见 D14 | 本轮改动（未提交时待填） |
| 2026-08-30 | 面板树形重构（R15）：根项"标题 \| 窗口名称"同行，展开显示完整多行备注 | 本轮改动（未提交时待填） |
| 2026-08-30 | 标注流程闭环（D16）：角标单击编辑（R5）+ 弹窗颜色选择（R16）+ 面板右键菜单与 Esc 关闭（R17）+ 标题/备注动态长度读取修复静默截断 | 本轮改动（未提交时待填） |
| 2026-08-30 | D17：标签颜色字节序修复（问题 15）+ 统一主题管理器与下拉框子类化（问题 16）+ 光标消失修复（问题 14）+ 弹窗单例/光标附近定位（问题 13）+ tooltip 定位钳制；问题 17（悬停备注不完整）待修复 | 本轮改动（未提交时待填） |
| 2026-08-31 | 扩展规划：新增需求 R18（托盘）、R19（分组/工作区）、R20（图表），登记问题 18-22；确认**分组纯会话不持久化**、**v1 系统图标可接受**、**分组控件置于新建标签弹窗标题下方（可输入或选择已有分组）**；规划归档见下文"四、扩展规划"与 [decision-records.md D21-D23](./decision-records.md) | —（文档规划，无代码提交） |
| 2026-09-01 | 托盘常驻化（D24）：Windows 子系统 + 单实例保护（守护窗口类）+ 默认托盘常驻零窗口启动 + 启动气泡可开关（show_balloon）+ `--no-tray` 禁用 + 有标签退出弹定制主题确认窗（ui/confirm）+ 托盘右键四项菜单/左键与气泡打开面板 + 面板底部"退出"按钮 + 设置页"气泡提示"开关 | `425303d` feat(tray) |
| 2026-09-01 | 底部控件客户区裁切修复（D25）：`ui::layout` 新增 `TITLEBAR_H`/`client_height()` 统一从窗口外高抵扣标题栏边框；`confirm`/`popup`/`settings` 底部按钮行改用客户区高度定位，修复退出确认窗"退出/取消"按钮溢出客户区被裁切 | `425303d` fix(ui) |
| 2026-09-01 | 托盘底层迁移（D26）：托盘自 `Shell_NotifyIconW` 手写迁至 `tray-icon`(tauri)（`TrayIconBuilder`+`Menu(MenuItem)`+嵌入资源图标，事件经 `TrayIconEvent`/`MenuEvent` crossbeam 通道由主循环 `try_recv` 轮询）；气泡由 `NIF_INFO`+`NIM_MODIFY` 迁至 `notify-rust`（Windows TOAST）；`TaskbarCreated` 重注册由 tray-icon 内部接管、删除 `WM_APP_TRAY` 常量；图标取自新增 `assets/icon.ico`（winresource ID=1，`Icon::from_resource`） | —（并行迁移，见 decision-records D26） |
| 2026-09-01 | 图形界面重构登记（D27）：四个图形界面窗口（panel/popup/settings/confirm）迁至 iced（tiny-skia 软件渲染器 + 独立线程通道通信，含总线最小化：`NativePrefs`/`NativeBridge` 收敛 + `pump_background_events` 单一分发表 + 托盘不绕 iced）、`ui/button`/`ui/layout` 删除、`ui/theme` 拆分；覆盖层/托盘/热键/事件监听与 Win32 消息泵保持纯 Win32 不变（详见 [decision-records.md D27](./decision-records.md)，分阶段执行方案见 [iced-migration.md](./iced-migration.md)） | ✅ 已实施（G0-G5）：`feat(ui)` D27 G0-G4 + `docs(ui)` G5 |

---

## 四、扩展规划（2026-08-31 调研产出，未实施）

### 调研来源
本规划由只读调研（3 路探索 + Oracle 架构评审）产出，未经任何代码修改。核心裁决见 [decision-records.md D21]-[D23](./decision-records.md)。

### 已确认决策（用户拍板）
| 决策点 | 结论 |
| :--- | :--- |
| 分组持久化边界 | **纯会话存储**——分组名挂在 `Tag.group` 上，随进程退出清空；仅 UI 偏好（含热键绑定、面板停靠偏好）持久化（D9/D10） |
| 托盘图标资源 | **v1 用系统图标** `LoadIconW(None, IDI_WINLOGO)`（零新增资源），真实 `.ico` 随后补 |
| 分组控件 UI | 置于**新建标签弹窗中标题输入框下方**，允许**直接输入或从已有分组下拉选择**；编辑时预填原分组 |

### 分阶段路线图
| 阶段 | 主题 | 独立交付价值 | 覆盖 |
| :--- | :--- | :--- | :--- |
| P1 快速 | 稳定 + 地基 | 修好日常缺陷（标签跟随移动）；面板变可用；配置路径链 | 问题 18-22、R1 地基 |
| P2 短期 | 托盘 + 可配置 | 托盘常驻；教程可发现；自定义快捷键；面板贴边 | R1/R4/R18、问题 21 |
| P3 中期 | 工作区 + 洞察 | 分组形成工作区（批量置顶）；统计图表 | R19/R20 |

### 关键架构对齐
- 新消息常量（`common/mod.rs`，WM_APP+1..7 已占）：`WM_APP+8`=`WM_APP_TRAY`（托盘回调）、`WM_APP+9`=`WM_APP_HOTKEYS_CHANGED`（热键重注册广播）、`WM_APP+10`=`WM_APP_BATCH_ACTIVATE`（分组批量置前）。
- 数据模型：`Settings` 增 `#[serde(default)] pub hotkeys: HotkeyMap` + `#[serde(default)] pub panel_dock: DockPref`；`Tag` 增 `pub group: String`。所有新字段必须带 `#[serde(default)]`（否则旧配置整体回退默认）。
- 托盘方向：`sys::tray.rs`（用 `TrayCommand` 枚举，`main` 接 ui）保持 `ui→core→sys`；左键→复用 `WM_APP_OPEN_SETTINGS`。
- 配置路径：`--config-dir` CLI > `WINTAG_CONFIG_DIR` env > `<exe_dir>/config`（仅当存在或可写）> `%APPDATA%`，`OnceLock` memoize，读穿透旧 `%APPDATA%`（D9 不复制）。

