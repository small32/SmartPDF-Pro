//! 生成应用图标（1024×1024 PNG），适配 macOS Tahoe 图标规范。
//! 画布 1024，图标主体 824×824 居中（四周 100px 透明边距），squircle 圆角半径 185。
//! 运行中 App 的 Dock 图标用原始位图渲染（不走 IconServices 遮罩），
//! 因此圆角形状必须直接烘焙进 PNG 本身，而非依赖系统裁剪。
//! 图案：蓝色渐变 squircle 底 + 一页"文档"（白色圆角纸面 + 灰色文字行）。
//! 用法：`cargo run --example gen_icon -- <输出.png>`

use image::{Rgba, RgbaImage};

/// 画布边长（Tahoe 模板：1024）
const SIZE: u32 = 1024;
/// 图标主体边长（Tahoe 模板：824，四周各留 100px 透明边距）
const BODY: f32 = 824.0;
/// squircle 圆角半径（Tahoe 模板：185）
const RADIUS: f32 = 185.0;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/icon.png".into());

    let mut img = RgbaImage::new(SIZE, SIZE);
    let s = SIZE as f32;
    let margin = (s - BODY) / 2.0;
    // 主体矩形（squircle 底板）
    let (bx, by, bw, bh) = (margin, margin, BODY, BODY);
    let center_x = bx + bw / 2.0;
    let center_y = by + bh / 2.0;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = (x as f32, y as f32);
            // 透明边距：主体之外全透明
            if !rounded_rect(px, py, bx, by, bw, bh, RADIUS) {
                img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                continue;
            }
            // 径向渐变蓝（相对主体中心）
            let dx = px - center_x;
            let dy = py - center_y;
            let d = (dx * dx + dy * dy).sqrt() / (BODY / 2.0);
            let t = d.clamp(0.0, 1.0);
            let r = (38.0 + 70.0 * t) as u8;
            let g = (122.0 + 40.0 * t) as u8;
            let b = (204.0 + 30.0 * t) as u8;
            img.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }

    // 白色纸面（圆角矩形），居中于主体：x=30%..70%, y=22%..78%（宽 40%，高 56%）
    let page = (bx + bw * 0.30, by + bh * 0.22, bw * 0.40, bh * 0.56);
    // 文字行（灰色条），全部居于纸面内：x=36% 起，y 依次 36/44/51/58/65%（相对主体）
    let bars: [(f32, f32, f32, f32); 5] = [
        (bx + bw * 0.36, by + bh * 0.36, bw * 0.28, bh * 0.045),
        (bx + bw * 0.36, by + bh * 0.44, bw * 0.24, bh * 0.030),
        (bx + bw * 0.36, by + bh * 0.51, bw * 0.28, bh * 0.030),
        (bx + bw * 0.36, by + bh * 0.58, bw * 0.22, bh * 0.030),
        (bx + bw * 0.36, by + bh * 0.65, bw * 0.26, bh * 0.030),
    ];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = (x as f32, y as f32);
            if rounded_rect(px, py, page.0, page.1, page.2, page.3, 28.0) {
                // 纸面内部
                let mut c = Rgba([255, 255, 255, 255]);
                for (bx2, by2, bw2, bh2) in bars {
                    if px >= bx2 && px <= bx2 + bw2 && py >= by2 && py <= by2 + bh2 {
                        c = Rgba([175, 182, 195, 255]);
                        break;
                    }
                }
                img.put_pixel(x, y, c);
            }
        }
    }

    img.save(&out).expect("保存图标失败");
    print!("{out}");
}

/// 判断点 (px, py) 是否在圆角矩形 (x,y,w,h,radius) 内。
fn rounded_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    if px < x || px > x + w || py < y || py > y + h {
        return false;
    }
    let cx = px.clamp(x + r, x + w - r);
    let cy = py.clamp(y + r, y + h - r);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}
