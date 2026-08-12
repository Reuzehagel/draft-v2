// tiny-skia renderers for the pill. Produces a premultiplied BGRA byte
// buffer suitable for UpdateLayeredWindow with AC_SRC_ALPHA.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

// The pill's proportions, authored against the 86x42 recording pill and kept
// as fractions of the render target so the same drawing code holds at any
// size — a resized recording pill, or a much smaller idle one.

/// Corner radius as a fraction of the target's shorter side. Clamped to half
/// the body height, so small targets round fully on their own.
const RADIUS_RATIO: f32 = 18.0 / 42.0;
/// Bar width and the gap between bars, as fractions of the target width: the
/// row of bars keeps its share of the pill's span rather than a fixed size.
const BAR_W_RATIO: f32 = 2.5 / 86.0;
const BAR_GAP_RATIO: f32 = 2.5 / 86.0;
/// Clear space above and below the tallest bar, as a fraction of the target
/// height.
const BAR_PAD_RATIO: f32 = 5.0 / 42.0;

/// Control-point distance, as a fraction of the radius, that makes a cubic
/// bezier approximate a quarter circle.
const KAPPA: f32 = 0.552_284_8;

/// Every length the pill is drawn from, derived from the render target's
/// dimensions. `scale` is the DPI/supersample factor and only governs the
/// hairline border, which is a device-pixel quantity rather than a
/// proportional one.
struct Geometry {
    border_w: f32,
    /// Distance from the pixmap edge to the body's stroke centreline.
    inset: f32,
    body_w: f32,
    body_h: f32,
    corner_r: f32,
    bar_w: f32,
    bar_gap: f32,
    bar_min_h: f32,
    bar_max_h: f32,
}

impl Geometry {
    fn new(w: f32, h: f32, scale: f32) -> Self {
        // Inset by half the border width PLUS a ~1px transparent margin. The
        // margin is what stops the outer edge looking pixelated: a centred
        // stroke ending exactly at the pixmap boundary has its anti-aliased
        // falloff clipped (nowhere to fade into), so the curve stair-steps.
        // The margin gives that falloff room to blend to full transparency
        // inside the pixmap.
        let border_w = (1.0 * scale).max(1.0);
        let inset = border_w * 0.5 + 1.0 * scale;
        let body_w = w - 2.0 * inset;
        let body_h = h - 2.0 * inset;
        let bar_w = w * BAR_W_RATIO;
        // The padding is a share of the target, but on a target short enough
        // that it no longer clears the body's own inset, the body wins: bars
        // never poke through the hairline.
        let bar_max_h = (h - 2.0 * (h * BAR_PAD_RATIO)).min(body_h);
        Self {
            border_w,
            inset,
            body_w,
            body_h,
            corner_r: (w.min(h) * RADIUS_RATIO).min(body_h / 2.0),
            bar_w,
            bar_gap: w * BAR_GAP_RATIO,
            // Min height > width: idle bars read as short pills instead of
            // dots, so the resting silhouette is clearly a row of bars. On a
            // target far wider than it is tall that would exceed the height
            // budget, so it yields to the maximum.
            bar_min_h: (bar_w * 2.5).min(bar_max_h),
            bar_max_h,
        }
    }

    fn of(pm: &Pixmap, scale: f32) -> Self {
        Self::new(pm.width() as f32, pm.height() as f32, scale)
    }

    /// Total width of a row of `count` bars, gaps included.
    fn bars_span(&self, count: usize) -> f32 {
        count as f32 * self.bar_w + count.saturating_sub(1) as f32 * self.bar_gap
    }
}

// A calm, muted success green — distinct from the settings lime, not loud.
const SUCCESS: (u8, u8, u8) = (74, 188, 120);

// A muted red for the failure flash — readable as "something went wrong"
// without being an alarm. Matches the settings destructive tone.
const ERROR: (u8, u8, u8) = (214, 96, 96);

// Neutral cool-grey for the "working" pulse shown while transcription/paste
// is still in flight.
const PROCESSING: (u8, u8, u8) = (190, 192, 200);

pub fn clear_transparent(pm: &mut Pixmap) {
    pm.fill(Color::TRANSPARENT);
}

pub fn draw_recording(pm: &mut Pixmap, scale: f32, bar_heights: &[f32]) {
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    draw_pill_bg(pm, &g);
    draw_bars(pm, &g, bar_heights, 1.0);
}

/// Success state shown briefly after a capture ends: the hairline turns a soft
/// green while the (frozen) waveform bars hold. `alpha` (0..1) multiplies the
/// whole pill for the fade-out at the end.
pub fn draw_success(pm: &mut Pixmap, scale: f32, bar_heights: &[f32], alpha: f32) {
    // Clamp so an easing overshoot/undershoot can't produce a negative alpha
    // (which `as u8` would turn into 0, blanking the whole pill for a frame).
    let alpha = alpha.clamp(0.0, 1.0);
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    draw_pill_shape(pm, &g, SUCCESS, 235, alpha);
    draw_bars(pm, &g, bar_heights, alpha);
}

/// Failure state: same shape as success but a muted red border, telling the
/// user the transcript never made it (transcription or paste error) so they
/// can recover it from History. `alpha` drives the same end fade-out.
pub fn draw_error(pm: &mut Pixmap, scale: f32, bar_heights: &[f32], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    draw_pill_shape(pm, &g, ERROR, 235, alpha);
    draw_bars(pm, &g, bar_heights, alpha);
}

/// "Working" state shown while the worker transcribes and pastes. The frozen
/// waveform bars hold, dimmed, behind a neutral border that breathes via
/// `pulse` (0..1) so a multi-second cloud round-trip still reads as live.
pub fn draw_processing(pm: &mut Pixmap, scale: f32, bar_heights: &[f32], pulse: f32) {
    let pulse = pulse.clamp(0.0, 1.0);
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    // Border alpha breathes between a dim and a brighter grey.
    let border_a = (110.0 + 110.0 * pulse) as u8;
    draw_pill_shape(pm, &g, PROCESSING, border_a, 1.0);
    draw_bars(pm, &g, bar_heights, 0.45);
}

fn draw_pill_bg(pm: &mut Pixmap, g: &Geometry) {
    // Recording: faint soft-grey hairline, fully opaque.
    draw_pill_shape(pm, g, (170, 172, 178), 64, 1.0);
}

fn draw_pill_shape(pm: &mut Pixmap, g: &Geometry, border_rgb: (u8, u8, u8), border_a: u8, alpha: f32) {
    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, g.inset, g.inset, g.body_w, g.body_h, g.corner_r);
    let path = pb.finish().unwrap();

    let mut fill = Paint::default();
    fill.set_color_rgba8(13, 13, 13, (245.0 * alpha) as u8);
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    let mut border = Paint::default();
    border.set_color_rgba8(border_rgb.0, border_rgb.1, border_rgb.2, (border_a as f32 * alpha) as u8);
    border.anti_alias = true;
    let stroke = Stroke {
        width: g.border_w,
        ..Default::default()
    };
    pm.stroke_path(&path, &border, &stroke, Transform::identity(), None);
}

fn draw_bars(pm: &mut Pixmap, g: &Geometry, bar_heights: &[f32], alpha: f32) {
    if bar_heights.is_empty() {
        return;
    }
    let total_w = g.bars_span(bar_heights.len());
    let start_x = (pm.width() as f32 - total_w) / 2.0;
    let cy = pm.height() as f32 / 2.0;
    let r = g.bar_w / 2.0;

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, (235.0 * alpha) as u8);
    paint.anti_alias = true;

    let mut pb = PathBuilder::new();
    for (i, &amp_norm) in bar_heights.iter().enumerate() {
        let amp = amp_norm.clamp(0.0, 1.0);
        let bh = (g.bar_min_h + amp * (g.bar_max_h - g.bar_min_h)).max(g.bar_min_h);
        let x = start_x + i as f32 * (g.bar_w + g.bar_gap);
        let y = cy - bh / 2.0;
        rounded_rect(&mut pb, x, y, g.bar_w, bh, r);
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
    // Cubic corners with the circle constant, not quadratics with the control
    // point on the corner: the latter draws a squircle that bulges past the
    // circle at 45 degrees, which is invisible on a shallow corner but obvious
    // the moment a full radius is meant to read as a disc.
    let c = r * KAPPA;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + c, y, x + w, y + r - c, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + c, x + w - r + c, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - c, y + h, x, y + h - r + c, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - c, x + r - c, y, x + r, y);
    pb.close();
}

// Convert tiny-skia's premultiplied RGBA into the premultiplied BGRA that
// UpdateLayeredWindow + AC_SRC_ALPHA expects.
pub fn pixmap_to_premul_bgra(pm: &Pixmap, dst: &mut [u8]) {
    let src = pm.data();
    debug_assert_eq!(src.len(), dst.len());
    // Bound by the shorter buffer so a size desync can never become an
    // out-of-bounds write in release builds (where the assert is compiled out).
    let pixels = src.len().min(dst.len()) / 4;
    for i in 0..pixels {
        dst[i * 4] = src[i * 4 + 2];     // B
        dst[i * 4 + 1] = src[i * 4 + 1]; // G
        dst[i * 4 + 2] = src[i * 4];     // R
        dst[i * 4 + 3] = src[i * 4 + 3]; // A
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pill::BAR_COUNT;

    // The geometry the pill shipped with at its 86x42 logical size. Deriving
    // the values from the target must reproduce these exactly, or the shipped
    // pill changes appearance.
    #[test]
    fn reference_size_reproduces_the_shipped_geometry() {
        let g = Geometry::new(86.0, 42.0, 1.0);
        assert_eq!(g.border_w, 1.0);
        assert_eq!(g.inset, 1.5);
        assert_eq!(g.corner_r, 18.0);
        assert_eq!(g.bar_w, 2.5);
        assert_eq!(g.bar_gap, 2.5);
        assert_eq!(g.bar_min_h, 6.25);
        assert_eq!(g.bar_max_h, 32.0);
    }

    // The pixmap is already in physical pixels, so a 2x target is the same
    // pill at twice the size — every length doubles.
    #[test]
    fn geometry_scales_with_the_target() {
        let one = Geometry::new(86.0, 42.0, 1.0);
        let two = Geometry::new(172.0, 84.0, 2.0);
        assert_eq!(two.corner_r, one.corner_r * 2.0);
        assert_eq!(two.bar_w, one.bar_w * 2.0);
        assert_eq!(two.bar_gap, one.bar_gap * 2.0);
        assert_eq!(two.bar_max_h, one.bar_max_h * 2.0);
        assert_eq!(two.inset, one.inset * 2.0);
    }

    // Small targets clamp to a fully-rounded silhouette rather than keeping a
    // proportionally smaller corner.
    #[test]
    fn small_target_is_fully_rounded() {
        let g = Geometry::new(20.0, 20.0, 1.0);
        assert_eq!(g.corner_r, g.body_h / 2.0);
    }

    // Bars must sit inside the body with symmetric margins at any size, not
    // just at the size the literals were picked for.
    #[test]
    fn bars_stay_inside_the_body_at_any_size() {
        // Includes sizes short enough that the proportional padding no longer
        // clears the body's inset, and one far wider than it is tall.
        for (w, h) in [
            (86.0, 42.0),
            (43.0, 21.0),
            (24.0, 24.0),
            (120.0, 30.0),
            (40.0, 12.0),
            (30.0, 8.0),
            (120.0, 10.0),
        ] {
            let g = Geometry::new(w, h, 1.0);
            let total_w = g.bars_span(BAR_COUNT);
            let side = (w - total_w) / 2.0;
            assert!(
                side >= g.inset,
                "{w}x{h}: bars overflow the body horizontally"
            );
            assert!(
                g.bar_max_h <= g.body_h,
                "{w}x{h}: bars overflow the body vertically"
            );
            assert!(
                g.bar_w > 0.0 && g.bar_min_h <= g.bar_max_h,
                "{w}x{h}: bar heights invert"
            );
        }
    }

    // The corner keeps the reference pill's proportion as the target shrinks
    // — a constant fraction of the size — and reaches the fully-rounded clamp
    // at the small end.
    #[test]
    fn corners_stay_proportional_down_to_fully_rounded() {
        for (w, h) in [(86.0, 42.0), (60.0, 30.0), (43.0, 21.0), (20.0, 20.0)] {
            let g = Geometry::new(w, h, 1.0);
            let full = g.body_h / 2.0;
            assert!(g.corner_r <= full, "{w}x{h}: corner exceeds a full round");
            // Never more than the reference's own shortfall from full.
            assert!(
                full - g.corner_r <= 1.5,
                "{w}x{h}: corner is flatter than the shipped pill's"
            );
        }
    }

    // A fully-rounded corner has to be a real circular arc. The old quadratic
    // form bulged past the circle at 45 degrees, which shows the moment
    // anything draws a disc.
    #[test]
    fn full_radius_corners_are_circular() {
        let mut pm = Pixmap::new(40, 40).unwrap();
        let mut pb = PathBuilder::new();
        rounded_rect(&mut pb, 0.0, 0.0, 40.0, 40.0, 20.0);
        let path = pb.finish().unwrap();
        let mut paint = Paint::default();
        paint.set_color_rgba8(255, 255, 255, 255);
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

        // 45-degree ray from the centre: inside the circle stays filled,
        // beyond it must be empty (a quadratic corner reaches ~21.2 here).
        let at = |dist: f32| {
            let d = dist / std::f32::consts::SQRT_2;
            let px = pm
                .pixel((20.0 + d).floor() as u32, (20.0 - d).floor() as u32)
                .unwrap();
            px.alpha()
        };
        assert!(at(18.0) > 200, "inside the circle should be filled");
        assert!(at(20.8) < 40, "corner bulges past the circle");
    }
}
