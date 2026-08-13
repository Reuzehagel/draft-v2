// tiny-skia renderer for the pill. Produces a premultiplied BGRA byte buffer
// suitable for UpdateLayeredWindow with AC_SRC_ALPHA.
//
// There is exactly one entry point, [`draw`], and it draws whatever [`Geom`] it
// is handed. Per-mode renderers are gone: the mode's *appearance* is its Geom
// (see `pill::geom`), and a frame mid-transition is a Geom that belongs to no
// mode at all — which a `draw_recording`/`draw_success` split has no way to
// express.
//
// The Geom is drawn centred in the pixmap rather than filling it. The window
// sits at a fixed envelope big enough for the largest mode, so a 36x10 nub and
// a 62x28 recording pill are the same window with different pixels in it —
// nothing is resized, moved, or reallocated to run an animation.

use crate::pill::geom::Geom;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

// The bar row's proportions, authored against an 86x42 draft and kept as
// fractions of the *body* so the same drawing code holds at any size the morph
// passes through.

/// Bar width and the gap between bars, as fractions of the body width: the row
/// keeps its share of the pill's span rather than a fixed size.
const BAR_W_RATIO: f32 = 2.5 / 86.0;
const BAR_GAP_RATIO: f32 = 2.5 / 86.0;
/// Clear space above and below the tallest bar, as a fraction of body height.
const BAR_PAD_RATIO: f32 = 5.0 / 42.0;

/// Control-point distance, as a fraction of the radius, that makes a cubic
/// bezier approximate a quarter circle.
const KAPPA: f32 = 0.552_284_8;

/// Every length the bar row is drawn from, derived from the body the Geom
/// describes — not from the pixmap, which is the envelope and stays put.
struct Bars {
    bar_w: f32,
    bar_gap: f32,
    bar_min_h: f32,
    bar_max_h: f32,
}

impl Bars {
    fn new(body_w: f32, body_h: f32) -> Self {
        let bar_w = body_w * BAR_W_RATIO;
        // The padding is a share of the body, but on a body short enough that
        // it no longer clears its own inset the body wins: bars never poke
        // through the hairline.
        let bar_max_h = (body_h - 2.0 * (body_h * BAR_PAD_RATIO)).max(0.0);
        Self {
            bar_w,
            bar_gap: body_w * BAR_GAP_RATIO,
            // Min height > width: idle bars read as short pills instead of
            // dots, so the resting silhouette is clearly a row of bars. On a
            // body far wider than it is tall that would exceed the height
            // budget, so it yields to the maximum.
            bar_min_h: (bar_w * 2.5).min(bar_max_h),
            bar_max_h,
        }
    }

    /// Total width of a row of `count` bars, gaps included.
    fn span(&self, count: usize) -> f32 {
        count as f32 * self.bar_w + count.saturating_sub(1) as f32 * self.bar_gap
    }
}

fn clear_transparent(pm: &mut Pixmap) {
    pm.fill(Color::TRANSPARENT);
}

/// Draw one frame: the `geom`'s body centred in `pm`, with `bar_heights`
/// (0..1 amplitudes) drawn across it at the Geom's bar opacity.
///
/// `scale` is the DPI/supersample factor. It governs the hairline's width and
/// the transparent margin around the body, both of which are device-pixel
/// quantities rather than proportional ones; every other length comes off the
/// Geom, which is already in logical pixels and is multiplied up here.
/// The stroke width and the body rect a `geom` draws at `scale`.
///
/// The body's stroke centreline is inset by half the border width PLUS a ~1px
/// transparent margin. The margin is what stops the outer edge looking
/// pixelated: a centred stroke ending exactly at the body's boundary has its
/// anti-aliased falloff clipped (nowhere to fade into), so the curve
/// stair-steps. The margin gives that falloff room inside the pixmap.
fn body_of(geom: &Geom, scale: f32) -> (f32, f32, f32) {
    let border_w = (geom.border_w * scale).max(1.0);
    let inset = border_w * 0.5 + scale;
    (
        border_w,
        (geom.w * scale - 2.0 * inset).max(0.0),
        (geom.h * scale - 2.0 * inset).max(0.0),
    )
}

pub fn draw(pm: &mut Pixmap, scale: f32, geom: &Geom, bar_heights: &[f32]) {
    clear_transparent(pm);

    let (border_w, body_w, body_h) = body_of(geom, scale);
    if body_w <= 0.0 || body_h <= 0.0 {
        return;
    }
    // Centred in the envelope: the pill grows and shrinks about its own middle,
    // which is what keeps it bottom-centred on screen at every size.
    let x = (pm.width() as f32 - body_w) / 2.0;
    let y = (pm.height() as f32 - body_h) / 2.0;
    let radius = (geom.radius * scale).min(body_h / 2.0).min(body_w / 2.0);

    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, x, y, body_w, body_h, radius);
    let Some(path) = pb.finish() else {
        return;
    };

    let mut fill = Paint::default();
    fill.set_color_rgba8(
        geom.fill.0,
        geom.fill.1,
        geom.fill.2,
        alpha_u8(geom.fill_a),
    );
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    let mut edge = Paint::default();
    edge.set_color_rgba8(
        geom.border.0,
        geom.border.1,
        geom.border.2,
        alpha_u8(geom.border_a),
    );
    edge.anti_alias = true;
    pm.stroke_path(
        &path,
        &edge,
        &Stroke {
            width: border_w,
            ..Default::default()
        },
        Transform::identity(),
        None,
    );

    if geom.bars > 0.0 {
        draw_bars(pm, &Bars::new(body_w, body_h), bar_heights, geom.bars);
    }
}

/// 0..255 with the clamp that stops an out-of-range alpha turning into a
/// blanked frame — `as u8` maps a negative float to 0, which would blink the
/// whole pill off for one frame.
fn alpha_u8(a: f32) -> u8 {
    a.clamp(0.0, 255.0) as u8
}

fn draw_bars(pm: &mut Pixmap, g: &Bars, bar_heights: &[f32], opacity: f32) {
    if bar_heights.is_empty() || g.bar_w <= 0.0 {
        return;
    }
    let total_w = g.span(bar_heights.len());
    let start_x = (pm.width() as f32 - total_w) / 2.0;
    let cy = pm.height() as f32 / 2.0;
    let r = g.bar_w / 2.0;

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, alpha_u8(235.0 * opacity.clamp(0.0, 1.0)));
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
        dst[i * 4] = src[i * 4 + 2]; // B
        dst[i * 4 + 1] = src[i * 4 + 1]; // G
        dst[i * 4 + 2] = src[i * 4]; // R
        dst[i * 4 + 3] = src[i * 4 + 3]; // A
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pill::core::{Origin, PillMode};
    use crate::pill::geom::{ENVELOPE_H, ENVELOPE_W};
    use crate::pill::BAR_COUNT;
    use tiny_skia::PremultipliedColorU8;

    /// A flat row — what every state but recording actually draws.
    const FLAT: [f32; BAR_COUNT] = [0.0; BAR_COUNT];

    const REC: PillMode = PillMode::Recording {
        origin: Origin::Hotkey,
    };

    fn envelope() -> Pixmap {
        Pixmap::new(ENVELOPE_W, ENVELOPE_H).unwrap()
    }

    /// Draw a mode into a fresh envelope-sized pixmap at 1x.
    fn frame(mode: PillMode, bars: &[f32]) -> Pixmap {
        let mut pm = envelope();
        draw(&mut pm, 1.0, &Geom::of(mode), bars);
        pm
    }

    fn brightest(p: PremultipliedColorU8) -> u8 {
        p.red().max(p.green()).max(p.blue())
    }

    /// The most saturated pixel across the body's left edge on the row through
    /// `y`, and one clear of both that edge and the bar row.
    fn edge_and_body(pm: &Pixmap, left: u32, y: u32) -> (u8, u8) {
        let edge = (left..left + 4)
            .map(|x| pm.pixel(x, y).unwrap())
            .max_by_key(|p| brightest(*p))
            .unwrap();
        (brightest(edge), brightest(pm.pixel(left + 8, y).unwrap()))
    }

    /// The nub is a marker, so what matters is that it is *there*: a body of
    /// the settled size, with a light edge, and no bars inside it.
    #[test]
    fn the_nub_draws_a_small_bare_body_with_a_light_edge() {
        let pm = frame(PillMode::Idle, &FLAT);
        let cy = ENVELOPE_H / 2;
        // 36 wide, centred in a 62-wide envelope: the body starts at x=13.
        let left = (ENVELOPE_W - 36) / 2;
        // Outside the nub is untouched — the envelope is not the pill.
        assert_eq!(pm.pixel(1, cy).unwrap().alpha(), 0, "the envelope is padding");
        assert_eq!(pm.pixel(1, 1).unwrap().alpha(), 0);
        let (edge, body) = edge_and_body(&pm, left, cy);
        assert!(edge > 60, "nub edge too dark to separate ({edge})");
        assert!(edge > body * 3, "nub edge ({edge}) barely differs from its body ({body})");
        // No bars: the centre column is body, not the white of a bar.
        let centre = pm.pixel(ENVELOPE_W / 2, cy).unwrap();
        assert!(brightest(centre) < 40, "the nub drew a bar row: {centre:?}");
    }

    /// The nub is 36x10 and no larger. It is measured off the drawn pixels
    /// rather than off the Geom, so a renderer that quietly filled the envelope
    /// would fail here even with the Geom untouched.
    #[test]
    fn the_nub_occupies_the_settled_rect() {
        let pm = frame(PillMode::Idle, &FLAT);
        let opaque = |x: u32, y: u32| pm.pixel(x, y).unwrap().alpha() > 8;
        let cy = ENVELOPE_H / 2;
        let cx = ENVELOPE_W / 2;
        let width = (0..ENVELOPE_W).filter(|&x| opaque(x, cy)).count();
        let height = (0..ENVELOPE_H).filter(|&y| opaque(cx, y)).count();
        // Within a pixel of the settled size, either side of the ~1px
        // transparent margin the anti-aliased edge fades into.
        assert!((34..=36).contains(&width), "nub width {width}");
        assert!((8..=10).contains(&height), "nub height {height}");
    }

    /// `Hidden` is the nub's shape at alpha 0 — so it draws nothing at all,
    /// and there is never a frame where the shape is ambiguous.
    #[test]
    fn hidden_draws_nothing() {
        let pm = frame(PillMode::Hidden, &FLAT);
        assert!(
            pm.pixels().iter().all(|p| p.alpha() == 0),
            "Hidden put pixels on screen"
        );
    }

    /// The near-black body is invisible on a black desktop, and fill alpha is
    /// not the dial that fixes it — a light edge is. Every mode has to carry
    /// one, including those that stroke an accent of their own.
    #[test]
    fn every_mode_draws_a_light_edge_around_a_dark_body() {
        let modes: [(&str, PillMode); 5] = [
            ("idle", PillMode::Idle),
            ("recording", REC),
            ("processing", PillMode::Processing { since: now() }),
            (
                "success",
                PillMode::Done {
                    ok: true,
                    since: now(),
                },
            ),
            (
                "error",
                PillMode::Done {
                    ok: false,
                    since: now(),
                },
            ),
        ];
        for (name, mode) in modes {
            let g = Geom::of(mode);
            let pm = frame(mode, &FLAT);
            let left = ((ENVELOPE_W as f32 - g.w) / 2.0).round() as u32;
            let (edge, body) = edge_and_body(&pm, left, ENVELOPE_H / 2);
            assert!(edge > 60, "{name}: edge too dark to separate ({edge})");
            assert!(
                edge > body * 3,
                "{name}: edge ({edge}) barely differs from the body ({body})"
            );
        }
    }

    fn now() -> std::time::Instant {
        std::time::Instant::now()
    }

    /// A frame mid-morph is a Geom belonging to no mode, and it has to draw
    /// like one — a body between the two sizes, still with its edge.
    #[test]
    fn a_frame_mid_morph_draws_a_body_between_the_two_modes() {
        let half = Geom::of(PillMode::Idle).lerp(Geom::of(REC), 0.5);
        let mut pm = envelope();
        draw(&mut pm, 1.0, &half, &FLAT);
        let cy = ENVELOPE_H / 2;
        let width = (0..ENVELOPE_W)
            .filter(|&x| pm.pixel(x, cy).unwrap().alpha() > 8)
            .count();
        assert!((36..=62).contains(&width), "mid-morph width {width}");
        assert!(width > 40 && width < 58, "mid-morph is at an endpoint: {width}");
    }

    /// The bar row has to stay a *visibly* variable row at the shipped size: if
    /// the resting floor eats most of the budget, quiet and loud speech stop
    /// being tellable apart and the pill reads as decoration.
    #[test]
    fn quiet_and_loud_stay_distinguishable_at_the_shipped_size() {
        // Off the same body derivation `draw` uses, not a hand-copy of its
        // inset arithmetic — the two would drift.
        let (_, body_w, body_h) = body_of(&Geom::of(REC), 1.0);
        let bars = Bars::new(body_w, body_h);
        let at = |amp: f32| bars.bar_min_h + amp * (bars.bar_max_h - bars.bar_min_h);
        assert!(bars.bar_min_h < bars.bar_max_h * 0.25);
        assert!(at(0.3) - at(0.0) > 1.0);
        assert!(at(1.0) - at(0.3) > 1.0);
    }

    /// Bars must sit inside the body at every size the morph passes through,
    /// not just the one the literals were picked for.
    #[test]
    fn bars_stay_inside_the_body_at_any_size() {
        for (w, h) in [
            (86.0, 42.0),
            (62.0, 28.0),
            (43.0, 21.0),
            (36.0, 10.0),
            (24.0, 24.0),
            (120.0, 30.0),
            (30.0, 8.0),
        ] {
            let g = Bars::new(w, h);
            let side = (w - g.span(BAR_COUNT)) / 2.0;
            assert!(side >= 0.0, "{w}x{h}: bars overflow the body horizontally");
            assert!(g.bar_max_h <= h, "{w}x{h}: bars overflow the body vertically");
            assert!(
                g.bar_w > 0.0 && g.bar_min_h <= g.bar_max_h,
                "{w}x{h}: bar heights invert"
            );
        }
    }

    /// The pixmap is in physical pixels, so a 2x scale is the same pill at
    /// twice the size.
    #[test]
    fn the_body_scales_with_the_dpi_factor() {
        let g = Geom::of(PillMode::Idle);
        let mut one = Pixmap::new(ENVELOPE_W, ENVELOPE_H).unwrap();
        let mut two = Pixmap::new(ENVELOPE_W * 2, ENVELOPE_H * 2).unwrap();
        draw(&mut one, 1.0, &g, &FLAT);
        draw(&mut two, 2.0, &g, &FLAT);
        let count = |pm: &Pixmap, y: u32| {
            (0..pm.width())
                .filter(|&x| pm.pixel(x, y).unwrap().alpha() > 8)
                .count()
        };
        let (a, b) = (count(&one, ENVELOPE_H / 2), count(&two, ENVELOPE_H));
        assert!(
            (b as i32 - 2 * a as i32).abs() <= 3,
            "1x drew {a} px, 2x drew {b}"
        );
    }

    /// A fully-rounded corner has to be a real circular arc. The old quadratic
    /// form bulged past the circle at 45 degrees, which shows the moment
    /// anything draws a disc — and the nub, at radius 5 on a 10px body, is
    /// exactly that.
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
            pm.pixel((20.0 + d).floor() as u32, (20.0 - d).floor() as u32)
                .unwrap()
                .alpha()
        };
        assert!(at(18.0) > 200, "inside the circle should be filled");
        assert!(at(20.8) < 40, "corner bulges past the circle");
    }
}
