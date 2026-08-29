//! 布局辅助：DPI 缩放（决策记录 D11）
//!
//! 进程声明了 Per-Monitor V2 DPI 感知，窗口尺寸/子控件坐标以物理像素
//! 解释；此前全部硬编码 96-DPI 像素，高 DPI 屏（150%）上控件会显得
//! 小而拥挤。本模块提供以窗口当前 DPI 为基准的缩放函数，所有布局
//! 常量以"设计像素"（96 DPI 基准）书写、经 [`dp`] 换算为物理像素。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;

/// 96 DPI 基准值（`USER_DEFAULT_SCREEN_DPI`）
pub const BASE_DPI: u32 = 96;

/// 纯函数：按 DPI 缩放设计像素（96 为基准，四舍五入）
///
/// `dpi` 为 0 时回退 96（不缩放），保证异常输入不 panic。
/// 最小返回 1（避免缩放后尺寸归零导致控件不可见）。
pub fn scale_px(px: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    let scaled = (px as i64 * dpi as i64 + BASE_DPI as i64 / 2) / BASE_DPI as i64;
    scaled.clamp(1, i32::MAX as i64) as i32
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
}
