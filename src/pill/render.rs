// tiny-skia renderers for the pill. Produces a premultiplied BGRA byte
// buffer suitable for UpdateLayeredWindow with AC_SRC_ALPHA.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const RADIUS: f32 = 18.0;

pub fn clear_transparent(pm: &mut Pixmap) {
    pm.fill(Color::TRANSPARENT);
}

pub fn draw_recording(pm: &mut Pixmap, scale: f32, bar_heights: &[f32]) {
    clear_transparent(pm);
    draw_pill_bg(pm, scale);
    draw_bars(pm, scale, bar_heights);
}

fn draw_pill_bg(pm: &mut Pixmap, scale: f32) {
    let w = pm.width() as f32;
    let h = pm.height() as f32;

    // Inset by half the border width so the stroke sits fully inside the
    // pixmap (a centred stroke at the edge would be clipped in half).
    let border_w = (1.0 * scale).max(1.0);
    let inset = border_w * 0.5;
    let rw = w - 2.0 * inset;
    let rh = h - 2.0 * inset;
    let r = (RADIUS * scale).min(rh / 2.0);

    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, inset, inset, rw, rh, r);
    let path = pb.finish().unwrap();

    let mut fill = Paint::default();
    fill.set_color_rgba8(13, 13, 13, 245);
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    // Thin white hairline border.
    let mut border = Paint::default();
    border.set_color_rgba8(255, 255, 255, 72);
    border.anti_alias = true;
    let stroke = Stroke {
        width: border_w,
        ..Default::default()
    };
    pm.stroke_path(&path, &border, &stroke, Transform::identity(), None);
}

fn draw_bars(pm: &mut Pixmap, scale: f32, bar_heights: &[f32]) {
    if bar_heights.is_empty() {
        return;
    }
    let w = pm.width() as f32;
    let h = pm.height() as f32;

    let bar_count = bar_heights.len() as f32;
    let bar_w = 2.5 * scale;
    let bar_gap = 4.0 * scale;
    let total_w = bar_count * bar_w + (bar_count - 1.0) * bar_gap;
    let start_x = (w - total_w) / 2.0;
    let cy = h / 2.0;
    let max_bar_h = h - 10.0 * scale;
    // Min height > width: idle bars read as short pills instead of dots,
    // so the resting silhouette is clearly a row of bars.
    let min_bar_h = bar_w * 2.5;
    let r = bar_w / 2.0;

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 235);
    paint.anti_alias = true;

    let mut pb = PathBuilder::new();
    for (i, &amp_norm) in bar_heights.iter().enumerate() {
        let amp = amp_norm.clamp(0.0, 1.0);
        let bh = (min_bar_h + amp * (max_bar_h - min_bar_h)).max(min_bar_h);
        let x = start_x + i as f32 * (bar_w + bar_gap);
        let y = cy - bh / 2.0;
        rounded_rect(&mut pb, x, y, bar_w, bh, r);
    }
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let r = r.min(w / 2.0).min(h / 2.0);
    if r <= 0.5 {
        if let Some(rect) = Rect::from_xywh(x, y, w, h) {
            pb.push_rect(rect);
        }
        return;
    }
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
}

// Convert tiny-skia's premultiplied RGBA into the premultiplied BGRA that
// UpdateLayeredWindow + AC_SRC_ALPHA expects.
pub fn pixmap_to_premul_bgra(pm: &Pixmap, dst: &mut [u8]) {
    let src = pm.data();
    debug_assert_eq!(src.len(), dst.len());
    for i in 0..(src.len() / 4) {
        dst[i * 4] = src[i * 4 + 2];     // B
        dst[i * 4 + 1] = src[i * 4 + 1]; // G
        dst[i * 4 + 2] = src[i * 4];     // R
        dst[i * 4 + 3] = src[i * 4 + 3]; // A
    }
}
