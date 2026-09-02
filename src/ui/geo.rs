//! 主线程窗口定位辅助（D27：自 `ui::layout` 与 `ui::popup` 移入，供 iced 弹窗定位）
//!
//! iced 自管 DPI 与窗口定位，但主线程仍需在 **物理像素** 空间计算 iced 弹窗的
//! 左上角坐标（光标右下偏移 + 钳制到显示器工作区）。本模块保留这两项纯/半纯
//! 的 Win32 辅助：DPI 缩放（[`dp`]）与工作区钳制（[`clamp_to_work`]）。

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;

/// 96 DPI 基准值（`USER_DEFAULT_SCREEN_DPI`）
pub const BASE_DPI: u32 = 96;

/// 标签弹窗逻辑尺寸（供主线程计算 iced 弹窗定位钳制用）
pub const POPUP_LOGICAL_W: i32 = 420;
pub const POPUP_LOGICAL_H: i32 = 320;

/// 纯函数：按 DPI 缩放设计像素（96 为基准，四舍五入）
///
/// `dpi` 为 0 时回退 96（不缩放），保证异常输入不 panic。
/// 最小返回 1（避免缩放后尺寸归零导致控件不可见）。
pub fn scale_px(px: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    let scaled = (px as i64 * dpi as i64 + BASE_DPI as i64 / 2) / BASE_DPI as i64;
    scaled.clamp(1, i32::MAX as i64) as i32
}

/// 纯函数：把物理像素还原为逻辑像素（`scale_px` 的逆运算，四舍五入）
///
/// 供主线程把 Win32 物理像素坐标（`GetCursorPos` / `GetWindowRect`）转成
/// iced 期望的**逻辑**坐标投递。iced 的 [`iced::window::Position::Specific`]
/// 被解释为 winit 逻辑坐标（见 iced_winit `conversion::position`），若直接传
/// 物理像素，HiDPI（150%/200%）下弹窗会偏移到 1.5×/2× 处。
pub fn unscale_px(px: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    let scaled = (px as i64 * BASE_DPI as i64 + dpi as i64 / 2) / dpi as i64;
    scaled.clamp(0, i32::MAX as i64) as i32
}

/// 取窗口当前 DPI 并缩放设计像素
///
/// `GetDpiForWindow` 失败（句柄无效等）时按 96 DPI（不缩放）处理；
/// 须在窗口创建之后调用（否则窗口尚无 DPI 关联）。
pub fn dp(hwnd: HWND, px: i32) -> i32 {
    // SAFETY: GetDpiForWindow 为只读查询，句柄无效时返回 0，无副作用。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    scale_px(px, if dpi == 0 { BASE_DPI } else { dpi })
}

/// 将弹窗位置钳制到光标附近 + 所在显示器工作区内（R19）
///
/// 首选光标右下偏移 (16, 16)；越出工作区右/下边界时向左/上收回，
/// 负坐标时贴工作区左/上边缘。主线程在 iced 弹窗创建前调用。
pub fn clamp_to_work(x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    // SAFETY: MonitorFromPoint 为只读查询，取不到时返回 NULL 由下方回退处理。
    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    if hmon.is_invalid() {
        return (x, y);
    }
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi 为栈上缓冲且 cbSize 已填，GetMonitorInfoW 只读填充。
    if !unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
        return (x, y);
    }
    let work = mi.rcWork;
    let mut nx = x;
    let mut ny = y;
    if nx + w > work.right {
        nx = work.right - w;
    }
    if ny + h > work.bottom {
        ny = work.bottom - h;
    }
    if nx < work.left {
        nx = work.left;
    }
    if ny < work.top {
        ny = work.top;
    }
    (nx, ny)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 96 DPI 不缩放，高 DPI 线性放大
    #[test]
    fn scale_px_levels() {
        assert_eq!(scale_px(12, 96), 12);
        assert_eq!(scale_px(12, 144), 18); // 150%
        assert_eq!(scale_px(12, 192), 24); // 200%
        assert_eq!(scale_px(100, 120), 125); // 125%
    }

    /// 四舍五入：非整数结果取最近整数
    #[test]
    fn scale_px_rounding() {
        // 12 * 110 / 96 = 13.75 → 14
        assert_eq!(scale_px(12, 110), 14);
        // 10 * 110 / 96 = 11.46 → 11
        assert_eq!(scale_px(10, 110), 11);
    }

    /// 异常输入：dpi=0 回退 96；极小值保底 1
    #[test]
    fn scale_px_fallbacks() {
        assert_eq!(scale_px(12, 0), 12);
        assert_eq!(scale_px(1, 96), 1);
        // 0px 请求也至少返回 1，避免控件归零不可见
        assert_eq!(scale_px(0, 96), 1);
    }

    /// unscale_px：96 DPI 不缩放；高 DPI 线性还原（scale_px 的逆运算）
    #[test]
    fn unscale_px_reverts_scale() {
        // 96 DPI 恒等：物理 == 逻辑
        assert_eq!(unscale_px(24, 96), 24);
        // 150%：物理 36 ← 逻辑 24
        assert_eq!(unscale_px(36, 144), 24);
        // 200%：物理 48 ← 逻辑 24
        assert_eq!(unscale_px(48, 192), 24);
    }

    /// unscale_px 与 scale_px 往返一致（含四舍五入边界）
    #[test]
    fn unscale_scale_roundtrip() {
        for dpi in [96, 110, 120, 144, 192] {
            for logical in [1, 8, 16, 24, 100, 420] {
                assert_eq!(unscale_px(scale_px(logical, dpi), dpi), logical);
            }
        }
    }

    /// unscale_px 异常输入：dpi=0 回退 96（不缩放）
    #[test]
    fn unscale_px_fallback() {
        assert_eq!(unscale_px(24, 0), 24);
    }
}
