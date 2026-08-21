//! Win32 原生暗色主题与圆角支持模块（任务 T2）
//!
//! 通过 DWM API（`DwmSetWindowAttribute`）切换窗口的沉浸式暗色模式（`DWMWA_USE_IMMERSIVE_DARK_MODE`）
//! 与圆角偏好（`DWMWA_WINDOW_CORNER_PREFERENCE`），并通过 `WM_CTLCOLOR` 画刷机制
//! （`CreateSolidBrush` + 进程级画刷缓存）为各窗口/控件提供明暗双调色板。
//!
//! 主题来源：支持注册表检测（`HKCU\...\Personalize\AppsUseLightTheme`）的系统深浅色自动跟随，
//! 注册表不可用时回退 `GetSysColor(COLOR_WINDOW)` 亮度启发式。

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, COLORREF, ERROR_SUCCESS, HWND};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWINDOWATTRIBUTE,
};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, GetSysColor, COLOR_WINDOW, HBRUSH};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, REG_DWORD, REG_VALUE_TYPE, RRF_RT_REG_DWORD,
};

use crate::core::settings::{CornerPreference, ThemeMode};

/// 主题调色板（所有颜色均为 BGR 格式的 `COLORREF`）
///
/// 供 `WM_CTLCOLOR*` 系列消息与 DWM 属性设置使用；字段名与
/// Win32 控件着色约定一一对应（背景 / 前景 / 编辑框 / 工具提示 / 列表视图）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeColors {
    /// 窗口背景色（BGR）
    pub bg: COLORREF,
    /// 窗口前景（默认文本）颜色（BGR）
    pub fg: COLORREF,
    /// 编辑框背景色（BGR）
    pub edit_bg: COLORREF,
    /// 编辑框前景（文本）颜色（BGR）
    pub edit_fg: COLORREF,
    /// 工具提示（tooltip）背景色（BGR）
    pub tooltip_bg: COLORREF,
    /// 工具提示前景（文本）颜色（BGR）
    pub tooltip_fg: COLORREF,
    /// 列表视图（概览面板列表）背景色（BGR）
    pub listview_bg: COLORREF,
    /// 列表视图前景（文本）颜色（BGR）
    pub listview_fg: COLORREF,
}

/// 亮色调色板
///
/// 参考 Windows 经典浅色：窗口背景近白、文本黑色、编辑框纯白底黑字、
/// 工具提示白底黑字、列表视图白底黑字。
pub fn light_colors() -> ThemeColors {
    ThemeColors {
        // 背景：近白（BGR 0x00F5F5F5，对应 RGB F5F5F5）
        bg: COLORREF(0x00F5F5F5),
        // 前景：纯黑
        fg: COLORREF(0x00000000),
        // 编辑框背景：纯白
        edit_bg: COLORREF(0x00FFFFFF),
        // 编辑框文本：纯黑
        edit_fg: COLORREF(0x00000000),
        // 工具提示背景：纯白
        tooltip_bg: COLORREF(0x00FFFFFF),
        // 工具提示文本：纯黑
        tooltip_fg: COLORREF(0x00000000),
        // 列表视图背景：纯白
        listview_bg: COLORREF(0x00FFFFFF),
        // 列表视图文本：纯黑
        listview_fg: COLORREF(0x00000000),
    }
}

/// 暗色调色板
///
/// 参考 Windows 深色：窗口背景深灰（#202020）、文本近白、编辑框 #2B2B2B 底白字、
/// 工具提示同编辑框、列表视图同窗口背景。
pub fn dark_colors() -> ThemeColors {
    ThemeColors {
        // 背景：深灰（BGR 0x00201F1E，对应 RGB 1E1F20 的近似深色）
        bg: COLORREF(0x00201F1E),
        // 前景：近白
        fg: COLORREF(0x00F3F3F3),
        // 编辑框背景：#2B2B2B
        edit_bg: COLORREF(0x002B2B2B),
        // 编辑框文本：纯白
        edit_fg: COLORREF(0x00FFFFFF),
        // 工具提示背景：#2B2B2B
        tooltip_bg: COLORREF(0x002B2B2B),
        // 工具提示文本：纯白
        tooltip_fg: COLORREF(0x00FFFFFF),
        // 列表视图背景：与窗口背景一致
        listview_bg: COLORREF(0x00201F1E),
        // 列表视图文本：近白
        listview_fg: COLORREF(0x00F3F3F3),
    }
}

/// 按主题模式与系统深浅色状态解析最终调色板（纯函数，无副作用）
///
/// - `ThemeMode::System`：跟随 `system_dark` 参数（调用方负责提供系统状态）
/// - `ThemeMode::Light`：恒为 [`light_colors`]
/// - `ThemeMode::Dark`：恒为 [`dark_colors`]
pub fn resolve_colors(theme: ThemeMode, system_dark: bool) -> ThemeColors {
    match theme {
        ThemeMode::System => {
            if system_dark {
                dark_colors()
            } else {
                light_colors()
            }
        }
        ThemeMode::Light => light_colors(),
        ThemeMode::Dark => dark_colors(),
    }
}

/// 检测系统当前是否处于深色模式
///
/// 两条路径，逐级回退：
///
/// 1. **注册表（首选）**：读取 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`
///    下的 `AppsUseLightTheme`（`REG_DWORD`），0 = 深色、1 = 浅色；
///    读取失败、类型不符或大小不符时进入路径 2。
/// 2. **`GetSysColor(COLOR_WINDOW)` 亮度启发式**：取窗口底色 BGR 的 R 分量（低字节），
///    R < 128 视为深色；`GetSysColor` 几乎不会失败，理论上失败时最终回退浅色（false）。
pub fn detect_system_dark() -> bool {
    // 路径 1：注册表 AppsUseLightTheme
    let subkey =
        windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let value_name = windows::core::w!("AppsUseLightTheme");
    let mut value: u32 = 0;
    let mut reg_type: REG_VALUE_TYPE = REG_VALUE_TYPE::default();
    let mut size = size_of::<u32>() as u32;
    // SAFETY: RegGetValueW 为线程安全标准 API；pvdata 指向栈上 u32 缓冲区，
    // pcbdata 指向栈上长度变量，均在调用期间存活；字符串为 'static 宽字面量。
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value_name.as_ptr()),
            RRF_RT_REG_DWORD,
            Some(&mut reg_type),
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    // 读取成功且类型/大小校验通过：0 = 深色、1 = 浅色
    if err == ERROR_SUCCESS && reg_type == REG_DWORD && size == size_of::<u32>() as u32 {
        return value == 0;
    }

    // 路径 2：GetSysColor 亮度启发式（取窗口底色 R 分量，<128 判暗）
    // SAFETY: GetSysColor 为线程安全标准 API，无失败路径，COLOR_WINDOW 为系统色索引常量。
    let window_color = unsafe { GetSysColor(COLOR_WINDOW) };
    let red = window_color & 0xFF;
    if red < 128 {
        return true;
    }
    false
}

/// 为窗口应用沉浸式暗色模式（DWM 属性 20），失败时回退旧版属性 19
///
/// 优先使用 `DWMWA_USE_IMMERSIVE_DARK_MODE`（属性值 20，Win10 20H1+）；
/// 若返回错误（如旧版系统不认识该属性），改用属性值 19
/// （`DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1`，旧版 Windows）。
/// 所有 `Err` 静默吞掉，仅以布尔值表达是否成功，绝不 panic。
///
/// # 参数
///
/// - `hwnd`：目标窗口句柄（须为有效窗口，由调用方保证）
/// - `dark`：true 应用暗色，false 应用亮色
pub fn apply_dark_mode(hwnd: HWND, dark: bool) -> bool {
    let value = BOOL(if dark { 1 } else { 0 });
    // SAFETY: hwnd 有效性由调用方保证；pvAttribute 指向栈上 BOOL，在
    // DwmSetWindowAttribute 调用期间存活；DwmSetWindowAttribute 为线程安全标准 API。
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &value as *const BOOL as *const c_void,
            size_of::<BOOL>() as u32,
        )
    };
    if result.is_ok() {
        return true;
    }

    // 旧版 Windows 回退：属性值 19
    // SAFETY: 同上方调用；属性值 19 为旧版沉浸式暗色模式常量，旧系统上可能仍失败，
    // 失败由 is_ok() 判定，无安全风险。
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(19),
            &value as *const BOOL as *const c_void,
            size_of::<BOOL>() as u32,
        )
    }
    .is_ok()
}

/// 为窗口应用圆角偏好（DWM 属性 33，Win11）
///
/// `DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ...)`，
/// 取值映射：Default→0（`DWMWCP_DEFAULT`）、Round→2（`DWMWCP_ROUND`）、
/// SmallRound→3（`DWMWCP_ROUNDSMALL`）。
///
/// Win10 及更早系统不支持该属性，`Err` 静默返回 false，不影响程序运行。
pub fn apply_corner_preference(hwnd: HWND, corner: CornerPreference) -> bool {
    let value: i32 = match corner {
        CornerPreference::Default => 0,    // DWMWCP_DEFAULT
        CornerPreference::Round => 2,      // DWMWCP_ROUND
        CornerPreference::SmallRound => 3, // DWMWCP_ROUNDSMALL
    };
    // SAFETY: hwnd 有效性由调用方保证；pvAttribute 指向栈上 i32，调用期间存活；
    // cbAttribute 固定 4 字节（i32）；失败（如 Win10 不支持）由 is_ok() 判定。
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &value as *const i32 as *const c_void,
            4,
        )
    }
    .is_ok()
}

/// 画刷缓存：`COLORREF` → `HBRUSH`，进程生命周期持有
///
/// `WM_CTLCOLOR*` 消息返回的画刷句柄必须在控件重绘期间保持有效，
/// 而控件可能在任意时刻重绘（且消息处理无法延迟持有画刷），因此
/// **缓存中的画刷永不删除**（不调用 `DeleteObject`），由进程退出时系统统一回收。
/// 若删除缓存画刷，下一次控件重绘将引用已释放的 GDI 对象，导致绘制失败或未定义行为。
///
/// 存储形式说明：
/// - windows-rs 0.58 的 `COLORREF` 未实现 `Hash`，无法直接作 HashMap 键，
///   故以其原始值 `color.0: u32` 作为键，语义等价；
/// - `HBRUSH` 内部为裸指针、未实现 `Send`，无法直接放入 `static Mutex`，
///   故缓存以 `brush.0 as usize`（句柄原始值）存储，取出时还原为 `HBRUSH`，
///   指针↔usize 往返转换在 Rust 中是良定义的。
static BRUSH_CACHE: OnceLock<Mutex<HashMap<u32, usize>>> = OnceLock::new();

/// 获取指定颜色的画刷（带进程级缓存）
///
/// 首次调用 `CreateSolidBrush` 创建并写入缓存；后续同色调用直接返回缓存画刷。
/// 锁中毒时绕过缓存直接创建新画刷（仍由进程生命周期持有，永不删除）。
pub fn get_brush(color: COLORREF) -> HBRUSH {
    let cache = BRUSH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut cache) = cache.lock() {
        if let Some(&raw) = cache.get(&color.0) {
            return HBRUSH(raw as *mut c_void);
        }
        // SAFETY: CreateSolidBrush 为线程安全标准 API；返回的画刷存入缓存，
        // 进程生命周期持有、永不删除（见 BRUSH_CACHE 文档注释）。
        let brush = unsafe { CreateSolidBrush(color) };
        cache.insert(color.0, brush.0 as usize);
        brush
    } else {
        // 锁中毒回退：直接创建，不缓存（调用方仍承诺不删除，进程持有）
        // SAFETY: 同上；单次创建无缓存，无并发问题。
        unsafe { CreateSolidBrush(color) }
    }
}

/// 全局主题状态（共享可变，供多窗口线程读取当前调色板）
static THEME_STATE: OnceLock<Arc<Mutex<ThemeColors>>> = OnceLock::new();

/// 设置全局主题调色板
///
/// 首次调用时初始化状态存储；此后每次调用覆盖为最新调色板。
/// 锁中毒（`Mutex` 被 panic 污染）时静默跳过写入，不影响后续调用。
pub fn set_theme(colors: ThemeColors) {
    let state = THEME_STATE.get_or_init(|| Arc::new(Mutex::new(colors)));
    if let Ok(mut state) = state.lock() {
        *state = colors;
    }
}

/// 读取当前全局主题调色板
///
/// 返回克隆值（`ThemeColors: Copy`）；从未设置过或锁中毒时返回 `None`。
pub fn theme_colors() -> Option<ThemeColors> {
    let state = THEME_STATE.get()?;
    state.lock().ok().map(|guard| *guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::ThemeMode;

    /// 明暗调色板各自与对应常量一致，且明暗关键字段值不同
    #[test]
    fn resolve_light_dark_match_palettes() {
        let light = resolve_colors(ThemeMode::Light, true);
        let dark = resolve_colors(ThemeMode::Dark, false);
        // Light 模式无视 system_dark 参数，恒等于亮色调色板
        assert_eq!(light, light_colors());
        // Dark 模式无视 system_dark 参数，恒等于暗色调色板
        assert_eq!(dark, dark_colors());
        // 明暗调色板字段值必须不同（否则主题切换无意义）
        assert_ne!(light.bg, dark.bg);
        assert_ne!(light.fg, dark.fg);
        assert_ne!(light.edit_bg, dark.edit_bg);
        assert_ne!(light.tooltip_bg, dark.tooltip_bg);
    }

    /// System 模式跟随 system_dark 参数选择明/暗调色板
    #[test]
    fn resolve_system_follows_system_dark() {
        assert_eq!(resolve_colors(ThemeMode::System, true), dark_colors());
        assert_eq!(resolve_colors(ThemeMode::System, false), light_colors());
    }

    /// 画刷缓存：同一颜色两次获取返回相同指针（缓存命中）
    ///
    /// 测试创建的画刷按设计进入进程级缓存且永不删除——测试进程退出时
    /// 由操作系统统一回收，可接受。
    #[test]
    fn get_brush_caches_same_pointer() {
        let color = COLORREF(0x00FFFFFF);
        let first = get_brush(color);
        let second = get_brush(color);
        // CreateSolidBrush 成功时返回有效画刷句柄（非 0 / 非 -1）
        assert!(!first.is_invalid());
        // 第二次调用命中缓存，返回与第一次相同的画刷句柄
        assert_eq!(first, second);
    }
}
