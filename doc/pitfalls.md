# 踩坑记录

开发过程中实际遇到的问题、排查过程与坑点。与 [decision-records.md](./decision-records.md)（决策与设计取舍）、[issues-and-requirements.md](./issues-and-requirements.md)（用户问题登记）互补，本文聚焦"为什么会被坑、下次如何避免"。

## 2026-08-30（D14/D15 开发轮次）

### 1. 角标默认不显示，设置开关应用后才出现（问题 12）

**现象**：新标注的窗口角标完全不见；进入设置页切换"角标显示标题"并保存后，角标立刻出现。

**排查思路**：两种状态路径的差异只有一处——设置保存会触发 `reapply_theme → overlay.refresh()`（`InvalidateRect + UpdateWindow` 强制重走一次 `UpdateLayeredWindow`），而首次显示只有 `Overlay::create` 里 `ShowWindow + UpdateWindow` 的同步绘制。既然"事后重绘能救回来"，说明**创建瞬间的同步 `UpdateLayeredWindow` 内容没有生效**。这个"哪条路径能修复，差异就是根因"的对比法，比逐行审查渲染代码快得多。

**坑 1：创建调用栈内的同步首绘不可靠。** `CreateWindowExW` 之后立刻 `ShowWindow + UpdateWindow`，`WM_PAINT` 在窗口尚未完全完成显示/composite 的调用栈内同步触发，首次 ULW 提交的内容可能被丢弃。修复：首绘异步化（只 `InvalidateRect`，让 `WM_PAINT` 在消息循环空闲时自然触发）+ 创建后立即 `refresh()` 双保险。

**坑 2：位置去重早退破坏了 z 序不变量。** `sync_position` 原先"位置没变就 early return"省掉 `SetWindowPos`——但这次调用同时承担**重申 `HWND_TOPMOST`** 的职责。目标窗口被激活压住覆盖层后（同属 topmost 带时后激活者在前），位置不再变化 → 永不重申置顶 → 角标永久消失。

**教训**：去重优化只对"纯位置写"安全；带副作用的幂等操作（重申 z 序）不能参与去重，除非把副作用单独剥离出来。另外渲染 API 失败别静默——现在 ULW 失败会打日志，同类问题下次直接看输出。

### 2. 弹窗按钮被备注输入框遮挡（问题 11）

**现象**：备注多行编辑框下探约 30px，盖住右下角"确认/取消"按钮。

**根因**：`note_h = btn_row_y - btn_gap - note_row_y` 漏减了备注标签行高（`ctrl_h + 4` 间距），编辑框顶边其实在 `note_row_y + ctrl_h + 4` 而非 `note_row_y`。

**连带坑：DPI 基准混算。** 排查时发现控件 x/宽度用的是未缩放的设计常量 `WIN_W`(420)，而边距/高度用了 `dp()` 缩放值。100% DPI 下两者相等掩盖了问题；非 100% 缩放下控件会溢出窗口右缘（窗口本身被 `WM_GETMINMAXINFO` 锁定为 `dp(420)`）。

**教训**：同一窗口内所有布局坐标必须同基准——要么全部 `dp()`，要么全部原始值；"在 100% DPI 下看起来正常"不能作为布局正确的证据。

### 3. 键盘处理的三个坑（R7/R8/R10）

**坑 1：非对话框窗口里 Tab 键会被控件类过程吞掉。** 项目不走 `IsDialogMessageW`，标准 EDIT 类过程对 `VK_TAB` 直接吞掉不外传，焦点永远卡在标题框。解法沿用项目既有的"子类化转发"机制（`SetWindowLongPtrW(GWLP_WNDPROC)` 拦截 `WM_KEYDOWN` 转发父窗口）。

**坑 2：不能为了 Tab 引入 `IsDialogMessageW`。** 对话框管理器会接管回车键走"查找默认按钮"（`DM_GETDEFID`）语义，本项目弹窗不是对话框、没有默认按钮，回车会变成无效或蜂鸣，与"子类化转发回车 = 保存"的既有机制直接冲突。**教训：键盘导航方案必须与既有按键语义一起评估，不能只看 Tab 一个键。**

**坑 3：BUTTON 类过程同样吞 Tab/Esc。** 焦点切到自绘按钮后 Tab 又卡死——按钮子类化过程（原本只跟踪悬停态）也要转发 `VK_TAB`/`VK_ESCAPE`。

**多行回车语义**：备注框裸回车转发保存、Shift+回车透传给 EDIT 插入换行，判定写在子类化过程里（`GetKeyState(VK_SHIFT)`），父窗口 `WM_KEYDOWN` 只消费已转发的键。

### 4. windows-rs 0.58 的 API 坑（编译期）

- **`GetFocus` 在 `Win32::UI::Input::KeyboardAndMouse`**，不在 `WindowsAndMessaging`——凭直觉 import 会 E0432。
- **`UpdateLayeredWindow` 返回 `Result`** 而非 BOOL，与多数 GDI/USER 函数不同；按 BOOL 写 `as_bool()` 会 E0599。
- **`unsafe {}` 块参与二元比较必须加括号**：`unsafe { GetKeyState(..) } >= 0` 语法歧义报"expected expression"，需写成 `(unsafe { .. }) >= 0`。
- **`iter().position()` 返回 `usize`**，混入 `i32` 运算后被迫来回 cast，`clippy -D warnings` 会以 `unnecessary_cast` 拦截。教训：索引运算全程保持 usize。

### 5. SysTreeView32 项是单行的（R15 重构的原因）

备注支持多行（Shift+回车换行）后，旧面板树把备注压成"备注：xxx"一行子项，换行内容被截断。TreeView 项**没有自动换行**，要换行只有 ownerdraw 自绘一条路（复杂度不成比例）。实际采用：多行备注逐行拆成独立子项，视觉上即"备注：标签行 + 内容行"的树形版式。**教训**：选系统控件前先确认其文本渲染边界（单行、无自动换行、宽度裁剪策略），功能需求（多行文本展示）可能直接推翻控件选型。

## 历史坑点索引

更早轮次的坑点（WinEvent 事件风暴合并、覆盖层铺满吞点击、`HTTRANSPARENT` 无法穿透跨进程窗口、WM_CTLCOLORBTN 无法改按钮底色等）分散在 [decision-records.md](./decision-records.md) 各决策的"背景/备选方案"中，此处不重复。
