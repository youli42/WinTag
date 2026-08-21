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

- **决策**：窗口事件监听使用 `SetWinEventHook`，固定 `WINEVENT_OUTOFCONTEXT` + `WINEVENT_SKIPOWNPROCESS` 标志，在主线程（隐藏窗口创建之后、消息循环之前）注册。监听拆分为两个 hook：系统事件段 `0x0003..=0x0017` 与对象事件段 `0x8001..=0x8018`（见 `src/sys/win_event.rs` 第 114 行起 `install`）。回调只做过滤与 `PostMessageW(WM_APP_WINEVENT)` 转发，不执行任何重活。
- **背景**：WinEvent 回调的执行线程取决于注册线程与进程上下文。若在独立线程注册，回调将跑在该线程的消息泵内，需要引入跨线程状态（Send/Sync、Arc 传递、生命周期管理）。
- **备选方案**：
  1. 独立线程注册 hook，回调内直接处理或经 channel 转发；
  2. 主线程注册，回调内只转发消息（采纳）。
- **理由**：
  1. 主线程本就是消息泵，`GetMessageW` 会处理本线程所有窗口消息（见 `src/main.rs` 第 98 行起），回调投递的 `WM_APP_WINEVENT` 自然回到同一线程处理，无需第二个线程，省去 Send/Sync 与生命周期成本；
  2. 覆盖层、面板、弹窗全部在主线程创建与访问，事件处理在主线程落地与现有架构完全一致；
  3. `WINEVENT_SKIPOWNPROCESS` 避免收到本进程（隐藏窗口、覆盖层等）自身产生的事件形成自激循环；
  4. 回调内只做 `should_forward` 过滤（`OBJID_WINDOW` + `CHILDID_SELF`，见 `src/sys/win_event.rs` 第 78 行起）与轻量 `PostMessageW`，不触碰布局/重绘 API，避免阻塞 USER 队列。
- **影响与后续跟进**：事件处理全部收敛到 `src/main.rs` 的 `handle_winevent`，逻辑单一。`WinEventHooks` 实现 `Drop` 自动注销（第 126 行起）。两个 hook 全部安装失败时 `degraded` 标志置位，退化到纯轮询模式。后续若需监听更多事件段，需评估系统段与对象段的边界划分是否继续成立。

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
- **背景**：WinEvent 事件存在丢失可能（hook 安装失败、进程崩溃、UWP 事件不完整），且最小化窗口的可见性判断容易误判。若完全依赖事件驱动，覆盖层可能停在错误位置或残留为孤儿。
- **备选方案**：
  1. 纯事件驱动，不做轮询；
  2. 纯轮询，放弃事件监听；
  3. 事件驱动为主 + 低频兜底轮询（采纳）。
- **理由**：
  1. tacky-borders 等项目的实践中，纯事件驱动在事件丢失时覆盖层会漂移，需要用轮询兜底；
  2. 事件驱动保证移动/缩放时的即时响应（毫秒级），500ms 轮询负责纠正漏网之鱼，两者成本互补；
  3. `sync_position` 内部带短路守卫（最小化或不可见直接返回）与变更去重（`last_rect` 矩形一致时跳过 `SetWindowPos`），轮询开销被压到最低（见 `src/sys/overlay.rs` 第 201 行起）；
  4. 两个 WinEvent hook 全部安装失败时 `is_degraded()` 为真，轮询成为唯一同步路径，系统仍可用（见 `src/main.rs` 第 75 行起打印降级提示）。
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
| F3 | **托盘图标** | development-plan.md 阶段一未勾选项"托盘图标，程序常驻后台" | 未实现。程序无系统托盘入口，退出需结束进程 | 低 | `Shell_NotifyIcon` 托盘图标 + 右键菜单（开关覆盖层/退出）；依赖需评估 tray-icon crate（曾因不引第三方否决，可复核） |
| F4 | **管理员权限窗口兼容** | technical-specs.md 边缘情况表 | 部分受限。程序默认非提权运行，UIPI 会拦截来自提权窗口的 WinEvent，覆盖层无法跟随管理员窗口；`AGENTS.md` 已注明"可能需要管理员权限" | 低 | 文档化限制 + 可选以管理员身份重启（无自动提权） |
| F5 | **热键冲突处理** | 健壮性 | ✅ 已修复（提交 `37419ed` fix(hotkey)）：改为逐个注册并记录失败项，冲突时降级（仅注册成功的热键）并打印提示，不再整体退出；同时新增设置页热键 `OpenSettings`（`Ctrl+Shift+S`） | 中 | 已完成，本条关闭 |
| F6 | **用户配置（热键自定义/默认颜色）** | development-plan.md 阶段三未勾选项 | ✅ 已实现（`170ce92` 起）：配置数据模型与 TOML 持久化（`%APPDATA%\WinTag\config.toml`），设置页支持主题/圆角选择并保存（见 [D10](#d10暗色主题与圆角采用-win32-原生方案配置持久化到-toml)）；热键编辑 UI 与默认颜色自定义待后续（见 issues-and-requirements.md R1） | 低 | 已完成（部分）：配置持久化落地，热键编辑 UI 待 R1 |

**登记说明**：F1 为需求文档明确功能（重构前已存在缺口，非本次回归）；F2/F3/F4 为技术规格声明的边缘处理；F5 为本次审计新发现；F6 为开发计划未勾选项。均在本次重构范围之外，重构目标（位置同步/DPI/跳转/预填/清理/健壮性）不受影响。

**更新（2026-08-21）**：F5（热键冲突处理）与 F6（用户配置）已随 T9 收尾落地，表中状态已更新；遗留项收敛为 F1（窗口激活闪烁反馈）、F2（全屏应用降级）、F3（托盘图标）、F4（管理员权限窗口兼容）。
