//! 角标软件光栅渲染（决策记录 D11，解决问题 7 / 完成需求 R2）
//!
//! 原实现用 GDI `FillRect` 画 12×12 实心方块 + 色键透明（`LWA_COLORKEY`），
//! 边缘零抗锯齿。本模块以纯函数软件光栅化"贴角圆边三角形"——
//!
//! - 直角顶点贴窗口左上角，斜边朝右下；
//! - 斜边与直角顶点处带圆角（`r ≈ size/6`），基于三角形带符号距离（SDF）
//!   计算每像素 coverage，天然抗锯齿；
//! - 1px 深色描边（混 `stroke` 色）保证浅色窗口上可辨识。
//!
//! 输出为预乘 BGRA 字节缓冲（字节序 B,G,R,A——`UpdateLayeredWindow` 32bpp 位图
//! 的内存布局；D16 修复：原先误写 RGBA 导致 R≠B 的颜色互换显示），
//! `render_badge` 为无窗口依赖的纯函数，可单测。

/// 角标渲染参数
#[derive(Debug, Clone, Copy)]
pub struct BadgeParams {
    /// 逻辑像素边长（角标贴左上角的等腰直角三角形腰长）
    pub size: i32,
    /// 填充色 RGBA
    pub fill: [u8; 4],
    /// 描边色 RGBA（1px 边界混色用）
    pub stroke: [u8; 4],
}

/// 点到线段的有符号距离（带约定正负侧）
///
/// 返回值为正表示在线段"内侧"（约定为三角形内部方向），为负表示外侧。
/// `px,py` 为查询点，`ax,ay`/`bx,by` 为线段两端。
/// 返回 (距离绝对值, 是否在内侧)。
fn signed_dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> (f32, bool) {
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-9 {
        let d = ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
        return (d, false);
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    let closest_x = ax + t * dx;
    let closest_y = ay + t * dy;
    let dist = ((px - closest_x).powi(2) + (py - closest_y).powi(2)).sqrt();
    // 叉积符号判定点在线段哪一侧
    let cross = dx * (py - ay) - dy * (px - ax);
    (dist, cross >= 0.0)
}

/// 渲染贴角圆边三角形为预乘 BGRA 字节缓冲
///
/// 三角形顶点（以左上角为原点，x 向右、y 向下）：
/// - A = (0, 0)        左上直角顶点
/// - B = (size, 0)      右上
/// - C = (0, size)      左下
///
/// 斜边 BC；内部判定：点在 BC 的左下侧（叉积 ≥ 0）且在两条直角边内侧。
/// 圆角：在顶点附近用距离场软化；描边：距斜边 < 1.5px 时混 stroke 色。
///
/// 返回 `size * size * 4` 字节的预乘 BGRA 缓冲（字节序 B,G,R,A；行优先、从上到下）。
pub fn render_badge(params: BadgeParams) -> Vec<u8> {
    let n = params.size.max(0) as usize;
    let mut buf = vec![0u8; n * n * 4];
    if n == 0 {
        return buf;
    }

    let s = params.size as f32;
    let a = [0.0_f32, 0.0]; // 左上直角顶点
    let b = [s, 0.0]; // 右上
    let c = [0.0, s]; // 左下
    let corner_r = (s / 6.0).max(2.0); // 顶点圆角半径

    for y in 0..n {
        for x in 0..n {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // —— 三条边的距离 ——
            // 斜边 BC
            let (d_bc, inside_bc) = signed_dist_to_segment(px, py, b[0], b[1], c[0], c[1]);
            // 直角边 AB（上边）：内侧为 y > 0
            let (d_ab, _) = signed_dist_to_segment(px, py, a[0], a[1], b[0], b[1]);
            let inside_ab = py >= 0.0;
            // 直角边 AC（左边）：内侧为 x > 0
            let (d_ac, _) = signed_dist_to_segment(px, py, a[0], a[1], c[0], c[1]);
            let inside_ac = px >= 0.0;

            // 是否在三角形内部（含圆角软化）
            let in_triangle = inside_bc && inside_ab && inside_ac;

            // —— 覆盖率（抗锯齿）——
            // 内部点覆盖率 = 1；边界附近按到最近边的距离软化
            let edge_dist = d_bc.min(d_ab).min(d_ac);
            // 顶点圆角：离任意顶点近时按圆角距离软化
            let d_a = ((px - a[0]).powi(2) + (py - a[1]).powi(2)).sqrt();
            let d_b = ((px - b[0]).powi(2) + (py - b[1]).powi(2)).sqrt();
            let d_c = ((px - c[0]).powi(2) + (py - c[1]).powi(2)).sqrt();
            let corner_dist = d_a.min(d_b).min(d_c);

            // 综合覆盖率：内部 1.0，边界按距离线性过渡到 0（1px 过渡带）
            let mut coverage = if in_triangle {
                1.0
            } else {
                // 圆角顶点：corner_r 范围内软化
                if corner_dist < corner_r {
                    (1.0 - (corner_dist - corner_r + 1.0).max(0.0)).clamp(0.0, 1.0)
                } else {
                    // 边界外：edge_dist 在 [0, 1] 过渡
                    (1.0 - edge_dist).clamp(0.0, 1.0)
                }
            };
            // 顶点圆角裁剪：在直角顶点 A 附近，超出 corner_r 圆的像素降低覆盖率，
            // 使直角顶点变圆
            if d_a < corner_r {
                // 在 A 顶点圆角内：按圆距离重新计算覆盖率
                let corner_coverage = (corner_r - d_a).clamp(0.0, 1.0);
                coverage = coverage.min(corner_coverage);
            }

            if coverage <= 0.0 {
                continue;
            }

            // —— 描边混色：距斜边 < 1.5px 时混 stroke 色 ——
            let stroke_mix = if d_bc < 1.5 && inside_bc {
                (1.5 - d_bc) / 1.5
            } else {
                0.0
            };

            let idx = (y * n + x) * 4;
            let base_r = params.fill[0] as f32;
            let base_g = params.fill[1] as f32;
            let base_b = params.fill[2] as f32;
            let s_r = params.stroke[0] as f32;
            let s_g = params.stroke[1] as f32;
            let s_b = params.stroke[2] as f32;
            let r = base_r * (1.0 - stroke_mix) + s_r * stroke_mix;
            let g = base_g * (1.0 - stroke_mix) + s_g * stroke_mix;
            let b = base_b * (1.0 - stroke_mix) + s_b * stroke_mix;
            let alpha = (coverage * 255.0).round() as u8;
            // 预乘：颜色通道乘以 coverage；字节序为 B,G,R,A——
            // UpdateLayeredWindow 的 32bpp 位图内存布局即 BGRA（D16 修复：
            // 原先按 RGBA 写入导致橙/蓝等 R≠B 的颜色互换显示）
            let premul = coverage;
            buf[idx] = (b * premul) as u8;
            buf[idx + 1] = (g * premul) as u8;
            buf[idx + 2] = (r * premul) as u8;
            buf[idx + 3] = alpha;
        }
    }
    buf
}

/// 截断标题为最多 `max_chars` 个字符，超出部分以省略号结尾（纯函数）
///
/// 按 Unicode 字符（`char`）计数，中文与英文等价对待；不足 `max_chars`
/// 时原样返回克隆。供覆盖层标题条显示截断使用（需求：默认显示 5 个字，
/// 多的用省略号表示）。
pub fn truncate_title(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 渲染圆角矩形为预乘 BGRA 字节缓冲（纯函数，SDF 抗锯齿）
///
/// `width`/`height` 为矩形逻辑像素尺寸，`radius` 为四角圆角半径
/// （自动夹取到不超过宽高一半）；`fill` 为填充色，`stroke` 为 1px
/// 边界描边混色。与 [`render_badge`] 同款：逐像素计算到圆角矩形边界
/// 的符号距离，边界 1px 过渡带实现抗锯齿。
///
/// 返回 `width * height * 4` 字节的预乘 BGRA 缓冲（字节序 B,G,R,A；行优先、从上到下）。
pub fn render_rounded_rect(
    width: i32,
    height: i32,
    radius: i32,
    fill: [u8; 4],
    stroke: [u8; 4],
) -> Vec<u8> {
    let w = width.max(0) as usize;
    let h = height.max(0) as usize;
    let mut buf = vec![0u8; w * h * 4];
    if w == 0 || h == 0 {
        return buf;
    }

    let fw = width as f32;
    let fh = height as f32;
    // 圆角半径夹取：不超过宽高一半
    let r = (radius as f32).clamp(0.0, fw.min(fh) / 2.0);
    let half_w = fw / 2.0;
    let half_h = fh / 2.0;
    let inner_w = half_w - r;
    let inner_h = half_h - r;

    for y in 0..h {
        for x in 0..w {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // 标准 round-box SDF：q 为超出内接矩形的分量，dist<0 在内部
            let qx = (px - half_w).abs() - inner_w;
            let qy = (py - half_h).abs() - inner_h;
            let out_x = qx.max(0.0);
            let out_y = qy.max(0.0);
            let dist = (out_x * out_x + out_y * out_y).sqrt() + (qx.max(qy)).min(0.0) - r;

            // 覆盖率：内部 1.0，边界 1px 过渡带线性衰减
            let coverage = (1.0 - dist).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }

            // 描边混色：距边界 1.5px 内侧向边界渐变混入 stroke 色
            let stroke_mix = if dist < 0.0 && dist > -1.5 {
                (dist + 1.5) / 1.5
            } else if dist >= 0.0 {
                1.0
            } else {
                0.0
            };

            let idx = (y * w + x) * 4;
            let r_ch = fill[0] as f32 * (1.0 - stroke_mix) + stroke[0] as f32 * stroke_mix;
            let g_ch = fill[1] as f32 * (1.0 - stroke_mix) + stroke[1] as f32 * stroke_mix;
            let b_ch = fill[2] as f32 * (1.0 - stroke_mix) + stroke[2] as f32 * stroke_mix;
            let alpha = (coverage * 255.0).round() as u8;
            let premul = coverage;
            // 字节序 B,G,R,A（UpdateLayeredWindow 32bpp 位图内存布局，同 render_badge）
            buf[idx] = (b_ch * premul) as u8;
            buf[idx + 1] = (g_ch * premul) as u8;
            buf[idx + 2] = (r_ch * premul) as u8;
            buf[idx + 3] = alpha;
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(buf: &[u8], size: usize, x: usize, y: usize) -> u8 {
        buf[(y * size + x) * 4 + 3]
    }

    /// 中心偏内部像素 alpha 接近 255（三角形内部实心）
    #[test]
    fn badge_interior_solid() {
        let buf = render_badge(BadgeParams {
            size: 20,
            fill: [255, 183, 77, 255],
            stroke: [0, 0, 0, 255],
        });
        // (3,3) 远离边界，应在三角形内部
        assert!(alpha_at(&buf, 20, 3, 3) > 200);
    }

    /// 右下角顶点外的像素 alpha 为 0（三角形外部）
    #[test]
    fn badge_exterior_empty() {
        let buf = render_badge(BadgeParams {
            size: 20,
            fill: [255, 183, 77, 255],
            stroke: [0, 0, 0, 255],
        });
        // (18,18) 在斜边右下外侧
        assert_eq!(alpha_at(&buf, 20, 18, 18), 0);
    }

    /// 斜边中点像素有部分覆盖率（抗锯齿过渡带）
    #[test]
    fn badge_hypotenuse_antialiased() {
        let buf = render_badge(BadgeParams {
            size: 20,
            fill: [255, 183, 77, 255],
            stroke: [0, 0, 0, 255],
        });
        // 斜边中点约在 (10,10) 附近，此处应有非零且非满的 alpha（过渡带）
        let a = alpha_at(&buf, 20, 10, 10);
        assert!(a > 0, "斜边中点不应完全透明");
        // 同时存在过渡：附近像素 alpha 应有梯度（不全为 0 也不全为 255）
        let a_in = alpha_at(&buf, 20, 8, 8);
        let a_out = alpha_at(&buf, 20, 12, 12);
        assert!(a_in >= a, "内侧 alpha 应 ≥ 斜边处");
        assert_eq!(a_out, 0, "斜边外侧应为 0");
    }

    /// size=0 返回空缓冲（不 panic）
    #[test]
    fn badge_zero_size_empty() {
        let buf = render_badge(BadgeParams {
            size: 0,
            fill: [255, 183, 77, 255],
            stroke: [0, 0, 0, 255],
        });
        assert!(buf.is_empty());
    }

    /// 缓冲长度 = size²×4
    #[test]
    fn badge_buffer_length() {
        let buf = render_badge(BadgeParams {
            size: 18,
            fill: [255, 183, 77, 255],
            stroke: [0, 0, 0, 255],
        });
        assert_eq!(buf.len(), 18 * 18 * 4);
    }

    /// 截断：不超过上限原样返回；超出取前 N 字符 + 省略号
    #[test]
    fn truncate_title_limits_and_ellipsizes() {
        assert_eq!(truncate_title("短", 5), "短");
        assert_eq!(truncate_title("五字标题哦", 5), "五字标题哦");
        assert_eq!(truncate_title("五字标题哦超出", 5), "五字标题哦…");
        assert_eq!(truncate_title("abcdefgh", 5), "abcde…");
        // 中文按字符计，不等同字节数
        assert_eq!(truncate_title("窗口标题测试超长", 5), "窗口标题测…");
        // 空串与 0 上限
        assert_eq!(truncate_title("", 5), "");
        assert_eq!(truncate_title("任意", 0), "…");
    }

    /// 圆角矩形：内部实心、外部透明、边界抗锯齿
    #[test]
    fn rounded_rect_interior_exterior_antialias() {
        let buf = render_rounded_rect(60, 16, 8, [47, 47, 47, 255], [60, 60, 60, 255]);
        let alpha_at = |x: usize, y: usize| buf[(y * 60 + x) * 4 + 3];
        // 内部（远离边界）实心
        assert!(alpha_at(30, 8) > 200);
        // 外部透明
        assert_eq!(alpha_at(0, 0), 0, "左上圆角外应为透明");
        // 边界附近存在抗锯齿过渡带（圆角外的对角区域）
        let corner = alpha_at(1, 1);
        assert!(corner < 255, "圆角过渡带应部分覆盖");
        // 缓冲长度
        assert_eq!(buf.len(), 60 * 16 * 4);
    }

    /// 圆角矩形：size=0 / radius 夹取不 panic
    #[test]
    fn rounded_rect_edge_cases() {
        assert!(render_rounded_rect(0, 10, 3, [0, 0, 0, 255], [0, 0, 0, 255]).is_empty());
        assert!(render_rounded_rect(10, 0, 3, [0, 0, 0, 255], [0, 0, 0, 255]).is_empty());
        // radius 超过宽高一半被夹取，不 panic 且产出合法缓冲
        let buf = render_rounded_rect(20, 20, 99, [255, 0, 0, 255], [0, 0, 0, 255]);
        assert_eq!(buf.len(), 20 * 20 * 4);
        // 中心像素应实心（胶囊形状）
        assert!(buf[(10 * 20 + 10) * 4 + 3] > 200);
    }

    /// 像素字节序必须是 BGRA（D16）：红色填充的中心像素
    /// 应为 [B=0, G=0, R=255, A=255]——按 RGBA 误读即出现橙/蓝互换
    #[test]
    fn badge_pixel_layout_is_bgra() {
        let buf = render_badge(BadgeParams {
            size: 18,
            fill: [255, 0, 0, 255],
            stroke: [255, 0, 0, 255],
        });
        // 取 (5,5)：远离三条边与顶点圆角（(9,9) 已在斜边过渡带上）
        let c = (5 * 18 + 5) * 4;
        assert_eq!(buf[c], 0, "byte0 应为蓝通道");
        assert_eq!(buf[c + 1], 0, "byte1 应为绿通道");
        assert_eq!(buf[c + 2], 255, "byte2 应为红通道");
        assert_eq!(buf[c + 3], 255);

        let rect = render_rounded_rect(20, 20, 6, [0, 0, 255, 255], [0, 0, 255, 255]);
        let c = (10 * 20 + 10) * 4;
        assert_eq!(rect[c], 255, "byte0 应为蓝通道");
        assert_eq!(rect[c + 2], 0, "byte2 应为红通道");
    }
}
