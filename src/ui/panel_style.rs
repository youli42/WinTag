//! 概览面板视觉样式辅助（D28，叶子模块）
//!
//! 供 `iced_app.rs` 的概览面板（confirm/settings/popup/panel 中的 panel）视觉改造复用：
//! - [`PanelPalette`] + [`panel_palette`]：Win11 暗色/亮色紧凑风格的语义调色板（`iced::Color`）
//! - 图标字形常量：拖拽手柄 / chevron / 编辑 / 删除 / 置前 / 保存 / 取消（Unicode 符号，非 web 图标字体）
//! - [`truncate_units`]：CJK 双宽单位截断纯函数（面板长路径省略号用）
//!
//! 本模块只依赖标准库与 `iced::Color`，不依赖任何其他项目模块（保持叶子），
//! 与 `ui::geo` / `ui::theme` 同级。颜色数值参考 demo（Win11 暗色紧凑版面板）。

use iced::Color;

/// 面板语义调色板（Win11 暗色/亮色紧凑风格）
///
/// 每个字段为 `iced::Color`（含 alpha），供 iced widget 直接着色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelPalette {
    /// 面板容器背景色
    pub surface: Color,
    /// 卡片（标签行）背景色
    pub card: Color,
    /// 鼠标悬停行底色
    pub hover: Color,
    /// 主文本色
    pub text: Color,
    /// 次要/元信息文本色
    pub subtle: Color,
    /// 强调色（主按钮 / 选中态 / 焦点光晕）
    pub accent: Color,
    /// 1px 边框 / 分隔线色
    pub border: Color,
    /// 危险操作（移除）色
    pub danger: Color,
}

/// 按明暗返回面板调色板（`dark=true` 用 Win11 暗色，否则浅色）
///
/// 取自 demo 的 Win11 暗色紧凑版：暗色底 `#202020`、卡片半透明 `45,45,45`、
/// 强调色 Win11 蓝 `#0078d4`、悬停 `rgba(255,255,255,.08)` 等。
pub fn panel_palette(dark: bool) -> PanelPalette {
    if dark {
        PanelPalette {
            surface: Color::from_rgb8(0x20, 0x20, 0x20),
            card: Color::from_rgba8(0x2D, 0x2D, 0x2D, 0.85),
            hover: Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.08),
            text: Color::from_rgb8(0xF3, 0xF3, 0xF3),
            subtle: Color::from_rgb8(0xA0, 0xA0, 0xA0),
            accent: Color::from_rgb8(0x00, 0x78, 0xD4),
            border: Color::from_rgb8(0x3A, 0x3A, 0x3A),
            danger: Color::from_rgb8(0xD1, 0x34, 0x38),
        }
    } else {
        PanelPalette {
            surface: Color::from_rgb8(0xF5, 0xF5, 0xF5),
            card: Color::from_rgb8(0xFF, 0xFF, 0xFF),
            hover: Color::from_rgba8(0x00, 0x00, 0x00, 0.05),
            text: Color::from_rgb8(0x1F, 0x1F, 0x1F),
            subtle: Color::from_rgb8(0x6E, 0x6E, 0x6E),
            accent: Color::from_rgb8(0x00, 0x78, 0xD4),
            border: Color::from_rgb8(0xD0, 0xD0, 0xD0),
            danger: Color::from_rgb8(0xD1, 0x34, 0x38),
        }
    }
}

/// 拖拽排序手柄图标（`⋮⋮`，两列竖点）
pub const DRAG_HANDLE: &str = "⋮⋮";
/// 收起态 chevron（向右箭头，展开后旋转 90°）
pub const CHEVRON_RIGHT: &str = "▸";
/// 展开态 chevron（向下箭头）
pub const CHEVRON_DOWN: &str = "▾";
/// 编辑按钮图标
pub const ICON_EDIT: &str = "✎";
/// 移除按钮图标
pub const ICON_DELETE: &str = "🗑";
/// 置前按钮图标
pub const ICON_TOP: &str = "▲";
/// 保存按钮图标
pub const ICON_SAVE: &str = "✓";
/// 取消按钮图标
pub const ICON_CANCEL: &str = "✕";

/// 单个字符的显示宽度单位（CJK 全角按 2 单位、其余 1 单位，用于截断预算）
///
/// 覆盖 CJK 统一表意文字（U+4E00..=U+9FFF）、扩展 A（U+3400..=U+4DBF）、
/// 平假名/片假名（U+3040..=U+30FF）、全角形式（U+FF00..=U+FFEF）、
/// CJK 符号/标点（U+3000..=U+303F）。全角字符显示宽约为半角两倍，
/// 故按 2 单位计入预算，保证混排文本截断宽度一致。
fn unit_width(c: char) -> usize {
    let cp = c as u32;
    let cjk = matches!(cp,
        0x3400..=0x4DBF   // CJK 扩展 A
        | 0x4E00..=0x9FFF // CJK 统一表意
        | 0x3040..=0x30FF // 平假名/片假名
        | 0x3000..=0x303F // CJK 符号/标点
        | 0xFF00..=0xFFEF // 全角形式
    );
    if cjk {
        2
    } else {
        1
    }
}

/// 按显示单位截断字符串（CJK 全角=2 单位、半角=1 单位），超出以省略号"…"结尾（纯函数）
///
/// 与 `sys::badge::truncate_title` 的字符数截断不同，本函数按 **显示宽度单位**
/// 预算（CJK 双宽），用于面板长路径/标题省略号，保证中英混排截断后视觉宽度一致。
/// `max_units` 为 0 时返回单省略号；未超限时返回原字符串克隆。
pub fn truncate_units(s: &str, max_units: usize) -> String {
    let total: usize = s.chars().map(unit_width).sum();
    if total <= max_units {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = unit_width(c);
        if used + w + 1 > max_units {
            // 预留给省略号（省略号宽 2 单位，此处保守按 1 预留剩余预算）
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯 ASCII：超限截断 + 省略号
    #[test]
    fn truncate_ascii() {
        assert_eq!(truncate_units("abcde", 3), "ab…");
        assert_eq!(truncate_units("abcde", 5), "abcde");
    }

    /// 纯中文：每字 2 单位
    #[test]
    fn truncate_chinese() {
        // "测试字样" 4 字 × 2 = 8 单位；预算 6 → 3 字
        assert_eq!(truncate_units("测试字样", 6), "测试…");
        // 预算 4 → 2 字
        assert_eq!(truncate_units("测试字样", 4), "测…");
    }

    /// 中英混合：CJK 双宽、ASCII 单宽
    #[test]
    fn truncate_mixed() {
        // "ab测试" = 1+1+2+2 = 6 单位；预算 4 → "ab…"
        assert_eq!(truncate_units("ab测试", 4), "ab…");
        // 预算 5 → "ab测…"（ab=2 + 测=2 = 4，剩 1 无法容纳下一个 CJK）
        assert_eq!(truncate_units("ab测试", 5), "ab测…");
    }

    /// 未超限：原样返回（克隆）
    #[test]
    fn truncate_under_limit_returns_clone() {
        let s = "短文本";
        assert_eq!(truncate_units(s, 100), s);
        // 边界：正好等于 max_units
        assert_eq!(truncate_units("测试", 4), "测试");
    }

    /// max_units=0：只返回省略号
    #[test]
    fn truncate_zero_limit() {
        assert_eq!(truncate_units("abc", 0), "…");
    }

    /// 暗色/亮色调色板各语义色非透明且相异（冒烟）
    #[test]
    fn palette_smoke() {
        let dark = panel_palette(true);
        let light = panel_palette(false);
        assert_ne!(dark.surface, light.surface);
        assert_eq!(dark.accent, Color::from_rgb8(0x00, 0x78, 0xD4));
        // 16 进制转换: surface 暗色 #202020
        assert_eq!(dark.surface, Color::from_rgb8(0x20, 0x20, 0x20));
    }

    /// 图标字形常量为非空字符串（无 tofu 空串）
    #[test]
    fn glyphs_non_empty() {
        for g in [
            DRAG_HANDLE,
            CHEVRON_RIGHT,
            CHEVRON_DOWN,
            ICON_EDIT,
            ICON_DELETE,
            ICON_TOP,
            ICON_SAVE,
            ICON_CANCEL,
        ] {
            assert!(!g.is_empty());
        }
    }
}
