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
use windows::Win32::Foundation::{BOOL, COLORREF, ERROR_SUCCESS, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWINDOWATTRIBUTE,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, GetSysColor, COLOR_WINDOW, HBRUSH, HFONT,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, REG_DWORD, REG_VALUE_TYPE, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, SendMessageW, SystemParametersInfoW, NONCLIENTMETRICSW,
    SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_SETFONT,
};

use crate::core::settings::{CornerPreference, ThemeMode};

/// 主题调色板（所有颜色均为 BGR 格式的 `COLORREF`）
///
/// 供 `WM_CTLCOLOR*` 系列消息与 DWM 属性设置使用；字段名与
/// Win32 控件着色约定一一对应（背景 / 前景 / 编辑框 / 工具提示 / 列表视图），
/// 并附带自绘控件（按钮 / 列表行 / 边框 / 次要文字）所需的语义色。
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
    /// 强调色（主按钮底色、选中态基色，Material 橙）
    pub accent: COLORREF,
    /// 1px 边框 / 分隔线颜色（含 tooltip 描边）
    pub border: COLORREF,
    /// 悬停态底色（列表行 / 次要按钮）
    pub hover: COLORREF,
    /// 列表选中行底色（accent 低饱和混合）
    pub selected: COLORREF,
    /// 次要文字（说明 / 元信息）颜色
    pub muted: COLORREF,
    /// 列表奇偶行交替底色（与 listview_bg 微差）
    pub listview_alt_bg: COLORREF,
    /// 表头（SysHeader32）背景色
    pub header_bg: COLORREF,
    /// 表头（SysHeader32）文字颜色
    pub header_fg: COLORREF,
}

/// 亮色调色板
///
/// 参考 Windows 11 浅色：窗口背景近白、文本深灰（纯黑过于生硬）、
/// 编辑框纯白底深灰字、工具提示白底深灰字、列表视图白底深灰字，
/// 强调色为 Material 橙（RGB 251,140,0）。
pub fn light_colors() -> ThemeColors {
    ThemeColors {
        // 背景：近白
        bg: COLORREF(0x00F5F5F5),
        // 前景：深灰（比纯黑柔和）
        fg: COLORREF(0x001F1F1F),
        // 编辑框背景：纯白
        edit_bg: COLORREF(0x00FFFFFF),
        // 编辑框文本：深灰
        edit_fg: COLORREF(0x001F1F1F),
        // 工具提示背景：纯白
        tooltip_bg: COLORREF(0x00FFFFFF),
        // 工具提示文本：深灰
        tooltip_fg: COLORREF(0x001F1F1F),
        // 列表视图背景：纯白
        listview_bg: COLORREF(0x00FFFFFF),
        // 列表视图文本：深灰
        listview_fg: COLORREF(0x001F1F1F),
        // 强调色：Material 橙 600（RGB 251,140,0）
        accent: COLORREF(0x00008CFB),
        // 边框：浅灰
        border: COLORREF(0x00D0D0D0),
        // 悬停底色：比背景深一档
        hover: COLORREF(0x00E9E9E9),
        // 选中行：accent 混入白底 25%
        selected: COLORREF(0x00BFE2FE),
        // 次要文字：中灰
        muted: COLORREF(0x006E6E6E),
        // 列表奇偶行交替：与白底微差
        listview_alt_bg: COLORREF(0x00F7F7F7),
        // 表头背景：比窗口背景深一档
        header_bg: COLORREF(0x00F0F0F0),
        // 表头文字：深灰
        header_fg: COLORREF(0x004A4A4A),
    }
}

/// 暗色调色板
///
/// 参考 Windows 11 深色（实测反馈 9.7：纯黑底 + 纯白字过于刺眼，
/// 调整为灰黑档）：窗口背景 #202020、文本 #E6E6E6、编辑框 #2F2F2F 底
/// 近白字、工具提示同编辑框、列表视图同窗口背景。
pub fn dark_colors() -> ThemeColors {
    ThemeColors {
        // 背景：灰黑档（原 #1E1F20 过黑，见问题 9.7）
        bg: COLORREF(0x00202020),
        // 前景：柔和近白（原 #F3F3F3 过亮）
        fg: COLORREF(0x00E6E6E6),
        // 编辑框背景：#2F2F2F（原 #2B2B2B 提亮一档）
        edit_bg: COLORREF(0x002F2F2F),
        // 编辑框文本：近白
        edit_fg: COLORREF(0x00F0F0F0),
        // 工具提示背景：#2F2F2F
        tooltip_bg: COLORREF(0x002F2F2F),
        // 工具提示文本：近白
        tooltip_fg: COLORREF(0x00F0F0F0),
        // 列表视图背景：与窗口背景一致
        listview_bg: COLORREF(0x00202020),
        // 列表视图文本：柔和近白
        listview_fg: COLORREF(0x00E6E6E6),
        // 强调色：Material 橙 600（RGB 251,140,0）
        accent: COLORREF(0x00008CFB),
        // 边框：深灰
        border: COLORREF(0x003C3C3C),
        // 悬停底色：比背景浅一档
        hover: COLORREF(0x002A2A2A),
        // 选中行：accent 混入背景 25%（RGB 87,59,24）
        selected: COLORREF(0x00183B57),
        // 次要文字：中灰
        muted: COLORREF(0x009E9E9E),
        // 列表奇偶行交替：与背景微差
        listview_alt_bg: COLORREF(0x00282828),
        // 表头背景：比窗口背景浅一档
        header_bg: COLORREF(0x00262626),
        // 表头文字：浅灰
        header_fg: COLORREF(0x00C8C8C8),
    }
}

/// 两色线性混合（纯函数）
///
/// `t = 0` 返回 `a`，`t = 1` 返回 `b`，中间值按 RGB 通道线性插值；
/// `t` 会被夹取到 `[0, 1]`。供自绘按钮悬停/按压变色与选中色派生使用。
pub fn blend(a: COLORREF, b: COLORREF, t: f32) -> COLORREF {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u32, y: u32| (x as f32 + (y as f32 - x as f32) * t).round() as u32;
    let ar = a.0 & 0xFF;
    let ag = (a.0 >> 8) & 0xFF;
    let ab = (a.0 >> 16) & 0xFF;
    let br = b.0 & 0xFF;
    let bg = (b.0 >> 8) & 0xFF;
    let bb = (b.0 >> 16) & 0xFF;
    COLORREF(lerp(ar, br) | (lerp(ag, bg) << 8) | (lerp(ab, bb) << 16))
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

// ============================================================
// 全局字体（决策记录 D11）
// ============================================================
//
// 全项目此前从不设置字体，所有控件使用遗留 System 位图字体（中文显示
// 尤其粗糙）。此处经 `SPI_GETNONCLIENTMETRICS` 取系统消息字体
// `lfMessageFont`（中文系统即微软雅黑，英文即 Segoe UI），创建一次、
// 进程生命周期持有（与画刷缓存同一策略，永不 DeleteObject）。

/// 进程级常规消息字体缓存（存 HFONT 原始值，理由同 BRUSH_CACHE）
static MESSAGE_FONT: OnceLock<usize> = OnceLock::new();
/// 进程级粗体消息字体缓存（tooltip 标题等强调文字用）
static MESSAGE_FONT_BOLD: OnceLock<usize> = OnceLock::new();

/// 创建系统消息字体（`lfMessageFont`，可覆盖字重）
///
/// `SystemParametersInfoW(SPI_GETNONCLIENTMETRICS)` 失败或字体创建失败时
/// 回退 `Segoe UI`（系统能自动回退到中文默认字体），再失败返回 0
/// （调用方据此跳过 `WM_SETFONT`，保持系统默认字体）。
fn create_message_font(weight: Option<i32>) -> usize {
    // SAFETY: SystemParametersInfoW 为线程安全标准 API；NONCLIENTMETRICSW 为
    // 栈上结构，cbSize 已按文档要求填写，调用期间存活。
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some(&mut ncm as *mut NONCLIENTMETRICSW as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
    };
    if ok {
        let mut lf = ncm.lfMessageFont;
        if let Some(w) = weight {
            lf.lfWeight = w;
        }
        // SAFETY: lf 为栈上 LOGFONTW，调用期间存活；返回的 HFONT 进入进程级
        // 缓存、永不删除（见 MESSAGE_FONT 文档）。
        let font = unsafe { CreateFontIndirectW(&lf) };
        if !font.is_invalid() {
            return font.0 as usize;
        }
    }
    // 回退：Segoe UI（系统对缺字字形自动回退到中文默认字体）
    // SAFETY: 参数均为值类型/静态宽字符串；失败返回无效句柄由调用方判 0。
    let font = unsafe {
        windows::Win32::Graphics::Gdi::CreateFontW(
            -12,
            0,
            0,
            0,
            weight.unwrap_or(400),
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::DEFAULT_CHARSET.0 as u32,
            windows::Win32::Graphics::Gdi::OUT_DEFAULT_PRECIS.0 as u32,
            windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS.0 as u32,
            windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY.0 as u32,
            windows::Win32::Graphics::Gdi::DEFAULT_PITCH.0 as u32,
            PCWSTR(windows::core::w!("Segoe UI").as_ptr()),
        )
    };
    if font.is_invalid() {
        0
    } else {
        font.0 as usize
    }
}

/// 获取全局常规消息字体（首次调用惰性创建，之后命中缓存）
///
/// 字体进程生命周期持有、永不删除；创建彻底失败（返回无效句柄）时
/// `HFONT(0)` 为空句柄，调用方须判空后再 `WM_SETFONT` / `SelectObject`。
pub fn message_font() -> HFONT {
    HFONT(*MESSAGE_FONT.get_or_init(|| create_message_font(None)) as *mut c_void)
}

/// 获取全局粗体消息字体（tooltip 标题等强调文字）
///
/// 与 [`message_font`] 同源（`lfMessageFont` 覆盖粗体字重），同样进程持有。
pub fn message_font_bold() -> HFONT {
    HFONT(*MESSAGE_FONT_BOLD.get_or_init(|| create_message_font(Some(700))) as *mut c_void)
}

/// EnumChildWindows 回调：向每个子控件发送 WM_SETFONT 注入全局字体
///
/// lParam 携带 HFONT 原始值；返回 TRUE 继续枚举。
unsafe extern "system" fn enum_font_child_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: 回调仅在 EnumChildWindows 枚举期间、同线程执行；hwnd 为被
    // 枚举的子控件；WM_SETFONT 的 wParam 为字体句柄、lParam=1 表示重绘。
    unsafe {
        let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(lparam.0 as usize), LPARAM(1));
    }
    BOOL(1)
}

/// 向窗口的所有子控件应用全局消息字体（须在子控件全部创建后调用）
///
/// 遍历 `EnumChildWindows` 并 `WM_SETFONT`；字体创建失败（空句柄）时为空操作，
/// 控件保持系统默认字体，不影响功能。
pub fn apply_font_to_children(hwnd: HWND) {
    let font = message_font();
    if font.0 as usize == 0 {
        return;
    }
    // SAFETY: hwnd 为存活窗口；回调函数签名符合 EnumChildWindows 约定；
    // lParam 携带字体句柄原始值（指针↔usize 往返为良定义转换）。
    unsafe {
        let _ = EnumChildWindows(hwnd, Some(enum_font_child_proc), LPARAM(font.0 as isize));
    }
}

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
        // 语义色（自绘按钮 / 列表行 / 边框）同样随明暗主题区分（accent 两套一致，不在此列）
        assert_ne!(light.border, dark.border);
        assert_ne!(light.hover, dark.hover);
        assert_ne!(light.selected, dark.selected);
        assert_ne!(light.muted, dark.muted);
        assert_ne!(light.listview_alt_bg, dark.listview_alt_bg);
    }

    /// 颜色混合：端点取值与中间线性插值
    #[test]
    fn blend_endpoints_and_midpoint() {
        let black = COLORREF(0x00000000);
        let white = COLORREF(0x00FFFFFF);
        assert_eq!(blend(black, white, 0.0), black);
        assert_eq!(blend(black, white, 1.0), white);
        // 中点：RGB 三通道均为 128 左右
        let mid = blend(black, white, 0.5);
        assert_eq!(mid.0 & 0xFF, 128);
        // t 越界被夹取
        assert_eq!(blend(black, white, 2.0), white);
        assert_eq!(blend(black, white, -1.0), black);
        // 通道独立插值：红→蓝（BGR: 0x000000FF → 0x00FF0000）
        let red = COLORREF(0x000000FF);
        let blue = COLORREF(0x00FF0000);
        let mid_rb = blend(red, blue, 0.5);
        assert_eq!(mid_rb.0 & 0xFF, 128); // R 通道
        assert_eq!((mid_rb.0 >> 16) & 0xFF, 128); // B 通道
    }

    /// 暗色背景不刺眼（问题 9.7）：各背景档位处于灰黑区间而非纯黑/纯白，
    /// 前景对比度足够（亮度差 > 180）
    #[test]
    fn dark_palette_soft_contrast() {
        let dark = dark_colors();
        let luma = |c: COLORREF| -> u32 {
            let r = c.0 & 0xFF;
            let g = (c.0 >> 8) & 0xFF;
            let b = (c.0 >> 16) & 0xFF;
            (r * 299 + g * 587 + b * 114) / 1000
        };
        // 背景亮度处于灰黑档（既非纯黑也过亮）
        assert!(luma(dark.bg) >= 28 && luma(dark.bg) <= 48);
        // 前景与背景对比足够
        assert!(luma(dark.fg) - luma(dark.bg) > 180);
        // 选中行与悬停行均可与背景区分
        assert_ne!(dark.hover, dark.bg);
        assert_ne!(dark.selected, dark.bg);
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
