# 决策记录（Decision Records）

本文件固化 WinTag 项目重大重构（移除 eframe/egui/winit，改用纯 Win32 windows-rs 0.58 手写）过程中的关键决策，供团队后续参考与回溯。每条记录包含**决策**、**背景**、**备选方案**、**理由**、**影响与后续跟进**五个部分。

- 决策日期：2026-08-20
- 关联文档：[架构设计](./architecture.md)、[技术规格](./technical-specs.md)、[需求规格说明](./requirements.md)、[开发计划](./development-plan.md)
- 决策范围：本次重构已完成并定稿的 9 项决策（D1 至 D9），以及本次新增的 D10（暗色主题/圆角与配置持久化）

---

## D1：不引入第三方 crate，纯 windows-rs 0.58 手写

- **决策**：热键、窗口事件监听、窗口管理、覆盖层绘制全部使用 windows-rs 0.58 手写实现，不引入 global-hotkey、win_event_hook、winit、tao、softbuffer 等第三方 crate。`Cargo.toml` 仅保留 `windows`、`anyhow`、`thiserror` 三个依赖。
- **背景**：早期原型基于 eframe/egui，重构时评估过引入成熟 crate 替代各 Win32 封装层的可能性，以换取更少的自有代码量。
- **备选方案**：
  1. 热键改用 `global-hotkey` crate；
  2. 事件监听改用 `win_event_hook` crate；
  3. 窗口与事件循环改用 `winit` 或 `tao`；
  4. 覆盖层绘制改用 `softbuffer`。
- **理由**：
  1. 热键代码体量极小，集中在 `src/hotkey.rs`（含测试共 92 行，生产逻辑约 60 行），已通过 `RegisterHotKey` + `WM_HOTKEY` 完整集成进主消息循环（见 `src/main.rs` 第 110 行起），额外引入 crate 得不偿失；
  2. `global-hotkey` 的回调运行在自身线程，需要额外的跨线程跳转才能回到主线程消息泵，且强制附加 `MOD_NOREPEAT` 修饰，行为不受本项目控制；
  3. `winit`/`tao` 的整窗点击穿透无法表达本项目需要的差异化命中能力：覆盖层要求"圆点区域可点击、其余区域穿透"（区域级 `WM_NCHITTEST` 区分 `HTCLIENT` 与 `HTTRANSPARENT`，见 `src/sys/overlay.rs` 第 350 行起）；
  4. 窗口枚举（`EnumWindows`）等基础能力没有生产级质量的 crate 封装，手写反而更可控；
  5. windows-rs 0.58 已覆盖本项目所需的全部 Win32 API，feature 清单见 `Cargo.toml`。
- **影响与后续跟进**：依赖树最小化，构建与发布简单。代价是全部 Win32 交互代码自维护，后续若需新增 API 需自行在 windows-rs feature 中补齐。关注 windows-rs 版本升级（0.58 之后的事件常量暴露情况），届时可简化 `src/sys/win_event.rs` 中的字面量常量定义。

---

## D2：SetWinEventHook 在主线程注册并转发

- **决策**：窗口事件监听使用 `SetWinEventHook`，固定 `WINEVENT_OUTOFCONTEXT` + `WINEVENT_SKIPOWNPROCESS` 标志，在主线程（隐藏窗口创建之后、消息循环之前）注册。监听拆分为两个钩子：系统事件段 `0x0003..=0x0017` 与对象事件段 `0x8001..=0x8018`（见 `src/sys/win_event.rs` 第 114 行起 `install`）。回调只做过滤与 `PostMessageW(WM_APP_WINEVENT)` 转发，不执行任何重活。
- **背景**：WinEvent 回调的执行线程取决于注册线程与进程上下文。若在独立线程注册，回调将跑在该线程的消息泵内，需要引入跨线程状态（Send/Sync、Arc 传递、生命周期管理）。
- **备选方案**：
  1. 独立线程注册钩子，回调内直接处理或经通道转发；
  2. 主线程注册，回调内只转发消息（采纳）。
- **理由**：
  1. 主线程本就是消息泵，`GetMessageW` 会处理本线程所有窗口消息（见 `src/main.rs` 第 98 行起），回调投递的 `WM_APP_WINEVENT` 自然回到同一线程处理，无需第二个线程，省去 Send/Sync 与生命周期成本；
  2. 覆盖层、面板、弹窗全部在主线程创建与访问，事件处理在主线程落地与现有架构完全一致；
  3. `WINEVENT_SKIPOWNPROCESS` 避免收到本进程（隐藏窗口、覆盖层等）自身产生的事件形成自激循环；
  4. 回调内只做 `should_forward` 过滤（`OBJID_WINDOW` + `CHILDID_SELF`，见 `src/sys/win_event.rs` 第 78 行起）与轻量 `PostMessageW`，不触碰布局/重绘 API，避免阻塞 USER 队列。
- **影响与后续跟进**：事件处理全部收敛到 `src/main.rs` 的 `handle_winevent`，逻辑单一。`WinEventHooks` 实现 `Drop` 自动注销（第 126 行起）。两个钩子全部安装失败时 `degraded` 标志置位，退化到纯轮询模式。后续若需监听更多事件段，需评估系统段与对象段的边界划分是否继续成立。

---

## D3：事件到动作的分类映射

- **决策**：WinEvent 编号经 `classify` 函数（`src/sys/win_event.rs` 第 60 行起）映射为六个语义动作之一：`Sync`（同步位置）、`Hide`（隐藏）、`Show`（显示）、`BringToTop`（置顶）、`Forget`（移除记录）、`Ignore`（忽略）。具体映射：
  - `EVENT_OBJECT_LOCATIONCHANGE` → `Sync`
  - `EVENT_OBJECT_DESTROY` → `Forget`（不查询窗口）
  - `EVENT_SYSTEM_MINIMIZESTART` / `EVENT_OBJECT_HIDE` / `EVENT_OBJECT_CLOAKED` → `Hide`
  - `EVENT_SYSTEM_MINIMIZEEND` / `EVENT_OBJECT_SHOW` / `EVENT_OBJECT_UNCLOAKED` → `Show`
  - `EVENT_SYSTEM_FOREGROUND` → `BringToTop`
  - 其余 → `Ignore`
- **背景**：窗口状态变化事件种类繁多，直接在各分支堆叠 Win32 调用会造成映射逻辑与 UI 动作耦合、难以测试。需要一层与具体 Win32 操作解耦的语义分类。
- **备选方案**：
  1. 回调中按事件编号直接调用覆盖层 API；
  2. 引入中间 `WinEventAction` 枚举，分类与执行分离（采纳）。
- **理由**：
  1. `classify` 为纯函数，不访问全局状态，便于单测（见 `src/sys/win_event.rs` 第 194 行起测试模块）；
  2. 映射依据 komorebi、glazewm、tacky-borders、screenpipe、SPlayer 等开源窗口管理器的生产实践：`DESTROY` 时窗口正在销毁，查询窗口属性无意义且可能失败，直接 `Forget`；最小化/遮蔽/隐藏统一收敛为 `Hide`，恢复统一收敛为 `Show`；
  3. `Forget` 动作在 `src/main.rs` 的 `handle_winevent` 中不经过 `with_overlay` 的覆盖层查找，直接清理标签与覆盖层（第 260 行起），语义清晰。
- **影响与后续跟进**：新增事件只需扩展 `classify` 的 match 分支与 `WinEventAction` 枚举。`classify` 目前不区分"窗口在顶层可见但被遮挡"的细节，若后续需要更细粒度策略（如 UWP 应用的 Cloaked 状态细分），在此处扩展即可。

---

## D4：事件驱动为主 + 定时器兜底轮询的混合同步策略

- **决策**：覆盖层位置同步以 WinEvent 事件驱动为主路径，同时在主消息循环挂载一个 500ms 的 `SetTimer` 兜底轮询（`TIMER_POLL_OVERLAYS = 0x1234`，见 `src/main.rs` 第 21 行与第 90 行起）。轮询逻辑 `poll_overlays`（第 285 行起）遍历覆盖层：用 `IsWindow` 校验目标窗口存活，失效则移除覆盖层并删除标签；存活则调用 `overlay.sync_position()` 同步位置。
- **背景**：WinEvent 事件存在丢失可能（钩子安装失败、进程崩溃、UWP 事件不完整），且最小化窗口的可见性判断容易误判。若完全依赖事件驱动，覆盖层可能停在错误位置或残留为孤儿。
- **备选方案**：
  1. 纯事件驱动，不做轮询；
  2. 纯轮询，放弃事件监听；
  3. 事件驱动为主 + 低频兜底轮询（采纳）。
- **理由**：
  1. tacky-borders 等项目的实践中，纯事件驱动在事件丢失时覆盖层会漂移，需要用轮询兜底；
  2. 事件驱动保证移动/缩放时的即时响应（毫秒级），500ms 轮询负责纠正漏网之鱼，两者成本互补；
  3. `sync_position` 内部带短路守卫（最小化或不可见直接返回）与变更去重（`last_rect` 矩形一致时跳过 `SetWindowPos`），轮询开销被压到最低（见 `src/sys/overlay.rs` 第 201 行起）；
  4. 两个 WinEvent 钩子全部安装失败时 `is_degraded()` 为真，轮询成为唯一同步路径，系统仍可用（见 `src/main.rs` 第 75 行起打印降级提示）。
- **影响与后续跟进**：轮询频率 500ms 为当前选值，覆盖层数量多时可在 `poll_overlays` 中增加"上次同步时间"节流。`stale` 集合在遍历结束后统一移除（第 308 行起），避免迭代期间修改 HashMap。后续接入 `EVENT_OBJECT_LOCATIONCHANGE` 高频率场景时，需评估去重逻辑是否足够。

---

## D5：DPI 感知与覆盖层坐标约定

- **决策**：在 `main()` 开头、创建任何窗口之前调用 `SetProcessDpiAwarenessContext`，优先 Per-Monitor V2，失败降级到 V1（`src/main.rs` 第 35 行起）。不引入 embed-manifest crate 声明 DPI 感知。覆盖层贴目标窗口可见区域使用 `DWMWA_EXTENDED_FRAME_BOUNDS`，失败时回退 `GetWindowRect`（`src/sys/overlay.rs` 第 211 行起）。
- **背景**：Windows 默认按系统 DPI 缩放窗口，覆盖层坐标若不与目标窗口处于同一 DPI 上下文，会出现位置偏移或尺寸错位。此前架构文档提到的高 DPI 处理是待办项，本次重构落地。
- **备选方案**：
  1. 通过 embed-manifest crate 在可执行文件清单中声明 DPI 感知；
  2. 运行时调用 `SetProcessDpiAwarenessContext`（采纳）；
  3. 不声明 DPI 感知，依赖系统位图拉伸（否决）。
- **理由**：
  1. 运行时调用失败时还能降级到 V1，比清单声明多一层容错；避免再引入一个编译期 crate 与 manifest 打包环节；
  2. 在 Per-Monitor V2 感知下，`GetWindowRect` 与 DWM 边界返回的都是物理像素，覆盖层与目标窗口在同一坐标空间，无需额外换算；
  3. `GetWindowRect` 包含隐形 resize 边框，直接对齐会与用户视觉感知不符；`DWMWA_EXTENDED_FRAME_BOUNDS` 返回的是窗口可见区域，与圆点覆盖层的"贴边"需求一致；
  4. windows-rs 0.58 的 `HiDpi` feature 已包含所需 API，无需额外依赖。
- **影响与后续跟进**：覆盖层绘制代码在 V2 感知下以物理像素工作，未涉及 DPI 换算路径。后续若支持多显示器不同缩放比例（PMv2 已天然支持），需验证跨屏拖动时 `sync_position` 的去重逻辑不受影响。

---

## D6：架构修复，sys 层不再依赖 core

- **决策**：消除 sys 层对 core 层的源码级依赖。`src/sys/mod.rs` 通过 `pub(crate) use super::core::tag::TagStore` 以相对路径重导出 `TagStore` 类型别名，仅供编译期类型解析（第 11 行起，附注释说明不产生运行时依赖）。覆盖层对标签存储的访问改为依赖注入：`sys::overlay::set_tag_store(Arc<Mutex<TagStore>>)` 由主线程在启动时注入（`src/main.rs` 第 60 行）。删除 `core::tag::TAG_STORE` 全局静态。
- **背景**：架构约定依赖方向为 `ui → core → sys`，sys 层不得反向依赖 core。重构前 overlay 层需要查询标签内容，若直接写 `crate::core::tag::TagStore` 会破坏依赖方向，也埋下循环引用隐患。
- **备选方案**：
  1. sys 层直接 `use crate::core::tag::TagStore`（否决，违反依赖方向）；
  2. 在 core 层定义全局 `TAG_STORE` 静态供 sys 读取（否决，全局可变状态扩散）；
  3. 类型别名重导出 + 运行时注入（采纳）。
- **理由**：
  1. `pub(crate) use super::core::tag::TagStore` 是路径级别名，仅让 overlay 的 `set_tag_store` 签名能引用该类型，不产生 sys → core 的行为依赖；审计确认 `src/sys/*.rs` 中 `crate::core` 零命中；
  2. `set_tag_store` 把存储所有权交给主线程，覆盖层只需在悬停时查询，未注入则静默（`TAG_STORE_INNER` 为 `Option` 语义，见 `src/sys/overlay.rs` 第 447 行起），解耦彻底；
  3. 删掉 core 层全局 `TAG_STORE` 后，标签数据单一来源为主线程持有的 `Arc<Mutex<TagStore>>`，同一份 Arc 同时注入 overlay 并登记到 `GLOBAL_TAG_STORE`（`src/main.rs` 第 58 行起）。
- **影响与后续跟进**：类型别名放在 `sys/mod.rs` 而非代码文件内，是因为它只服务于 sys 模块内部编译期解析。后续若重构 core 的存储结构，只需同步调整 `sys/mod.rs` 的别名与 `main.rs` 的注入点。审计命令 `Select-String src/sys/*.rs -Pattern "crate::core"` 保持零命中作为回归防线。

---

## D7：共享工具层 src/common/mod.rs

- **决策**：新增 `src/common/mod.rs` 作为系统级共享工具层（叶子模块，不依赖项目内任何模块），集中提供：自定义消息常量 `WM_CREATE_OVERLAY`（`WM_APP+1`）、`WM_DESTROY_OVERLAY`（`WM_APP+2`）、`WM_APP_WINEVENT`（`WM_APP+3`）；`widestring` UTF-16 宽字符串转换；`get_userdata`/`set_userdata` 窗口用户数据读写（基于 `GetWindowLongPtrW`/`SetWindowLongPtrW` 直读直写）。替代原先散布在 4 个文件里的重复实现。
- **背景**：重构前自定义消息常量、宽字符串转换、窗口用户数据读写在多处重复实现；用户数据读写使用"先 `SetWindowLongPtrW(0)` 再恢复原值"的反模式，有窗口消息重入期间的副作用风险。
- **备选方案**：
  1. 保持各文件独立实现（否决，重复代码 + 反模式隐患）；
  2. 收敛到 `common` 叶子模块（采纳）。
- **理由**：
  1. 消息常量语义集中，`main.rs` 的隐藏窗口 WndProc（第 179 行起）、`popup.rs` 的覆盖层创建请求、`win_event.rs` 的事件转发统一引用 `common`，一处修改全链生效；
  2. `GetWindowLongPtrW` 直接读取是无副作用操作，取代先置 0 再恢复的反模式，语义等价且消除了消息重入窗口期读到空指针的窗口期；
  3. 叶子模块无反向依赖风险，任何模块（含 `hotkey`、`sys`、`ui`）均可按需复用，符合依赖方向约定（任意模块 → common）。
- **影响与后续跟进**：审计确认 `src/sys/*.rs` 中已无对 core 的 `crate::` 路径引用，sys 层仅依赖 common 与 windows-rs。后续若新增自定义消息，统一在 `common` 中声明并保持 `WM_APP` 偏移递增；`set_userdata` 的调用方必须遵守"窗口销毁前置空或释放"的约定（已在函数文档注释中写明）。

---

## D8：覆盖层生命周期与弹窗确认解耦

- **决策**：覆盖层创建从"弹窗弹出即预创建"改为"确认（OK）后创建"。`popup.rs` 的 OK 分支在标签写入成功后通过 `PostMessageW(hidden_hwnd, WM_CREATE_OVERLAY)` 请求创建覆盖层（第 370 行起）；取消分支只关闭弹窗，不再涉及覆盖层创建或销毁。覆盖层 `Overlay` 的 `Drop` 实现顺带销毁仍显示的孤儿 tooltip 并清空 userdata（`src/sys/overlay.rs` 第 284 行起）。悬停跟踪状态由原先的 `static TRACKING` 单一全局标志改为 per-overlay 的 `OverlayState { tracking: AtomicBool }`，登记在 `TARGET_MAP` 中（`src/sys/overlay.rs` 第 41 行起、第 51 行起）。
- **背景**：修复前，弹窗一打开就预创建覆盖层，取消操作需要额外销毁，容易残留孤儿覆盖层；悬停跟踪使用全局 `static TRACKING`，多个覆盖层共享一个标志，导致第二个覆盖层无法重新臂定 `TME_LEAVE`，悬停展开便签在多个窗口间切换时失效。
- **备选方案**：
  1. 弹窗创建时预建覆盖层、取消时销毁（否决，生命周期与用户确认语义耦合）；
  2. 确认后创建、Drop 兜底清理（采纳）；
  3. 悬停跟踪继续用全局标志（否决，多覆盖层相互干扰）。
- **理由**：
  1. 用户取消标记意味着该窗口没有被标记，预创建的覆盖层是纯浪费且需要额外的销毁路径；确认后创建让覆盖层的存在与标签的存在严格一致；
  2. 覆盖层创建请求经由隐藏窗口 WndProc 的 `WM_CREATE_OVERLAY` 分支处理（`src/main.rs` 第 179 行起），已有重复创建去重（`Entry::Occupied` 忽略）；
  3. 即使某条路径遗漏销毁，`Overlay` 离开 `OVERLAY_STORE` 时触发 `Drop`，自动销毁悬停 tooltip（先置空 userdata 再 `DestroyWindow`，防止 tooltip WndProc 访问悬垂指针）并销毁自身窗口，孤儿不可能长期残留；
  4. per-overlay `tracking` 标志用 `swap(true)` 原子读写（`src/sys/overlay.rs` 第 416 行），每个覆盖层独立臂定/复位 `TME_LEAVE`，修复多覆盖层互踩。
- **影响与后续跟进**：`WM_CLOSE` 分支的注释明确记录了"覆盖层创建已移至确认分支，取消/关闭时无需销毁覆盖层"（`src/ui/popup.rs` 第 396 行起），后续改弹窗流程时勿回退到预创建模式。`OverlayState.target_hwnd` 供悬停 tooltip 反向查询目标窗口标签，若后续支持多个 tooltip 同时显示，需将 userdata 从单指针扩展为链表或集合。

---

## D9：快速开发阶段，不承诺向后兼容

- **决策**：项目当前处于**快速开发阶段**，功能迭代优先于稳定兼容，**不承诺任何向后兼容**。窗口行为、快捷键、数据结构、公开 API、配置文件格式等均可随需求随时调整，无需维护旧版本兼容层或迁移路径。
- **背景**：产品方向仍在快速探索（见 `doc/issues-and-requirements.md` 的 5 项已确认问题与 5 项需求规划），大量行为（弹窗焦点、退出方式、角标形态、Manager 形态）预计会调整；过早固化接口只会拖慢迭代。
- **备选方案**：维持稳定 API 并做兼容层（否决，当前阶段无外部依赖方，兼容成本纯属浪费）。
- **理由**：项目为纯会话内工具、无持久化、无插件生态、无第三方调用方，唯一用户即本项目自身；快速迭代期保持灵活性收益远大于兼容性收益。
- **影响与后续跟进**：① 文档（README、doc/）始终以当前 HEAD 为准，历史决策记录仅作参考不构成契约；② 任何提交可自由引入破坏性变更；③ 当产品形态稳定后（预计在 R4/R5 落地、问题 1/3/5 修复完成后），再评估是否进入稳定兼容阶段并冻结公开接口。

---

## D10：暗色主题与圆角采用 Win32 原生方案，配置持久化到 TOML

- **决策**：暗色主题与圆角全部采用 Win32 原生实现：`DwmSetWindowAttribute`（沉浸式暗色 `DWMWA_USE_IMMERSIVE_DARK_MODE` + 圆角 `DWMWA_WINDOW_CORNER_PREFERENCE`）+ `WM_CTLCOLOR*` 画刷机制（`CreateSolidBrush` + 进程级画刷缓存），不引入 UI 框架。UI 偏好持久化到 `%APPDATA%\WinTag\config.toml`（TOML 格式，`serde` 序列化，见 `src/core/settings.rs`）。设置页（`src/ui/settings.rs`）复用 panel 窗口范式。此举打破"纯会话、无持久化"原则，D9 已授权。
- **背景**：任务 T9 需要暗色模式、设置页面与配置持久化。此前项目为纯 Win32 手写（D1），无 UI 框架、无持久化设计（D9 明确不承诺向后兼容、不维护兼容层）。
- **备选方案**：
  1. 引入 egui/eframe 或第三方 UI 库实现设置页与主题（否决，与 D1 决策冲突，覆盖层区域级点击穿透仍无法表达）；
  2. 主题色直接硬编码到各窗口 WndProc（否决，设置页实时切换需要统一状态源）；
  3. 配置存注册表或 JSON 文件（备选，最终选 TOML + serde，零配置即用且结构直接对应 `Settings` 结构体）。
- **理由**：
  1. 暗色/圆角均为 DWM 层面单属性调用，一次 `DwmSetWindowAttribute` 即可表达，无需自绘整套 UI；圆角属性（值 33）在 Win10 及更早系统静默失败不影响运行；
  2. `WM_CTLCOLOR*` + 进程级画刷缓存（`BRUSH_CACHE`）让明暗双调色板以最小成本覆盖静态文本/编辑框/列表视图/按钮全部控件；缓存画刷永不删除（进程退出统一回收），避免"画刷句柄失效导致控件重绘未定义行为"；
  3. 主题调色板经 `ui::theme::set_theme(ThemeColors)` 写入全局状态、`theme_colors()` 读取，面板/弹窗/设置页在 `WM_CTLCOLOR*` 时读取同一调色板；设置页保存后经 `WM_APP_THEME_CHANGED`（`WM_APP+5`）广播到主线程，主线程重新应用到各窗口，实时生效；
  4. 配置加载失败（文件缺失/损坏/缺字段）一律回退默认配置并打印中文警告，绝不 panic；父目录不存在时保存自动创建（`%APPDATA%\WinTag` 首次运行不存在）；
  5. 设置页复用 panel 的窗口生命周期范式（`lpCreateParams` 传 Box → `WM_CREATE` 写 userdata → `WM_DESTROY` 回收，`WM_CLOSE` 仅隐藏不销毁），一致性成本最低；
  6. 配置持久化范围严格限定在 UI 偏好（主题/圆角），标签/便签数据仍纯会话内，符合 D9 授权的边界。
- **影响与后续跟进**：① 打破"纯会话"原则属于 D9 授权范围，仅 UI 偏好持久化；② `config.toml` 格式在快速开发阶段可随时调整（D9），无需迁移路径；③ R1（自定义快捷键）配置表结构已在 `Settings` 模型预留扩展位，设置页热键编辑 UI 待后续；④ 圆角仅 Win11 生效，Win10 静默降级为直角，符合预期；⑤ 主题变更实时生效依赖 `WM_APP_THEME_CHANGED` 广播链路，后续新增窗口需在 `WM_CTLCOLOR*` 中统一走 `theme_colors()` 读取，勿硬编码颜色。

---

## D11：UI 全面现代化（comctl32 v6 manifest + 全局字体 + 自绘控件 + UpdateLayeredWindow 角标）

- **决策**：以四项基础设施根治"丑"的系统性根因，沿用 D10 的纯 Win32 原生路线：
  1. **comctl32 v6 视觉样式清单**（`build.rs` + `winresource` 嵌入 manifest）：使 EDIT/BUTTON/ListView/ComboBox 从 Win95 经典外观切到现代视觉样式，是 `SetWindowTheme("DarkMode_Explorer")` 能生效的前提；
  2. **全局消息字体**（`SPI_GETNONCLIENTMETRICS` → `lfMessageFont` → `CreateFontIndirectW`，进程级 `OnceLock` 缓存 + `EnumChildWindows` + `WM_SETFONT`）：根治遗留 System 位图字体（中文粗糙），两档（常规/粗体）覆盖 tooltip 标题强调；
  3. **扩展调色板**（`ThemeColors` 增 `accent`/`border`/`hover`/`selected`/`muted`/`listview_alt_bg`/`header_*`，暗色调柔和为灰黑档 #202020/#E6E6E6）：供自绘控件与列表行着色，新增 `blend()` 颜色混合纯函数；
  4. **自绘圆角按钮**（`src/ui/button.rs`，`BS_OWNERDRAW` + `WM_DRAWITEM`）：`WM_CTLCOLORBTN` 无法改变标准按钮灰面子（Win32 已知限制），改自绘扁平圆角矩形，accent/secondary 两档 + hover/pressed 状态 + 键盘焦点框；
  5. **ListView 完整暗色**（`SetWindowTheme("DarkMode_Explorer")` + `NM_CUSTOMDRAW` 行级着色 + `LVS_EX_DOUBLEBUFFER`）：表头 SysHeader32 与滚动条随之暗化，行奇偶交替/选中态着色，双缓冲消除拖动闪烁；
  6. **DPI 缩放辅助**（`src/ui/layout.rs` `dp()`，`GetDpiForWindow` 96 基准）：全部硬编码像素坐标经 `dp()` 换算，高 DPI 屏控件不再过小；
  7. **覆盖层 UpdateLayeredWindow + 角标软件光栅**（`src/sys/badge.rs` 纯函数 SDF 光栅圆边三角形 → `overlay.rs` `UpdateLayeredWindow(ULW_ALPHA)` 32bpp 预乘 RGBA 提交）：替代原 `FillRect(DOT_RECT)` 实心方块 + `LWA_COLORKEY` 色键透明，获得逐像素 alpha 抗锯齿，完成 R2；
  8. **tooltip 圆角分层重绘**（`RoundRect` r=6 + 1px 边框 + 标题加粗/备注正文分层 + 宽度自适应上限 360px）：解决问题 4 固定 300px 截断；
  9. **tooltip 主题热更新**（`TOOLTIP_THEME` 从 `OnceLock` 改 `Mutex`）：主题切换后新建 tooltip 即时采用新配色，修复原 OnceLock 一次性注入的遗留。
- **背景**：实测反馈（问题 7、9.1-9.8、10、4）系统性指出 UI 粗糙——根因非单点配色，而是 manifest/字体/主题覆盖/角标形态/布局缺陷一批基础设施缺失。D10 的 `WM_CTLCOLOR*` 机制无法覆盖按钮灰面与 ListView 表头/列表体。
- **备选方案**：
  1. 引入 UI 框架（否决，与 D1 冲突，覆盖层区域级点击穿透仍无法表达）；
  2. 仅调配色不改控件外观（否决，`WM_CTLCOLORBTN` 对标准按钮无效，根因仍在）；
  3. 角标用 GDI `Polygon`（否决，无抗锯齿，边缘锯齿）。
- **理由**：
  1. manifest 是 Win32 控件现代化的前提（v6 才加载现代视觉样式 + 使 `SetWindowTheme` 生效），零运行时成本，仅构建期嵌入；
  2. 全局字体经 `lfMessageFont` 取系统消息字体（中文即微软雅黑），与系统主题一致，无硬编码字体名风险；
  3. 自绘按钮是唯一能彻底控制按钮外观的方案，`BS_OWNERDRAW` + `WM_DRAWITEM` 是 Win32 标准自绘路径，hover/pressed 经子类化跟踪 `TME_LEAVE`；
  4. `DarkMode_Explorer` 是 Win11 ListView 暗色的事实标准（Win10 1809+ 生效，更旧降级不撕裂）；
  5. 角标用软件光栅（SDF）而非 GDI 路径，因 GDI `Polygon` 无抗锯齿；`UpdateLayeredWindow` 是 `WS_EX_LAYERED` 窗口逐像素 alpha 的唯一标准路径；
  6. `dp()` 缩放与 D5 的 Per-Monitor V2 感知配套，96 基准设计像素直观可读。
- **影响与后续跟进**：① `winresource` 为新 dev-dependency（构建期，首次需网络；拉不到可外置 manifest 兜底）；② 暗色滚动条在 Win11 上随 manifest+主题大部分生效，属尽力而为项；③ 弹窗颜色仍硬编码 `TagColor::Orange`（五色选择 UI 不在本轮范围，留作后续）；④ `ui::button` 与 `ui::layout` 为新增模块，AGENTS.md 项目结构已同步；⑤ tooltip 主题改 `Mutex` 后 `set_tooltip_theme` 可重复调用，`reapply_theme` 链路已接入。

---

## D12：覆盖层标题条 + 面板树形化 + 单击置前（2026-08-30）

- **决策**：四项功能改进，全部沿用纯 Win32 原生路线：
  1. **覆盖层左上角标题条**（R6 落地）：角标右侧附加圆角胶囊标题条，显示标签标题（`tag.title`，空回退 `window_title`），超 5 个 Unicode 字符以省略号截断（`badge.rs::truncate_title` 纯函数）；渲染在 `update_layered_badge` 内合成——`badge.rs::render_rounded_rect` SDF 圆角矩形做底（tooltip 主题配色 + border 描边）+ GDI `CreateDIBSection` 文字蒙版（黑底白字 → 亮度作 alpha → tooltip_fg 着色）→ 预乘 RGBA 合成提交；窗口尺寸随内容自适应（`UpdateLayeredWindow` 的 `psize` 驱动窗口大小，`sync_position` 加 `SWP_NOSIZE` 仅跟位置）；
  2. **悬停显示完整标题与备注**：标题条区域命中（`WM_NCHITTEST` 返回 HTCLIENT）即触发既有 tooltip 机制（`WM_MOUSEMOVE` → `TrackMouseEvent` → 自绘弹窗，显示完整 title + note），未新建机制；
  3. **面板改可展开树形列表**：概览面板 SysListView32 四列报表 → SysTreeView32（`TVS_HASBUTTONS|HASLINES|LINESATROOT|SHOWSELALWAYS`），每个标签一个根项（`lParam` = 目标 HWND），展开显示"备注/窗口/进程"三行详情子项；配色改 `TVM_SETBKCOLOR/SETTEXTCOLOR/SETLINECOLOR`（替代 ListView 的 `NM_CUSTOMDRAW` 行级着色），暗色主题沿用 `DarkMode_Explorer`；
  4. **面板单击置前**：`NM_CLICK`/`NM_DBLCLK` → `TVM_HITTEST` 定位根项（命中 `[+]` 按钮时交给树控件展开，详情子项不触发）→ 目标窗口最小化先 `SW_RESTORE`，再 `SetForegroundWindow` + `HWND_TOP`（**临时置前**语义：切走后不驻留顶层）。
- **背景**：用户反馈三项使用性需求——① 角标仅有色块不足以辨识窗口，需直接显示标题（默认 5 字 + 省略号），悬停看完整标题与注释；② Ctrl+Shift+M 面板的四列表格信息密度低，希望改为可展开列表；③ 面板内应能一键把对应软件置顶到最前。
- **备选方案**：
  1. 标题条用独立分层窗口（否决，窗口数量翻倍且与角标同步复杂化，单窗口内合成更简单可靠）；
  2. 文字整体软件光栅（否决，GDI 字体渲染质量与回退链成熟，仅做蒙版合成即可兼顾抗锯齿与主题色）；
  3. 面板按软件分组两级树（否决，多数软件单窗口会多一层空壳，每标签一行 + 详情子项更贴合现有数据结构）；
  4. 置顶用 `HWND_TOPMOST` 常驻（否决，会一直压住其他窗口，采用与既有双击一致的临时置前）。
- **理由**：
  1. 标题条合入覆盖层单窗口，复用既有 `UpdateLayeredWindow` 管线与 TagStore 读取链，命中测试仅在 `WM_NCHITTEST` 增加矩形分支，影响面最小；
  2. `Settings.show_badge_title` 经 `sys::overlay::set_show_title` 注入（`AtomicBool`），维持 sys 层不读 core 的依赖方向（镜像 `set_tooltip_theme` 模式），`reapply_theme` 统一重注入并 `refresh()` 全部覆盖层（顺带修复主题切换后覆盖层不重绘、重复标注同窗口不刷新两处遗留）；
  3. 旧 config.toml 缺 `show_badge_title` 字段经 `#[serde(default = "default_true")]` 回退显示，不破坏既有配置；
  4. TreeView 的 lParam 承载 HWND 与原 ListView 方案一一对应，`handle_list_activate` 逻辑平移，单击即可达（双击保留同样语义）。

---

## D13：面板标签变更自动刷新 + 最小宽度放宽（2026-08-30）

- **决策**：
  1. **标签变更广播**：新增 `common::WM_APP_TAGS_CHANGED`（`WM_APP+6`），便签弹窗 `save_and_close` 保存成功后在既有 `WM_CREATE_OVERLAY` 广播旁追加发送；主线程 `hidden_wndproc` 收到后若面板可见则原样转发到面板窗口（`wParam` 透传目标 HWND），面板 `panel_wndproc` 收到即调 `refresh_tree` 重建树形列表。
  2. **展开状态保留**：`refresh_tree` 重建前经 `collect_expanded_targets`（`TVM_GETNEXTITEM` 遍历根项 + `TVM_GETITEMW` 读 `TVIS_EXPANDED`）按目标窗口句柄（根项 `lParam`）记录展开集，重建后对匹配项 `TVM_EXPAND(TVE_EXPAND)` 恢复——自动刷新不再打断用户的展开浏览。
  3. **面板最小宽度**：`MIN_W` 520 → 300（96 DPI 设计像素，经 `dp()` 缩放；150% DPI 下物理最小宽度 780 → 450px），列表形态允许拖窄到紧凑宽度。
- **背景**：用户反馈面板开启时新建标签后列表不自动更新（面板此前为纯拉取式刷新，仅在打开/搜索输入/点击失效项时重建）；且 520 设计像素的最小宽度对列表而言过宽，无法拖窄。
- **理由**：
  1. 广播链完全镜像 `WM_APP_THEME_CHANGED` 的注入模式（弹窗 → 隐藏窗口 → 主线程 → 面板），ui 层之间不直接持有对方句柄，维持 `main` 统一分发的依赖方向；
  2. 转发前以 `IsWindowVisible` 判可见，面板隐藏时跳过刷新（下次 `toggle_panel` 打开时本就会全量重建）；
  3. 展开状态按目标 HWND 而非树项句柄记录，规避 `TVM_DELETEITEM` 令旧句柄失效的问题；
  4. `MIN_W=300` 下搜索框与树列表仍可用（两者宽度均随 `WM_SIZE`/`layout_children` 收缩），无控件溢出。
- **遗留**：面板创建尺寸 `WIN_W`/`WIN_H` 尚未经 `dp()` 缩放（高 DPI 下初始窗口偏小），独立问题另行处理。

---

## D14：弹窗键盘语义补全 + 按钮遮挡修复 + 面板默认纵向 + 角标首绘修复（2026-08-30）

- **决策**：四项修复，全部沿用既有机制（子类化转发 / 刷新广播），不引入新架构：
  1. **弹窗 Tab 焦点循环**（R7）：编辑框子类化过程（`popup.rs::edit_subclass_proc`）与按钮子类化过程（`button.rs::button_subclass_proc`）均拦截 `VK_TAB`（按钮加 `VK_ESCAPE`）`PostMessageW` 转发父弹窗；弹窗 `WM_KEYDOWN` 按"标题 → 备注 → 确认 → 取消"（`FOCUS_ORDER`）循环 `SetFocus`，Shift+Tab 反向。**不引入 `IsDialogMessageW`**——它会抢走回车键走对话框默认按钮语义，与本项目"子类化转发回车=保存"机制冲突；
  2. **备注框回车=保存、Shift+回车=换行**（R8/R10）：子类化过程中备注框裸回车与标题框回车同路径转发保存；Shift+回车不拦截，透传多行 EDIT 类过程插入换行；
  3. **弹窗按钮遮挡修复**（问题 11）：`note_h` 补减标签行高（`ctrl_h + 4`）；顺带统一弹窗布局 DPI 基准——控件坐标原先混用未缩放的 `WIN_W`，改用 `dp(WIN_W)`/`dp(WIN_H)` 与 `WM_GETMINMAXINFO` 锁定的窗口尺寸同基准；
  4. **角标首绘修复**（问题 12）+ **面板默认纵向**（R14）：见"根因"。
- **根因（角标不显示）**：实测只有设置保存广播触发的 `reapply_theme → refresh()`（InvalidateRect+UpdateWindow 重走 `UpdateLayeredWindow`）能让角标出现，说明**创建瞬间的同步首绘内容未生效**；且 `sync_position` 按位置去重早退，目标窗口激活压住覆盖层后 z 序永远无法恢复。修复三管齐下：
  1. `Overlay::create` 首绘异步化（去掉创建栈内 `UpdateWindow`，改 `InvalidateRect` 由消息循环空闲时绘制）；
  2. `WM_CREATE_OVERLAY` Vacant 分支创建后立即 `refresh()`（与设置保存同一恢复路径，双保险）；
  3. `sync_position` 去掉去重早退，每次同步都 `SetWindowPos(HWND_TOPMOST)` 重申置顶（事件合并 + 500ms 轮询已节制频率）；`update_layered_badge` 的 ULW 失败改为记日志，便于今后定位。
- **备选方案**：
  1. Tab 导航改 `IsDialogMessageW` + `WS_TABSTOP`（否决：对话框管理器接管回车后与子类化转发保存冲突，且影响 settings/panel 现有按键处理）；
  2. 角标首绘改 PostMessage 延迟一拍（否决：InvalidateRect 异步路径已达同样效果且更简单）。
- **理由**：
  1. 子类化转发链是本弹窗既定模式（问题 5.2 起沿用），新增 VK_TAB 分支零新增机制；
  2. 面板 640×480 → 400×640 仅改常量，`WM_SIZE → layout_children` 本就自适应；
  3. 角标三项修复互相独立、均为幂等操作，任一根因成立都覆盖。
- **遗留**：面板创建尺寸仍未经 `dp()` 缩放（D13 遗留项延续，另行处理）。

---

## D15：面板树形重构——根项合并窗口名、展开显示完整多行备注（2026-08-30）

- **决策**：`refresh_tree` 项结构调整：根项文本由标签标题改为"标题 | 窗口名称"一行（`lParam` 仍为目标窗口句柄，点击置前/展开状态保留逻辑不变）；详情子项由"备注/窗口/进程三行"改为"备注："标签行 + 备注完整内容——TreeView 项为单行控件项，多行备注逐行拆为独立子项（空备注显示占位"（无）"）。进程/窗口字段不再单独展示，但搜索过滤仍匹配全部四个字段。
- **背景**：R7/R8/R10 落地后备注支持多行（Shift+回车换行），而旧树形结构把备注压成"备注：xxx"一行子项，换行内容被截断无法完整显示；窗口名称与标题并排展示信息密度也更高。
- **备选方案**：
  1. 子项内嵌换行渲染（否决：SysTreeView32 项为单行，需自绘 ownerdraw 才能换行，复杂度不成比例）；
  2. 备注首行并入"备注："、续行单独子项（否决：与用户预期的"备注：/内容"两段式版式不符）。
- **理由**：仅改 `refresh_tree` 的文本拼装，树控件机制（lParam 置前、展开状态按 HWND 恢复、搜索过滤、DarkMode_Explorer 主题）零改动；多行拆行后每行仍受树宽自动裁剪，横向滚动条兜底超长行。
- **遗留**：无。

---

## D16：标注流程闭环——角标点击编辑 + 颜色选择 + 面板右键菜单（2026-08-30）

- **决策**：四项围绕"标注流程闭环"的改进：
  1. **角标/标题条单击打开编辑弹窗**（R5）：覆盖层可交互区（`WM_NCHITTEST` 已返回 HTCLIENT 的角标三角形 + 标题条）补 `WM_LBUTTONDOWN` → 经注入的隐藏窗口发 `WM_APP_EDIT_TAG`（`WM_APP+7`）→ 主线程校验目标存活且有标签后调用 `ui::popup::create_popup`（预填标题/备注/颜色）。sys 层不反向依赖 ui，隐藏窗口句柄经 `set_message_target` 注入（镜像 `set_tag_store` 模式）；
  2. **弹窗颜色选择行**（R16）：5 个 `BS_OWNERDRAW` 色块（`WM_DRAWITEM` 拦截自绘纯色圆角矩形，选中项 2px 前景色描边环），点击走标准 `WM_COMMAND` 路由更新 `PopupData.selected_color`；编辑已有标签预选原颜色；保存不再固定橙色；
  3. **面板右键菜单 + Esc 关闭**（R17）：根项右键 `TrackPopupMenu(TPM_RETURNCMD)`（置前/编辑/移除），移除 = 清存储 + `WM_DESTROY_OVERLAY` + 刷新；搜索框子类化转发 Esc（标准 EDIT 吞键，与弹窗 Tab 同一坑）；
  4. **标题/备注动态长度读取**：`GetWindowTextLengthW` 分配取代 256/1024 定长栈缓冲——原先超长内容被**静默丢弃**，用户毫无感知。
- **备选方案**：
  1. 色块用 `button::create_button` 复用文本按钮（否决：文本按钮自绘流程注册状态表、悬停态对纯色块无意义，直接 `BS_OWNERDRAW` + `WM_DRAWITEM` 拦截更薄）；
  2. 覆盖层直接调用 `ui::popup::create_popup`（否决：sys → ui 反向依赖，违反模块方向约定）；
  3. 面板菜单经 `WM_COMMAND` 路由（否决：`TPM_RETURNCMD` 同步取回更简单，无路由状态需保存）。
- **理由**：全链路复用既有机制——渲染管线按 `tag.color` 取色使颜色选择零渲染改动；`WM_APP_EDIT_TAG` 通道同时服务角标单击与面板菜单两个入口；Esc 转发与 D14 的按键子类化转发同构。
- **遗留**：无。

---

## D17：BGRA 字节序修复 + 统一主题管理器 + 弹窗单例/定位 + 光标修复（2026-08-30）

- **决策**：五项修复，全部为既有机制的纠偏与收口：
  1. **覆盖层颜色字节序**（问题 15）：`badge.rs` 两个 SDF 光栅函数与 `overlay_text_into` 文字合成的像素写入从 RGBA 改为 **BGRA**（`UpdateLayeredWindow` 32bpp 位图内存布局即字节序 B,G,R,A、预乘 alpha），新增单测锁定字节序（红填充中心像素 `[B=0,G=0,R=255,A=255]`）；
  2. **统一主题管理器**（问题 16，用户需求"用一个统一的主题管理器控制背景色、方便扩展复用"）：`theme.rs` 新增 `sync_window_theme()`（读全局设置+系统深浅色 → 解析调色板写入全局 → 返回 `WindowThemeCtx{colors,dark,corner}`）与 `apply_control_theme()`（对 EDIT/BUTTON/COMBOBOX 应用 `DarkMode_Explorer`/`Explorer` comctl32 变体，使下拉框箭头/复选框图形/滚动条随主题）；弹窗/面板/设置三窗口 WM_CREATE 统一走该入口（替代各自复制的四步），`reapply_theme` 热更新时重应用控件变体；
  3. **下拉框配色**（问题 16）：下拉列表/`CBS_DROPDOWNLIST` 显示区的 `WM_CTLCOLORLISTBOX`/`WM_CTLCOLORSTATIC` 发给 COMBOBOX 自身而非父窗口——子类化下拉框拦截配色按全局调色板着色（镜像 Edit 子类化模式）；
  4. **弹窗单例 + 光标附近定位**（问题 13/用户需求）：`ACTIVE_POPUP` 注册表——同目标请求复用置前聚焦、异目标销毁旧弹窗重建；弹窗创建于光标右下 (16,16)，按 dp 实际尺寸钳制到所在显示器工作区（创建样式去掉 `WS_VISIBLE` 先定位后显示，避免默认位置闪现）；顺带修正弹窗创建尺寸未过 `dp()` 的 DPI 隐患；
  5. **窗口类光标**（问题 14）：5 个自注册窗口类统一 `hCursor = arrow_cursor()`（common 新增辅助）——类光标 NULL 时 `DefWindowProc` 的 `WM_SETCURSOR` 会隐藏光标。
- **附带改进**：tooltip 文本动态长度读取（>512 字符备注不再截断）、tooltip 位置工作区钳制（底部放不下翻到光标上方）、标题尾部 CR 剥离。
- **备选方案**：
  1. 字节序在合成端交换而非光栅端输出 BGRA（否决：三处消费点统一为一种布局更不易再错，且单测可直接断言缓冲字节）；
  2. 下拉框换 `CBS_OWNERDRAWFIXED` 自绘（否决：为配色引入整套项绘制，子类化拦截 CTLCOLOR 更薄）。
- **遗留**：**问题 17（悬停 tooltip 备注显示不完整）待修复**——已做动态读取/定位钳制/CR 清理三部分，现象仍待复测排查。

---

## D18：设置页配色消息参数修复 + 下拉框 owner-draw 自绘 + tooltip 备注裁切修复（2026-08-30）

- **决策**：三项修复，均为用户实测反馈的缺陷收口：
  1. **设置页 `WM_CTLCOLOR*` 参数误用**：`settings.rs` 的 `ctlcolor_brush` 把 **lParam（控件 HWND）当作 HDC** 传给 `SetTextColor`/`SetBkColor`，而 Win32 约定为 wParam = HDC、lParam = 控件句柄——对窗口句柄调用 GDI 文字函数静默失败，设置页所有文字按 DC 默认黑字白底绘制，暗色主题下明显错乱。改为取 wParam（与 `panel.rs`/`popup.rs` 的同名函数对齐）；
  2. **下拉框改 `CBS_OWNERDRAWFIXED` 自绘**（**推翻 D17-3 的子类化方案**）：实测（诊断日志证实）Win11 上闭合成 `CBS_DROPDOWNLIST` 的显示区由主题变体直接绘制，**不发送任何 `WM_CTLCOLOR*`**——父窗口与子类过程均收不到配色消息，子类化路径对显示区是死路（仅下拉列表部分有效）。下拉框改用 owner-draw：`WM_DRAWITEM` 中关闭态选中字段（`itemID == -1`/`ODS_COMBOBOXEDIT`）按编辑框配色、列表项按选中/禁用态用列表配色绘制，文本经 `CB_GETLBTEXTLEN` 动态读取，`WM_MEASUREITEM` 自报行高；`DarkMode_Explorer` 仍应用于下拉列表边框/滚动条；
  3. **tooltip 备注不可见**（D17 遗留问题 17 根因定位）：两层缺陷叠加——创建时用窗口默认字体对全文整体量测高度（实际绘制用粗体标题 + 常规备注的消息字体，行高更大），窗口偏矮裁掉备注行；`WM_PAINT` 里用 `y = tr.bottom + 4` 推备注行位置，但 `DrawTextW` 不带 `DT_CALCRECT` 时**不回写矩形 bottom**，备注被画到窗口底边之外。修复：新增 `measure_text_size`（`DT_CALCRECT`、分字体量测后按绘制排版累加），绘制侧改用 `DrawTextW` 返回值（实际绘制高度）推进下一行。
- **备选方案**：
  1. 继续子类化 + 补拦 `WM_ERASEBKGND` 等消息（否决：显示区不走任何配色消息，无消息可拦，实测证据否定该路径）；
  2. 量测端与绘制端都改 `DT_CALCRECT`（采用：量测端必须 `DT_CALCRECT` 才不落屏；绘制端以返回值为准，不再依赖矩形回写语义）。
- **教训**：D17-3 的"子类化拦截 CTLCOLOR"方案当时未经实测验证即写入决策记录；本轮以诊断日志实证其不覆盖显示区后推翻。决策记录中未经验证的方案应标注"待实测"。

---

# 代码质量审计

审计日期：2026-08-20。审计范围为 `src/` 下全部 14 个 `.rs` 文件，共 2561 行。审计在项目根目录执行。

## 审计方法与命令

| 检查项 | 命令 | 期望 |
| :--- | :--- | :--- |
| unsafe 出现次数 | `Select-String -Path src/**/*.rs -Pattern "unsafe"`（经 `Get-ChildItem -Recurse` 完整列举） | 逐文件统计 |
| 裸 unwrap/expect | `Select-String -Path src/**/*.rs -Pattern "\.unwrap\(\)|\.expect\("` | 0 命中 |
| sys 层依赖方向 | `Select-String -Path src/sys/*.rs -Pattern "crate::core"` | 0 命中 |
| 魔法常量残留 | `Select-String -Path src/**/*.rs -Pattern "0x8000"` | 0 命中 |
| 行数统计 | `Get-Content` 计数 | 逐文件统计 |

> 说明：PowerShell 7 的 `src/**/*.rs` 通配符会漏掉 `src/` 根目录下的文件（`main.rs`、`hotkey.rs`、`lib.rs`），本次审计改用 `Get-ChildItem -Recurse` 完整枚举后逐文件执行 `Select-String`，数字为全量结果。

## 各文件行数与 unsafe 统计

| 文件 | 行数 | unsafe 出现行数 | `// SAFETY:` 注释行数 |
| :--- | ---: | ---: | ---: |
| src/main.rs | 371 | 8 | 7 |
| src/hotkey.rs | 92 | 1 | 1 |
| src/lib.rs | 5 | 0 | 0 |
| src/common/mod.rs | 72 | 2 | 2 |
| src/core/mod.rs | 2 | 0 | 0 |
| src/core/tag.rs | 39 | 0 | 0 |
| src/core/matcher.rs | 30 | 0 | 0 |
| src/sys/mod.rs | 11 | 0 | 0 |
| src/sys/window.rs | 125 | 11 | 11 |
| src/sys/win_event.rs | 247 | 4 | 3 |
| src/sys/overlay.rs | 634 | 33 | 30 |
| src/ui/mod.rs | 2 | 0 | 0 |
| src/ui/panel.rs | 509 | 40 | 40 |
| src/ui/popup.rs | 422 | 35 | 31 |
| **合计** | **2561** | **134** | **125** |

unsafe 全部集中在与 Win32 API 直接交互的层：sys 层（window/win_event/overlay 共 48 处）、ui 层（panel/popup 共 75 处）、main.rs（8 处）、common（2 处）、hotkey（1 处）。core 层（tag/matcher）零 unsafe，纯数据逻辑与架构约定一致。

## SAFETY 注释抽查（5 处代表性 unsafe）

以下 5 处通过读取文件逐一确认，均存在紧邻的 `// SAFETY:` 说明：

1. **src/main.rs:38** `SetProcessDpiAwarenessContext`（V2→V1 降级链）：前置注释说明"进程级设置，无参数生命周期问题；失败时降级到 V1"。
2. **src/main.rs:297** `poll_overlays` 中的 `IsWindow` 校验：注释说明"IsWindow 为只读查询，句柄失效时返回 FALSE，不会产生未定义行为"。
3. **src/sys/overlay.rs:92-93** `unsafe impl Send/Sync for Overlay`：前置注释块（第 86-91 行）论证单线程消息泵架构下 HWND 手动实现 Send/Sync 不会产生未定义行为。
4. **src/sys/win_event.rs:144** `install_range` 中的 `SetWinEventHook`：注释说明"仅注册回调，不执行被监控进程内注入；固定 OUTOFCONTEXT + SKIPOWNPROCESS"。
5. **src/ui/popup.rs:84** `create_popup` 中的 `CreateWindowExW`：注释说明失败时归还 `data_ptr` 所有权，避免 Box 泄漏。

## 关键检查项结果

| 检查项 | 结果 | 说明 |
| :--- | :--- | :--- |
| 裸 `.unwrap()` / `.expect(` | **0 命中** | 生产代码与测试代码均无；错误处理统一走 `anyhow::Result`、`?` 传播与 `ok()`/`unwrap_or` 分支，符合 AGENTS.md 约定 |
| sys 层 `crate::core` 依赖 | **0 命中** | `src/sys/*.rs` 无对 core 的源码级路径引用；全树搜索命中仅存在于 `core/matcher.rs`（core 内部）与 `ui/panel.rs`、`ui/popup.rs`（ui → core，合法方向） |
| `0x8000` 魔法常量 | **0 命中** | 事件常量使用 `0x8001`/`0x8002`/`0x8003`/`0x800B`/`0x8017`/`0x8018`（见 `src/sys/win_event.rs`），均为 windows-rs 0.58 未暴露的稳定字面量且带注释，未出现 `0x8000` 占位 |

## 如实报告的遗留项

审计发现的注释粒度差异（均为低风险，不影响正确性，仅记录）：

1. **src/main.rs:233** 隐藏窗口 WndProc 默认分支的 `DefWindowProcW` 直通调用无紧邻 `// SAFETY:` 注释。该调用与 panel.rs、popup.rs 中带注释的同型直通一致，属于简单透传。
2. **src/sys/win_event.rs:163** `unsafe extern "system" fn win_event_callback` 函数声明本身无紧邻注释（windows-rs 回调签名强制 `unsafe extern`）。函数体内的 `PostMessageW` unsafe 块有完整注释（第 181-183 行）。
3. **src/sys/overlay.rs:92-93** `unsafe impl Send` 与 `unsafe impl Sync` 两处共享第 86-91 行同一段多行 SAFETY 说明，按行计数时会多出 1 处差异。
4. **src/ui/popup.rs:350-361** `Tag` 构造段中多处内联 `unsafe { (*data).window_title.clone() }` 等解引用共用第 352 行的单条 SAFETY 说明（同一 `data` 指针，WM_DESTROY 前有效），未逐行重复注释；第 378 行 `println!` 内解引用同理。

上述 4 项均为"注释粒度"问题而非缺注释，建议后续提交时顺手补齐，使 unsafe 行数与 `// SAFETY:` 行数严格对应，便于用脚本做零差异回归。

## 功能级遗留项登记（对照需求/技术规格文档）

以下为需求文档（`doc/requirements.md`、`doc/technical-specs.md`、`doc/development-plan.md`）中声明、但本次重构**未实现**的功能项，逐一登记供后续迭代排期。登记的目的：避免"需求文档写了、代码没做、审计也没记"的信息断层。

| # | 遗留项 | 来源 | 现状 | 优先级 | 建议方案 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| F1 | **窗口激活闪烁反馈** | requirements.md §2"窗口激活反馈：切换到已标记窗口时，标记短暂闪烁或高亮提醒" | 未实现。`EVENT_SYSTEM_FOREGROUND` 已接入但仅映射 `BringToTop`（同步位置+置顶），无闪烁/高亮动画 | 中 | 覆盖层 WM_PAINT 增加短暂高亮状态（如扩大圆点/提亮 500ms），FOREGROUND 事件触发后经 `SetTimer` 恢复 |
| F2 | **全屏应用降级处理** | technical-specs.md 边缘情况表"全屏应用：检测全屏状态，隐藏覆盖层，仅托盘通知" | 未实现。无全屏检测（如前台窗口与工作区同尺寸判定），无托盘通知 | 低 | 检测前台全屏窗口时隐藏全部覆盖层；托盘依赖 F3 |
| F3 | **托盘图标** | development-plan.md 阶段一未勾选项"托盘图标，程序常驻后台" | ✅ 已实现（D22/D24）：`Shell_NotifyIconW` 托盘常驻、右键四项菜单、左键/气泡打开面板、`--no-tray` 禁用、退出确认、explorer 重启恢复，见 [D24](#d24托盘常驻化-单实例保护-退出确认-气泡开关2026-09-01d21-p2-short-落地) | 低 | 已完成，本条关闭 |
| F4 | **管理员权限窗口兼容** | technical-specs.md 边缘情况表 | 部分受限。程序默认非提权运行，UIPI 会拦截来自提权窗口的 WinEvent，覆盖层无法跟随管理员窗口；`AGENTS.md` 已注明"可能需要管理员权限" | 低 | 文档化限制 + 可选以管理员身份重启（无自动提权） |
| F5 | **热键冲突处理** | 健壮性 | ✅ 已修复（提交 `37419ed` fix(hotkey)）：改为逐个注册并记录失败项，冲突时降级（仅注册成功的热键）并打印提示，不再整体退出；同时新增设置页热键 `OpenSettings`（`Ctrl+Shift+S`） | 中 | 已完成，本条关闭 |
| F6 | **用户配置（热键自定义/默认颜色）** | development-plan.md 阶段三未勾选项 | ✅ 已实现（`170ce92` 起）：配置数据模型与 TOML 持久化（`%APPDATA%\WinTag\config.toml`），设置页支持主题/圆角选择并保存（见 [D10](#d10暗色主题与圆角采用-win32-原生方案配置持久化到-toml)）；热键编辑 UI 与默认颜色自定义待后续（见 issues-and-requirements.md R1） | 低 | 已完成（部分）：配置持久化落地，热键编辑 UI 待 R1 |

**登记说明**：F1 为需求文档明确功能（重构前已存在缺口，非本次回归）；F2/F3/F4 为技术规格声明的边缘处理；F5 为本次审计新发现；F6 为开发计划未勾选项。均在本次重构范围之外，重构目标（位置同步/DPI/跳转/预填/清理/健壮性）不受影响。

**更新（2026-08-21）**：F5（热键冲突处理）与 F6（用户配置）已随 T9 收尾落地，表中状态已更新；遗留项收敛为 F1（窗口激活闪烁反馈）、F2（全屏应用降级）、F3（托盘图标）、F4（管理员权限窗口兼容）。

**更新（2026-09-01）**：F3（托盘图标）已随 D24 托盘常驻化落地（`Shell_NotifyIconW` + 右键菜单 + 气泡 + `--no-tray`），表中状态已更新；遗留项收敛为 F1（窗口激活闪烁反馈）、F2（全屏应用降级）、F4（管理员权限窗口兼容）。

## D19：角标置顶可选化——新增 `badge_always_top` 设置（2026-08-30）

- **问题**：覆盖层自创建起即带 `WS_EX_TOPMOST`（`overlay.rs` 创建 ex-style），且 `sync_position` 每次同步（事件驱动 + 500ms 兜底轮询）用 `SetWindowPos(HWND_TOPMOST)` 重申置顶（D14 修复角标被遮挡时引入）。置顶带意味着角标浮在**所有**窗口之上，而非仅目标窗口之上——标记窗口一多，被其他窗口盖住的那些目标的角标仍然悬浮在最上层，屏幕上到处都是标志，用户不可控。
- **决策**：新增设置项"角标始终置顶"（R19，`Settings.badge_always_top`，缺省 `true` 保持现状，旧配置缺字段经 serde default 回退为置顶）：
  1. **关闭后的 z 序方案**：`sync_position` 改用 **insert-after**——`SetWindowPos` 的 `hWndInsertAfter` 传目标窗口句柄而非 `HWND_TOPMOST`，把覆盖层插到目标窗口正上方一格。目标被其他窗口盖住时角标随之被遮挡；目标重新激活时 z 序自然回到目标之上。创建时的 ex-style 同步按开关拼接 `WS_EX_TOPMOST`；tooltip 窗口（悬停标题/备注）跟随同一开关，关闭时插到角标窗口正上方（盖住角标本体但非全局置顶）。
  2. **注入链路**（完全镜像 `show_badge_title`/`SHOW_TITLE`）：`sys::overlay::set_badge_always_top(AtomicBool)` 由主线程启动时与 `reapply_theme`（设置保存广播 `WM_APP_THEME_CHANGED` 后）注入；`reapply_theme` 同时对 `OVERLAY_STORE` 中所有已存在覆盖层执行 `refresh()` + `sync_position()`，设置切换即时生效（否则需等下一次位置事件或 500ms 轮询才收敛）。
  3. **设置页**：新增 `BS_AUTOCHECKBOX`"角标始终置顶"（IDC_TOP_CHECK，复用 R6 复选框行距），保存/回显与标题显示开关同路径。
- **备选方案**：
  1. 关闭时移除 `WS_EX_TOPMOST` 后不再重申置顶（否决：普通窗口激活时天然盖过覆盖层，角标极易被目标自身激活压住而"消失"，重蹈 D14 修复前的缺陷）；
  2. 关闭时隐藏被遮挡的角标（否决：需要逐帧遮挡检测，复杂且行为不可预期；insert-after 由窗口管理器原生维护 z 序，零额外成本）。
- **教训**：D14 的"每次同步重申置顶"修复了本程序内部的遮挡问题，但把副作用（全局置顶带）当成了唯一正确行为；需求评审时应区分"z 序相对目标正确"与"全局最高"两个目标。

---

## D20：修复 `badge_always_top` 关闭后角标被目标窗口自身挡住（2026-08-31）

- **问题**：取消"角标始终置顶"后，角标被目标窗口**自身**挡住（不可见）；在 `WM_CREATE_OVERLAY` 创建后立即补调 `sync_position()` 的修复无效。
- **根因**：
  1. **`SetWindowPos` 的 `hWndInsertAfter` 方向用反了**——其语义是"被移动窗口跟在它**之后（背后）**"（MSDN：`hWndInsertAfter` = "The window to precede the positioned window in the Z order"；`HWND_BOTTOM` 备注亦出现 "after any non-topmost window"）。旧代码把**目标窗口句柄**传作 `insert_after`，于是覆盖层被排到目标背后，角标即被目标自身遮挡。创建后补调的 `sync_position()` 因为执行的正是这个错误重排，所以不生效（甚至因新窗口原本在带顶反而更快被压下去）。
  2. **z 带不匹配**：覆盖层的 `WS_EX_TOPMOST` 只在**创建时**按当时开关拼接，开关来回切换不会更新已有覆盖层的样式。跟随模式下非 topmost 覆盖层永远进不了 topmost 带（按带边界钳制），被压到非 topmost 带顶端、恰好落在目标之下。
  3. **陈旧样式残留**：`reapply_theme` 切换开关时只调 `sync_position()`，**从不移除**已存在覆盖层遗留的 `WS_EX_TOPMOST`，导致取消置顶后角标仍悬浮最上层（置顶未真正关闭），或随目标 z 带错乱被遮挡。
- **修正**（`sys/overlay.rs`）：`sync_position` 的"跟随目标"分支改为两步——① 先按目标窗口的 topmost 状态**对齐覆盖层 z 带**（目标 topmost 则补设 `HWND_TOPMOST`，否则用 `HWND_NOTOPMOST` 清掉残留样式）；② 把插入点改为目标**前邻窗口** `GetWindow(target, GW_HWNDPREV)`（返回 NULL/失败 → `HWND_TOP`，即目标处于带顶时置于带顶），从而把覆盖层插到目标**正前方一格**。tooltip 使用同一逻辑（对齐到角标窗口带位 + 插到角标前邻），统一修复同类"被角标挡在背后"的缺陷。置顶模式（`HWND_TOPMOST`）行为不变。
- **实现中遇到的缺陷**：
  1. windows-rs 0.58 的 `GetWindow` 返回 `Result<HWND, Error>` 而非裸 `HWND`，且前置窗口不存在时值域为 NULL；用 `.ok().filter(|h| !h.0.is_null()).unwrap_or(HWND_TOP)` 兜底（`HWND_TOP` 即 NULL）。
  2. clippy `-D warnings` 报 `let_and_return`：块尾应返回表达式，勿先 `let` 再返回。
  3. tooltip 插入代码位于已存在的 `unsafe` 块内，内层再包 `unsafe` 触发 `unused_unsafe` 警告，需移除冗余嵌套；新增 `SWP_NOMOVE` 导入。
- **验证**：`cargo build` / `cargo clippy -- -D warnings` / `cargo fmt -- --check` 全部通过；`cargo test` 67 用例（37 单元 + 30 冒烟）全绿。

---

## D21：扩展规划总纲——分阶段功能增强蓝图（2026-08-31，只读规划未实施）

- **决策**：为 WinTag 追加一批功能增强与使用性问题修复，按分阶段路线图推进（P1 快速稳定+地基 → P2 短期托盘+可配置 → P3 中期工作区+洞察）。本决策仅登记**调研结论与架构约定**，不含代码改动；具体实现由后续决策（D22/D23）与任务执行落实。
- **背景（用户需求 4）**：① 创建托盘替代纯命令行常驻；② 添加图表；③ 添加分组/工作区（批量置顶）；④ 允许自定义快捷键；以及一组使用性问题：窗口移动标签不跟随、概览面板（无默认展开详情 / 无一键展开收起 / 不能靠边隐藏 / 无法纯键盘操作）。
- **备选方案**：一次性全量实现（否决——改动面大、风险高、难以独立验收）；按依赖与价值分阶段（采纳）。
- **理由**：
  1. 项目处于快速开发期（D9），分阶段能保证每个阶段独立可交付、可回归验证；
  2. P1 先修日常缺陷与使用性问题（标签跟随、面板四项），是后续一切增强的地基；
  3. P2 托盘+可配置达成"后台常驻 + 用户主动权"；P3 分组/图表在 P1 的树/面板数据流就绪后才可行。
- **架构约定（贯穿各阶段）**：
  - 新消息常量（`common/mod.rs`，WM_APP+1..7 已占）：`WM_APP+8`=`WM_APP_TRAY`、`WM_APP+9`=`WM_APP_HOTKEYS_CHANGED`、`WM_APP+10`=`WM_APP_BATCH_ACTIVATE`；
  - 数据模型扩展：`Settings` 新字段一律 `#[serde(default)]`（镜像 `show_badge_title`，否则旧配置缺字段整体回退默认）；`Tag` 增 `group: String`；
  - 配置路径解析链：`--config-dir` CLI > `WINTAG_CONFIG_DIR` env > `<exe_dir>/config`（仅当存在或可写）> `%APPDATA%`，`OnceLock` memoize 读写一致，读穿透旧 `%APPDATA%`（D9 不复制）；
  - 依赖方向与既有模式：托盘/图表/分组全部手写 Win32（D1），`ui→core→sys` 不变，注入模式沿用 `set_tag_store`/`set_show_title` 先例。
- **影响与后续跟进**：本决策对应 `doc/issues-and-requirements.md` 的 R18-R20、问题 18-22；各阶段落地时补齐对应决策记录与提交。

---

## D22：系统托盘 + 配置路径解析链（P2 核心，2026-08-31）

- **决策**：托盘与配置路径两块独立改动，均沿用手写 Win32（D1）与既有注入/广播模式。
- **托盘**（`src/sys/tray.rs`）：
  - `Shell_NotifyIconW(dwmessage: NOTIFY_ICON_MESSAGE, lpdata: *const NOTIFYICONDATAW)`；`NOTIFY_ICON_MESSAGE`（`NIM_ADD=0`/`NIM_MODIFY=1`/`NIM_DELETE=2`）；`NOTIFY_ICON_DATA_FLAGS`（`NIF_*`，支持 `BitOr`）；`NOTIFYICONDATAW` 字段 `cbSize/hWnd/uID/uFlags/uCallbackMessage(u32)/hIcon/szTip[128]`；
  - `uCallbackMessage = WM_APP+8`（`WM_APP_TRAY`），单线程消息泵直达 `hidden_wndproc` 无额外线程；
  - **不调 `NIM_SETVERSION`**（保持 `uVersion=0` → 原生 `WM_LBUTTONUP`/`WM_RBUTTONUP` 透传，右键在 `WM_RBUTTONUP` 里 `TrackPopupMenu` 最简；VERSION_4 会变 `WM_CONTEXTMENU` 徒增分支）；
  - `NIM_ADD` 须晚于 `create_hidden_window()`（需要 hWnd），建议热键注册后；
  - 图标：v1 用系统图标 `LoadIconW(None, IDI_WINLOGO)`（零新增 feature/资源）；真实 `.ico` 经 `build.rs` winresource `.icon(1, "assets\\wintag.ico")` + `LoadIconW(GetModuleHandleW(None), MAKEINTRESOURCEW(1))` 升级。图标所有权：Shell 不接管 `hIcon`，自加载的须在 `NIM_DELETE` 后 `DestroyIcon`，系统图标不销毁；
  - **已知限制**：Explorer 重启吞图标——`RegisterWindowMessageW("TaskbarCreated")` 广播重新 `NIM_ADD`（约 15 行，v1 实现，失败则登记为已知限制）。
- **配置路径解析链**（`src/core/settings.rs`）：
  - 不把 `<exe_dir>/config` 当无条件默认（Program Files 只读 + 升级覆盖丢配置）；做「运行时探测可写性 + 显式覆盖优先 + `%APPDATA%` 兜底」解析链，`resolve_config_root()` `OnceLock` memoize（读写一致）；
  - 优先级：`--config-dir` CLI > `WINTAG_CONFIG_DIR` env > `<exe_dir>/config`（仅当存在或 `create_dir_all` 探测可写）> `%APPDATA%\WinTag\config.toml`；
  - 定位 exe_dir：`std::env::current_exe()?.parent()`（内部即 `GetModuleFileNameW`，无需额外 Win32 调用）；
  - 用户指定机制：**env/CLI**（配置读取前解析，无"鸡生蛋"）；**排除设置页内改**（会让"读配置找位置"与"改配置写位置"互相依赖）；
  - 迁移：**不做一次性复制**（D9），改**读穿透**——解析路径不存在时 best-effort 读旧 `%APPDATA%`，命中打印提示；
  - `parse_str` 为路径无关纯函数，sanitize 逻辑与既有 6 单测不受影响。
- **备选方案**：设置页内改配置位置（否决：鸡生蛋）；无条件用 exe 目录（否决：Program Files 只读）。
- **理由**：Program Files 自 Vista 对非提权进程只读，WinTag 不应常驻提权；探测制既满足"默认到 exe 目录"需求又不牺牲健壮性。
- **影响与后续跟进**：新增 `sys::tray.rs`；`Settings` 结构体**不**加配置路径字段（启动时解析）；`doc/issues-and-requirements.md` R18 对应。

---

## D23：自定义快捷键 + 分组/工作区 + 图表（P2/P3，2026-08-31）

- **决策**：三项功能，模型与 UI 均沿既有模式扩展。
- **自定义快捷键**（R1）：
  - `Settings` 增 `#[serde(default)] pub hotkeys: HotkeyMap`；新 `src/core/hotkey_config.rs`（纯 serde：`HotkeyAction`/`HotkeyBinding{modifiers,vk}`/`HotkeyMap`，`Default`=现硬编码值）；
  - `hotkey.rs` 注册改读全局配置（`register_all` → `register_from_settings`），`from_id`/`from_message` 语义不变（唯一耦合点），新增 `reload_hotkeys`（`UnregisterHotKey` + 重注册）；
  - 设置页"快捷键"分页：录制按钮（`WM_KEYDOWN` 修饰键掩码 Ctrl/Shift/Alt + VK）、冲突检测、保存后发 `WM_APP_HOTKEYS_CHANGED`（`WM_APP+9`）广播热更新。
- **分组/工作区**（R19，纯会话不持久化）：
  - `Tag` 增 `pub group: String`（空串=未分组）；**UI 已确认：分组控件置于新建标签弹窗标题输入框下方，允许直接输入或从已有分组下拉选择，编辑时预填原分组**；
  - `refresh_tree` 按分组聚类（组头根项 `lParam=0` 标记 → 标签项 `lParam`=目标 HWND → 备注子项；组名排序，未分组置末）；
  - 组头右键"置前整个分组"批量置前：遍历 `OVERLAY_STORE` 调 `sync_position`+`refresh`（复用现成批量入口 `main.rs` reapply_theme 段），经 `WM_APP_BATCH_ACTIVATE`（`WM_APP+10`）+ `common` 共享槽（`OnceLock<Mutex<Option<String>>>` 注入模式）传组名；
  - **纯会话不持久化**：分组名与标签一起随进程退出清空，仅 UI 偏好持久化（D9/D10 边界）。
- **图表**（R20，默认假设待确认）：新 `src/ui/charts.rs` 独立窗口、GDI 柱状图（纯函数 `bar_layout()` 可单测）；刷新链镜像面板（`WM_APP_TAGS_CHANGED` 仅可见时转发）；从托盘菜单入口打开。默认假设=按分组计数柱状图，时间线需给 `Tag` 加 `created_at`（待拍板）。
- **备选方案**：分组元数据持久化（否决：D9 纯会话原则，仅 UI 偏好持久化；工作区记忆另立例外）；图表时间线（默认不选，需加字段）。
- **理由**：三项均复用既有渲染/树/注入/广播模式，无新架构；分组纯会话契合 D9；`#[serde(default)]` 保证旧配置兼容。
- **影响与后续跟进**：新增 `src/core/hotkey_config.rs`、`src/ui/charts.rs`；`common/mod.rs` 加 `WM_APP+9/+10`；`doc/issues-and-requirements.md` R19/R20 对应。图表形态待用户确认后定稿。

---

## D24：托盘常驻化 + 单实例保护 + 退出确认 + 气泡开关（2026-09-01，D21 P2 短期落地）

- **决策**：将 D21 P2 短期规划的"托盘+可配置"配置项由只读规划转为落地。五项改动全部沿用既有纯 Win32（D1）与注入/广播模式，不引入新 crate：
  1. **Windows 子系统 + 默认托盘常驻、零窗口启动**：`main.rs` 首行 `#![windows_subsystem = "windows"]`（无控制台，删除 D13 之后的 Ctrl+C 优雅退出 handler——无控制台即无 `SetConsoleCtrlHandler` 可挂）。程序启动不再显示任何窗口，仅创建隐藏窗口（热键/事件中转）+ 托盘图标；
  2. **每次启动弹托盘气泡**（设置项 `show_balloon` 可关）：纯函数 `should_show_balloon(no_tray, show_balloon)`（`sys/tray.rs`）判定；为 `true` 时经一次性定时器 `TIMER_BALLOON=0x1235` 延迟 1500ms（等消息循环就绪，避免图标注册初期 `NIM_MODIFY` 气泡被 shell 丢弃），`WM_TIMER` 分支 `KillTimer` 后调 `sys::tray::show_balloon`（`NIF_INFO` + `NIM_MODIFY`）；
  3. **`--no-tray` 命令行参数**：`parse_cli_no_tray`（逐项 `OsString == "--no-tray"` 相等比较，不做前缀匹配）禁用托盘图标与气泡；此模式下退出走概览面板底部"退出"按钮（`IDC_EXIT`，`WM_APP_EXIT` 请求）；启动时 `NO_TRAY` 全局注入，`TaskbarCreated` 重注册分支与 `request_exit` 的 `remove_tray` 均按其判断；
  4. **有标签退出弹定制主题确认窗**：`request_exit` 经纯函数 `should_confirm_exit(has_tags, confirmed)` 判定——有标签且未确认时调 `ui::confirm::create_confirm`（新 `src/ui/confirm.rs`，`WinTagConfirm` 窗口类 + `ConfirmData`，自绘暗色/圆角/自绘按钮 + 回车确认/Esc 取消/Tab 循环，仿 popup 模板）；确认窗"退出"按钮回投 `WM_APP_EXIT(wParam=1)` 完成确认；确认或无标签时 `remove_tray`（--no-tray 时跳过）+ `PostMessageW(WM_QUIT)` 走正常收尾。退出入口三处：托盘右键菜单"退出"、面板"退出"按钮（wParam=0）、确认窗"确定"（wParam=1）；
  5. **单实例保护**：用 `RegisterClassW("WinTag_SingleInstance")` 注册守护窗口类，失败且 `GetLastError() == ERROR_CLASS_ALREADY_EXISTS` 即判定另一实例在运行，直接 `return Ok(())`（退 0，不创建窗口/托盘）。**为何不用 `CreateMutexW`**：windows-rs 0.58 该 API 被 `Win32_Security` feature 门控（本项目未启用且本任务不得改动 Cargo.toml），守护类注册同为原子操作，且进程退出时窗口类自动注销，无需显式清理；
  6. **托盘图标 v1 用系统图标**：`load_tray_icon` 优先取传入窗口（隐藏窗）类小图标 `GCLP_HICONSM`，回退系统共享图标 `LoadIconW(None, IDI_WINLOGO)`（零新增 feature/资源，共享图标不 `DestroyIcon`）；
  7. **回调消息与消息路由**：`uCallbackMessage = WM_APP_TRAY = WM_APP+8`（`common/mod.rs`），托盘回调直达 `hidden_wndproc`；左键单击（`WM_LBUTTONUP=0x0202`）与气泡点击（`NIN_BALLOONUSERCLICK`）经纯函数 `tray_command_from_lparam` 解码为 `TrayCommand::OpenPanel`（打开概览面板）；右键（`WM_RBUTTONUP=0x0205`）在 `WM_RBUTTONUP` 分支 `show_context_menu`（`TrackPopupMenu` + `TPM_RETURNCMD`，四项：打开概览面板/打开设置页/快速标记/退出，取消失败回退 OpenPanel 绝不低于退）；`dispatch_tray_command` 由 main 接 ui（镜像热键分发语义）；
  8. **explorer 重启恢复**：`RegisterWindowMessageW("TaskbarCreated")` 注册动态消息，`hidden_wndproc` 匹配该消息且 `--no-tray` 关闭时重新 `add_tray`（失败登记已知限制）；
  9. **`show_balloon` 持久化**：`Settings` 增 `show_balloon: bool`（`#[serde(default = "default_true")]` 默认 `true`，旧配置缺字段回退 true），持久化于 config.toml；设置页新增"气泡提示"复选框（`IDC_BALLOON_CHECK`，保存/回显与主题/角标开关同路径）；`sys::tray::set_balloon_enabled` 经 `reapply_theme`（`WM_APP_THEME_CHANGED`）热注入，镜像 `set_show_title`/`set_badge_always_top` 模式。
- **背景**：D21 规划 P2 短期托盘+可配置、D22 登记托盘设计（Shell_NotifyIconW 方案、TaskbarCreated、图标 v1 用系统图标），但 D22 落地时仅实现配置路径解析链，托盘本体尚未落地。用户需求 4 明确要求"创建托盘替代纯命令行常驻"；同时需要单实例保护（防多实例热键/托盘冲突）与有标签退出确认（防误丢会话内标签）。
- **备选方案**：
  1. 托盘图标改用真实 `.ico`（`build.rs` winresource `.icon` + `LoadIconW`）（备选：D22 已给出升级路径，v1 先系统图标，后续升级）；
  2. 单实例用 `CreateMutexW`（否决：windows-rs 0.58 该 API 被 `Win32_Security` feature 门控，项目未启用且本任务不改 Cargo.toml）；
  3. 退出确认用 `MessageBoxW`（否决：与整体定制主题 UI 割裂；`ui::confirm` 仿 popup 模板成本低且主题一致）；
  4. 气泡不延迟直接弹（否决：实测图标注册初期 `NIM_MODIFY` 气泡被 shell 丢弃，1500ms 定时器等循环就绪更稳）。
- **理由**：
  1. 零窗口启动 + 托盘常驻是"程序常驻后台"（F3）的核心体验，`--no-tray` 保留纯命令行/开发模式；
  2. 单实例保护避免多实例同时注册热键（后者会静默失败）与托盘图标互相覆盖，守护类方案零额外依赖且进程退出自动清理；
  3. 有标签退出确认把"退出即清空会话数据"的不可逆操作前置一次确认，`should_confirm_exit` 纯函数可单测；
  4. 气泡开关走既有 `#[serde(default)]` + `reapply_theme` 注入链路，设置页交互与角标开关同构，改动面最小。
- **影响与后续跟进**：新增 `src/sys/tray.rs`（D22 落地：`add_tray`/`remove_tray`/`show_balloon`/`show_context_menu`/`register_taskbar_created` + `TrayCommand` 枚举 + 纯函数层）、`src/ui/confirm.rs`；`common/mod.rs` 补 `WM_APP_TRAY=WM_APP+8`、`WM_APP_EXIT=WM_APP+9`（原 D21 规划 `WM_APP+9=WM_APP_HOTKEYS_CHANGED` 顺延至 D23 落地时占用）；`Settings` 增 `show_balloon`（新增单测：默认 true、旧配置缺字段回退、false 往返持久化）；托盘菜单"打开概览面板"同时是 `--no-tray` 之外的主入口。遗留：真实 `.ico` 图标升级、explorer 重启恢复失败的已知限制（D22 登记）；F3（托盘图标）遗留项随本决策关闭。

---

## D25：底部控件客户区高度折算（layout::TITLEBAR_H / client_height，2026-09-01）

- **决策**：把「`WS_OVERLAPPEDWINDOW` 窗口外高 → 客户区高度」的折算统一收口到 `ui::layout`——新增常量 `TITLEBAR_H = 30`（设计像素，Win11 默认标题栏 + 边框的近似高度）与函数 `client_height(hwnd, window_h_design) = dp(window_h_design) - dp(TITLEBAR_H)`，并令三个顶层浮窗（`confirm`/`popup`/`settings`）的底部按钮行统一以此客户区高度定位。
- **背景**：`WS_OVERLAPPEDWINDOW` 的外高 = 客户区高 + 标题栏 + 边框，底部控件必须以**客户区高**为基准。此前三个窗口各自内联「-30」且不一致：`popup` 写 `dp(hwnd, WIN_H) - dp(hwnd, 30)`，`settings` 写 `WIN_H - dp(hwnd, 30)`（混用「未缩放外高 + 缩放标题栏」，是高 DPI 下的潜在隐患）；而新建的 `confirm.rs` 直接以窗口外高当客户区高度、未做任何抵扣——退出确认窗的「退出/取消」按钮因此被排到客户区底部之下，渲染时被裁切（实测截图：橙色按钮贴着窗口下缘且被剪掉一半）。三处重复且漂移的魔法数是同一类布局缺陷的温床。
- **备选方案**：
  1. 各窗继续内联 `-30`（否决：魔法数三处漂移，`confirm` 已漏扣造成可见裁切，`settings` 未缩放外高混用是潜在高 DPI 隐患）；
  2. `WM_NCCALCSIZE` 去边框 + 自定义标题栏 / 全量无边框（否决：会打破 `confirm` 与 `popup`/`panel`/`settings` 既有的「原生标题栏 + DWM 沉浸式暗色」统一外观，需自处理拖拽/缩放/标题绘制，改动面远超本次修复范围）。
- **理由**：
  1. 收口到单点后，「客户区高度」只有唯一权威来源，新窗口不会再漏扣或写错基准；
  2. `client_height` 为基于 `dp` 的纯函数，可直接单测、无窗口句柄之外的副作用；
  3. 对固定尺寸浮窗，从外高抵扣标题栏是最低成本且正确的做法——`confirm`/`popup`/`settings` 均固定尺寸（`WM_GETMINMAXINFO` 锁定）且以 `WIN_H` 为外高基准，语义一致。
- **影响与后续跟进**：`src/ui/layout.rs` 增 `TITLEBAR_H`/`client_height`（保留既有 `dp`，新增 1 个 `ignore` doctest）；`confirm.rs` 改 `client_h = client_height(hwnd, WIN_H)` 定位按钮行与消息区，修复退出确认窗按钮溢出裁切；`popup.rs`/`settings.rs` 改用同一函数（`popup` 行为完全一致，`settings` 一并修正高 DPI 外高未缩放的隐患）。`cargo build`/`clippy -D warnings`/`fmt --check`/`test` 全绿。

---

## D26：托盘底层迁移至 tray-icon(tauri) + 气泡迁至 notify-rust（2026-09-01）

- **决策**：把 `sys/tray.rs` 从手写 `Shell_NotifyIconW` + `NOTIFYICONDATAW` + `CreatePopupMenu`/`AppendMenuW`/`TrackPopupMenu`（D22/D24 落地）整体迁至 [`tray-icon`](https://crates.io/crates/tray-icon)（tauri）——`TrayIconBuilder` + `Menu(MenuItem)` + 嵌入资源图标；图标点击/菜单选择经 `TrayIconEvent`/`MenuEvent` 静态 crossbeam 通道投递，由主线程消息循环 `GetMessageW` 返回后非阻塞 `try_recv` 排空并分发。启动气泡从 `NIF_INFO`+`NIM_MODIFY` 迁至 [`notify-rust`](https://crates.io/crates/notify-rust)（Windows 走 `tauri-winrt-notification` 的 WinRT TOAST）。
- **背景**：手写托盘层要处理的东西过多（约 10 项职责：图标加载、NOTIFYICONDATAW 填充、UTF-16 截断、回调消息解码、右键菜单四项、气泡弹出、TaskbarCreated 重注册、移除、`--no-tray` 三分支、纯函数单测），且与主线程消息泵 / 注入模式 / 退出流深度耦合，改动一处要联动四处。迁移评估结论：底层换库、上层架构不破坏——保留 `TrayCommand` 纯逻辑层与 `dispatch_tray_command`，仅替换托盘实现末端。
- **备选方案**：
  1. 迁至 `tray-item`（olback）（否决：0.10.0 在 docs.rs 构建失败、文档缺失，事件同样跨线程，Windows 图标要求 `.rc` 打包，且与 `windows 0.58` 并存两套绑定）；
  2. 继续手写仅做轻度重构（否决：仍保留全部 Win32 手写面与 unsafe，治标不治本）；
  3. `winrt-notification` 做气泡（否决：依赖旧 `windows 0.24`，类型与项目 `windows 0.58` 不互通）。
- **理由**：
  1. `tray-icon`（tauri）维护活跃、API 更现代，事件模型经通道与现有"主线程统一消息泵"契合（事件写通道在 `DispatchMessageW` 调起其内部 WndProc 时发生，主循环天然唤醒）；
  2. 图标取自嵌入资源（build.rs `winresource::set_icon` 写 ID=1，`Icon::from_resource(1, None)` 读取），消除"共享类图标/IDI_WINLOGO 不销毁"的取巧语义，`exe` 资源管理器里也有正式应用图标；
  3. `TaskbarCreated`（explorer 重启）由 tray-icon 窗口过程内部接管重注册（`ChangeWindowMessageFilterEx` + 静态注册），删去本项目自实现；
  4. 净删 ~250 行 Win32 手写，unsafe 块由 ~10 处收敛到 ~2 处；`TrayCommand` 纯逻辑层与 `dispatch_tray_command` 原样复用，单测保留并新增事件解码用例。
- **线程模型**：tray-icon 要求托盘与 Win32 事件循环同线程创建——主线程 `GetMessageW` 循环恰好满足，故零新增线程；`TrayIcon` 参考计数、最后实例 drop 自动移除，`create_tray` 返回句柄以 main 局部变量持有至退出。
- **影响与后续跟进**：
  - `Cargo.toml` 增 `tray-icon = "0.24"`、`notify-rust = "4"`（新增 ~150 传递依赖：`muda`、`windows-sys 0.61`、`crossbeam-channel`、`png` 等；与项目 `windows 0.58` 两套 Win32 绑定并存，无运行时冲突）；
  - 新增 `assets/icon.ico`（仅新资源文件，Python 脚本生成 16/32/48/256 多尺寸 PNG 压缩 ICO）；`build.rs` 用 `winresource::set_icon` 嵌入；文件缺失时降级跳过（图标缺失仅影响外观）；
  - `src/sys/tray.rs` 重写为「纯逻辑层（`TrayCommand` + `icon_event_to_command`/`menu_id_to_command`/`should_show_balloon`，零 tray-icon 依赖）+ 适配层（`create_tray`/`set_balloon_enabled`/`show_balloon`）」，删除全部 `Shell_NotifyIconW`/`fill_wide`/`show_context_menu`/`register_taskbar_created`/Win32 `load_tray_icon`；
  - `src/main.rs`：删 `WM_APP_TRAY` 分支与 `TaskbarCreated` 分支（`registered_msg`）、删 `TASKBAR_CREATED`/`NO_TRAY` 静态与 `no_tray_active`/`remove_tray`，改在主循环 `poll_tray_events(hwnd)` 轮询两通道后 `dispatch_tray_command`；`request_exit` 不再显式移除托盘（`TrayIcon` drop 承担）；启动气泡定时器保留，`show_balloon` 改 notify-rust；
  - `src/common/mod.rs`：删 `WM_APP_TRAY` 常量（`WM_APP+8` 空出），`WM_APP_EXIT`（`WM_APP+9`）保留；
  - `cargo build`/`clippy -D warnings`/`fmt --check`/`test` 全绿（新增 4 个托盘单测用例）。

---

## D27：四个图形界面窗口迁至 iced（tiny-skia 渲染器 + 独立线程通道通信，2026-09-01）

- **决策**：把 `ui/panel`（概览面板）、`ui/popup`（标签编辑弹窗）、`ui/settings`（设置页）、`ui/confirm`（退出确认）四个**自绘 Win32 窗口**迁至 [iced](https://github.com/iced-rs/iced)（`tiny-skia` 软件渲染器），用其声明式控件 + 主题 + 高 DPI 取代手写的 `WNDCLASSW`/`CreateWindowExW`/`WM_CTLCOLOR*`/`WM_DRAWITEM`/`BS_OWNERDRAW` 自绘/子类化/控件布局。**其余层面保持纯 Win32 不变**：
  - `sys/overlay`（透明覆盖层/角标/标题条/tooltip）、`sys/tray`（托盘）、`sys/win_event`、`sys/window`、`hotkey`、`main.rs` Win32 消息泵——**均不迁移**；
  - 架构改为 **iced 跑在独立线程、主线程保留 `GetMessageW` 消息泵，二者经 crossbeam 通道双向通信**（镜像 D26 `tray-icon` 的通道轮询模式）；
  - `ui/button.rs`、`ui/layout.rs` 整体删除；`ui/theme.rs` 拆分为「纯调色板常量（留供覆盖层注入 + iced 主题映射）+ DWM `apply_dark_mode`/`apply_corner_preference`（保留给原生覆盖层/隐藏窗口）」；
  - **机制收敛（总线最小化）**：为约束「跨线程/跨层接口不随迁入 iced 而膨胀」，D27 同时落地四条约束——① 7 个 `set_*` 注入设置器（`set_tag_store`/`set_message_target`/`set_tooltip_theme`/`set_show_title`/`set_badge_always_top`/`set_balloon_enabled`/`set_global_settings`）收敛为**一个 `NativePrefs` 纯值结构**（`show_title`/`badge_always_top`/`tooltip_theme`/`balloon_enabled`）+ **一个 `NativeBridge`**（封装覆盖层「上报 edit_tag / 激活窗口」的回传能力），主线程一次性注入，消除「改一条 UI 偏好要同步改 N 个设置器」的牵连；② 主循环散落的 `try_recv`（tray×2 + iced×1）收进**单一 `pump_background_events(hwnd)` 阶段**，循环体读起来是「来源 → 动作」一张表；③ **托盘不绕 iced**：托盘是原生层、只与主线程对话，iced 只管理四个面板，二者不直连、iced 不拥有 overlay/tray/hotkey；④ iced 侧**不镜像 `WM_APP_*` 的 UI 消息**——原生消息只负责「进主循环」，出主循环一律走 iced 通道，同一条消息只有一个出口、不双写。
- **背景**：WinTag 的图形界面由四套手写 Win32 窗口构成，合计约 4000 行（panel 1511 / popup 1111 / settings 965 / confirm 469）+ `button.rs` 401 + `theme.rs` 624。每一套都重复实现窗口类注册、`WM_CREATE` 建子控件、`WM_CTLCOLOR*` 配色、`WM_ERASEBKGND` 背景、`WM_GETMINMAXINFO` 固定尺寸、`WM_DRAWITEM` 自绘（按钮/颜色块/下拉框）、Tab/Esc/回车键盘语义、DPI 缩放（`ui::layout::dp`）。这种「一窗一个 WndProc 全手写」的模式，在 D11-D25 期间**每一轮都要跨四窗重复修相同的问题**（D18 配色 `wParam`/`lParam` 误用、D25 客户区高度漏扣、dark-mode 控件变体 `apply_control_theme` 四处调用）。iced 恰好把这类样板收敛为声明式控件，并补上目前四窗缺少的现代能力（文本自适应、平滑滚动、可移植主题）。
- **备选方案**：
  1. **egui/eframe**（否决）：D1 已明确移除 egui；egui 即时模式对「树形面板/搜索框/下拉框」的控件语义不如 elm-architecture 的 iced 清晰，且即时模式与 Win32 原生窗口共存时易重绘漂移；
  2. **tauri + WebView**（否决）：引入完整前端链，对 4 个面板过重，与 WinTag「轻量常驻 + 零浏览器进程」定位冲突；`--no-tray` 纯命令行模式也不应背着 WebView；
  3. **winit + softbuffer**（否决）：只换窗口壳，仍要手写全部控件/样式/布局，不解决重复劳动根因；
  4. **继续手写（纯重构收口）**（否决）：即便把 theme/layout/button 收口成共享工具，四个窗口仍是 4000+ 行手写 WndProc，D18/D25 那类跨窗同病仍会复发；
  5. **iced 全量替换**（否决）：覆盖层/托盘/热键/事件监听无法用 iced 表达（iced/winit 整窗模型无法在外部窗口上做逐像素透明层——正是 D1 否决 winit 的理由）；`Application::run` 自带消息泵会与现有 `GetMessageW` 主循环冲突；
  6. **iced 跑独立线程 + 通道（采纳）**：只移植四窗，复用 D26 已验证的跨线程通道分发模式。
- **理由**：
  1. **边界干净**：覆盖层/托盘/热键/事件监听是「在外部窗口上打标」的产品核心，必须留在 Win32；四窗是纯「模态/常驻窗口」，与 iced 控件语义一一对应。边界正好落在「是否需要贴目标窗口」；
  2. **命中重复痛点**：D11-D25 每次修复都在四窗重复铺开，iced 一次消灭这类样板代码；后续 D23 的「分组下拉/快捷键录制/图表」可直接用 iced 控件实现；
  3. **线程冲突可控**：`Application::run` 阻塞一个线程，主线程消息泵不受影响；`TagStore`/`Settings` 已是 `Arc<Mutex>`（Send），跨线程安全；通道协议与 D26 `TrayCommand` 纯逻辑层同构；
  4. **性能匹配**：四窗为低频交互窗口，`tiny-skia` 无 GPU 初始化、二进制小、无 wgpu 设备枚举，适合常驻托盘工具；
  5. **保留 D1 的有效部分**：D1 否决 winit 的三条理由针对覆盖层与消息泵；迁四窗并不违背其精神，overlay/tray/win_event/hotkey/main 消息泵仍是纯 Win32。
- **影响与后续跟进**：
  - `Cargo.toml` 增 `iced`（`tiny-skia` 渲染器，版本按 MSRV 与稳定版校验后定稿）与 `crossbeam-channel`（已随 tray-icon 传递，补显式声明；与项目 `windows 0.58`、tray-icon 的 `windows-sys 0.61` 两套 Win32 绑定并存，无运行时冲突）；
  - 新增 `src/ui/iced_proto.rs`：纯协议层（无 iced/Win32 依赖），`IcedCommand`（主→iced：`ShowPanel`/`HidePanel`/`OpenSettings`/`EditTag`/`RefreshTags`/`ApplyTheme`/`ShowConfirm`/`CloseConfirm`）与 `GuiEvent`（iced→主：`TagSaved`/`SettingsChanged`/`EditTagRequested`/`ActivateWindow`/`RemoveTag`/`ExpandAll`/`CollapseAll`/`ConfirmExit`/`CancelExit`/`PanelVisibilityChanged`）；`Tag`/`Settings`/`TagColor` 均 `Clone+Send` 可跨线程；
  - 新增 `src/ui/iced_app.rs`：`iced::Application` 实现 + 多窗口（panel/settings/popup/confirm），`subscription` 经 `from_main_rx.unfold()` 消费 `IcedCommand`，`update` 产出 `GuiEvent` 经发送器回主线程；
  - `main.rs`：删 `PANEL_HWND`/`settings_hwnd`/`ensure_settings_window`/`toggle_settings` 的窗口句柄路径，改由「面板/设置可见状态 + 发送 IcedCommand」；`handle_quick_tag`/`WM_APP_EDIT_TAG`/托盘 `QuickTag` 的 `create_popup` 改为发 `EditTag`（`clamp_to_work` 光标定位保留在主线程计算）；`WM_APP_TAGS_CHANGED` 改发 `RefreshTags`；`poll_tray_events`/`dispatch_tray_command`/`handle_winevent` 的 UI 出口改 `tx_iced.send(...)`；`reapply_theme` 在原生覆盖层重注入外补发 `ApplyTheme`；新增 `pump_background_events(hwnd)`（收拢 tray×2 + iced×1 的 `try_recv`，替代原 `poll_tray_events`；后续 D23 若加图表/快捷键窗口，均复用同一对 `IcedCommand`/`GuiEvent`，**不新增通道**）；
  - `request_exit` 的 `create_confirm` → `ShowConfirm{count}`；`ConfirmExit` 回主线程走既有 `WM_APP_EXIT(wParam=1)` 退出流（复用 `should_confirm_exit`）；
  - 删除 `src/ui/button.rs`、`src/ui/layout.rs`；`ui/theme.rs` 拆分（纯调色板 `light_colors`/`dark_colors`/`blend` 留作覆盖层 `set_tooltip_theme` 注入 + iced 主题映射；DWM `apply_*` 保留）；sys 层注入点收敛为 `NativePrefs`/`NativeBridge` 各一次；
  - **回归面**：Tab 循环、Esc 取消、回车保存/确认、颜色块选择、树形列表展开、右键置前/编辑/移除——迁到 iced 后是**行为重写**而非逐行搬运，需在 `tests/smoke.rs` 与手动验收单逐项覆盖；
  - `doc/architecture.md` 按「一层 UI 由 iced 承担 + 覆盖层/托盘仍由 sys 层 Win32 承担」更新分层图与模块职责；`doc/iced-migration.md` 新增**分阶段执行方案**（里程碑、逐阶段任务、文件级改动、验证门禁、可维护性约定）；`AGENTS.md` 同步；
  - `cargo build`/`clippy -D warnings`/`fmt --check`/`test` 全绿（本决策仅登记设计；分阶段实施见 `doc/iced-migration.md`）。

---
