//! 系统级共享工具层（叶子模块，无任何项目内依赖）
//!
//! 本模块只依赖 windows-rs 与标准库，不依赖项目内其他模块
//! （core / sys / ui / hotkey），供任意模块按需复用。当前提供：
//!
//! - 自定义窗口消息常量：覆盖层创建 / 销毁、WinEvent 事件转发、设置窗口、主题变更
//! - [`widestring`]：UTF-16 宽字符串转换
//! - [`get_userdata`] / [`set_userdata`]：窗口用户数据（GWLP_USERDATA）读写
//!
//! 依赖方向约定：`任意模块 → common`，禁止反向依赖。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, GWLP_USERDATA, WM_APP,
};

/// 自定义消息：创建覆盖层（wParam = 目标窗口句柄）
///
/// 由主线程通过 `PostMessage` 发送到隐藏窗口，请求为目标窗口创建透明覆盖层。
pub const WM_CREATE_OVERLAY: u32 = WM_APP + 1;

/// 自定义消息：销毁覆盖层（wParam = 目标窗口句柄）
///
/// 由主线程通过 `PostMessage` 发送到隐藏窗口，请求销毁目标窗口的覆盖层。
pub const WM_DESTROY_OVERLAY: u32 = WM_APP + 2;

/// 自定义消息：WinEvent 事件转发（wParam/lParam 携带事件参数）
///
/// 用于将 `SetWinEventHook` 回调中捕获的事件转发到主线程消息循环统一处理。
pub const WM_APP_WINEVENT: u32 = WM_APP + 3;

/// 自定义消息：打开设置窗口
///
/// 由主线程通过 `PostMessage` 发送到隐藏窗口，请求创建并展示设置窗口。
pub const WM_APP_OPEN_SETTINGS: u32 = WM_APP + 4;

/// 自定义消息：主题变更广播（重新应用主题）
///
/// 用于主题状态变化后向主线程广播，重新应用暗色/浅色主题到各窗口。
pub const WM_APP_THEME_CHANGED: u32 = WM_APP + 5;

/// 将字符串编码为以 NUL（`\0`）结尾的 UTF-16 宽字符串
///
/// 用于向 Win32 API 传入窗口类名、窗口标题等宽字符串参数，
/// 返回值可直接通过 `PCWSTR(ptr)` 或 `PCWSTR(as_ptr())` 使用。
pub fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读取窗口的用户数据指针（GWLP_USERDATA）
///
/// 直接通过 `GetWindowLongPtrW` 读取，取代旧的
/// "先置 0 再恢复原值"（`SetWindowLongPtrW` + restore）反模式，
/// 无副作用且语义等价。
///
/// # Safety
///
/// - 仅在主线程消息循环内调用（WndProc 或同线程代码）；
/// - 窗口生命周期由调用方保证：窗口销毁后不得再调用本函数访问其用户数据；
/// - 返回指针指向数据的生命周期同样由调用方管理，解引用前必须保证数据仍存活。
pub unsafe fn get_userdata<T>(hwnd: HWND) -> *mut T {
    // SAFETY: 调用方保证 hwnd 存活且本函数仅在主线程消息循环内调用；
    // GetWindowLongPtrW 为无副作用读取，返回的指针由调用方负责生命周期管理。
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut T
}

/// 写入窗口的用户数据指针（GWLP_USERDATA）
///
/// 供覆盖层 / 面板 / 弹窗在窗口创建后关联自身状态数据，
/// 由各自 WndProc 在收到消息时通过 [`get_userdata`] 取回。
///
/// # Safety
///
/// - 仅在主线程消息循环内调用（WndProc 或同线程代码）；
/// - 窗口生命周期由调用方保证：窗口销毁前必须置空（传入 `null_mut`）或释放，
///   避免 WndProc 后续访问悬垂指针；
/// - `data` 指向数据的生命周期由调用方管理，须存活至再次写入或窗口销毁。
pub unsafe fn set_userdata(hwnd: HWND, data: *mut std::ffi::c_void) {
    // SAFETY: 调用方保证 hwnd 存活且本函数仅在主线程消息循环内调用；
    // data 的生命周期由调用方保证，窗口销毁前须置空或释放，防止悬垂指针。
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize);
}
