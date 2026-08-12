// tiny-skia renderers for the pill. Produces a premultiplied BGRA byte
// buffer suitable for UpdateLayeredWindow with AC_SRC_ALPHA.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

// The pill's proportions, authored against an 86x42 draft and kept as fractions
// of the render target so the same drawing code holds at any size — the 62x28
// the session pill settled on, or a much smaller idle one.

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

/// A stroke on the pill's edge: a colour and its own alpha, before the whole
/// pill's fade is applied on top.
#[derive(Clone, Copy)]
struct Edge {
    rgb: (u8, u8, u8),
    a: u8,
}

// The edge the pill is *found* by, drawn on every mode under whatever accent
// that mode adds. The body is near-black, so on a black desktop nothing but a
// light edge separates it — and fill alpha is not the dial that fixes that: a
// dark body on a dark background is invisible at any alpha.
const HAIRLINE: Edge = Edge {
    rgb: (220, 224, 232),
    a: 120,
};

/// The bar row's opacity once the handoff is over and the row is only being
/// held. Full opacity during the handoff itself, since the row is still the
/// waveform the user was watching.
const PROCESSING_BARS_ALPHA: f32 = 0.45;

pub fn clear_transparent(pm: &mut Pixmap) {
    pm.fill(Color::TRANSPARENT);
}

pub fn draw_recording(pm: &mut Pixmap, scale: f32, bar_heights: &[f32]) {
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    // Recording wears the bare hairline: the bars are what say "live".
    draw_pill_shape(pm, &g, None, 1.0);
    draw_bars(pm, &g, bar_heights, 1.0);
}

/// Success state shown briefly after a capture ends: a soft green over the
/// hairline, above the flat bar row. `alpha` (0..1) multiplies the whole pill
/// for the fade-out at the end.
pub fn draw_success(pm: &mut Pixmap, scale: f32, bar_heights: &[f32], alpha: f32) {
    // Clamp so an easing overshoot/undershoot can't produce a negative alpha
    // (which `as u8` would turn into 0, blanking the whole pill for a frame).
    let alpha = alpha.clamp(0.0, 1.0);
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    draw_pill_shape(pm, &g, Some(Edge { rgb: SUCCESS, a: 235 }), alpha);
    draw_bars(pm, &g, bar_heights, alpha);
}

/// Failure state: same shape as success but a muted red, telling the user the
/// transcript never made it (transcription or paste error) so they can recover
/// it from History. `alpha` drives the same end fade-out.
pub fn draw_error(pm: &mut Pixmap, scale: f32, bar_heights: &[f32], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    draw_pill_shape(pm, &g, Some(Edge { rgb: ERROR, a: 235 }), alpha);
    draw_bars(pm, &g, bar_heights, alpha);
}

/// "Working" state shown while the worker transcribes and pastes: a flat bar
/// row under a neutral border that breathes via `pulse` (0..1), so a
/// multi-second cloud round-trip still reads as live.
///
/// `handoff_progress` is how far past the Recording → Processing handoff this
/// frame is — 0 at the mode change, 1 once the handoff is over. It drives
/// everything this mode does *not* share with recording: the neutral border
/// crossfades in over it, and the bar row dims to [`PROCESSING_BARS_ALPHA`] on
/// the same ramp. At 0 this draws exactly what recording draws, which is the
/// point — the handoff is 320 ms of animation, not a swap with animation either
/// side of it.
pub fn draw_processing(
    pm: &mut Pixmap,
    scale: f32,
    bar_heights: &[f32],
    pulse: f32,
    handoff_progress: f32,
) {
    let pulse = pulse.clamp(0.0, 1.0);
    let progress = handoff_progress.clamp(0.0, 1.0);
    let g = Geometry::of(pm, scale);
    clear_transparent(pm);
    // Border alpha breathes between a dim and a brighter grey, faded in by the
    // handoff so the colour arrives with the bars' fall rather than ahead of it.
    let border_a = ((110.0 + 110.0 * pulse) * progress) as u8;
    draw_pill_shape(
        pm,
        &g,
        Some(Edge {
            rgb: PROCESSING,
            a: border_a,
        }),
        1.0,
    );
    draw_bars(
        pm,
        &g,
        bar_heights,
        1.0 - (1.0 - PROCESSING_BARS_ALPHA) * progress,
    );
}

/// The body, its hairline, and — for every mode but recording — an `accent`
/// stroked over that hairline on the same 1px path. Over rather than instead:
/// the accent is what the mode *says*, the hairline is how the pill is found at
/// all, and a green that has breathed or faded down must not take the pill's
/// edge with it.
fn draw_pill_shape(pm: &mut Pixmap, g: &Geometry, accent: Option<Edge>, alpha: f32) {
    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, g.inset, g.inset, g.body_w, g.body_h, g.corner_r);
    let path = pb.finish().unwrap();

    let mut fill = Paint::default();
    fill.set_color_rgba8(13, 13, 13, (245.0 * alpha) as u8);
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    let stroke = Stroke {
        width: g.border_w,
        ..Default::default()
    };
    let mut edge = |e: Edge| {
        let mut paint = Paint::default();
        paint.set_color_rgba8(e.rgb.0, e.rgb.1, e.rgb.2, (e.a as f32 * alpha) as u8);
        paint.anti_alias = true;
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    };
    edge(HAIRLINE);
    if let Some(accent) = accent {
        edge(accent);
    }
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

    use crate::pill::{PILL_H, PILL_W};
    use tiny_skia::PremultipliedColorU8;

    /// A flat row — what every state but recording actually draws.
    const FLAT: [f32; BAR_COUNT] = [0.0; BAR_COUNT];

    /// One pill mode, named and paired with a call that draws it.
    type Mode = (&'static str, fn(&mut Pixmap));

    // The ratios pinned at the size the session pill actually ships at, so a
    // change to one of them shows up as a number here rather than only on
    // screen. Judged by eye at 62x28 — see #41.
    #[test]
    fn the_shipped_size_derives_the_geometry_it_was_judged_at() {
        let g = Geometry::new(PILL_W as f32, PILL_H as f32, 1.0);
        assert_eq!(g.border_w, 1.0);
        assert_eq!(g.inset, 1.5);
        // Half a pixel short of half the body height — the ratio's own slight
        // flattening, held over from 86x42 and invisible at this size.
        assert_eq!(g.corner_r, 12.0);
        assert!(g.body_h / 2.0 - g.corner_r <= 0.5);
        assert!((g.bar_w - 1.802).abs() < 0.001);
        assert!((g.bar_gap - 1.802).abs() < 0.001);
        assert!((g.bar_min_h - 4.506).abs() < 0.001);
        assert!((g.bar_max_h - 21.333).abs() < 0.001);
    }

    // The bars have to stay a *visibly* variable row at the reduced height: if
    // the resting floor eats most of the budget, quiet and loud speech stop
    // being tellable apart and the pill reads as decoration.
    #[test]
    fn quiet_and_loud_stay_distinguishable_at_the_shipped_size() {
        let g = Geometry::new(PILL_W as f32, PILL_H as f32, 1.0);
        let at = |amp: f32| g.bar_min_h + amp * (g.bar_max_h - g.bar_min_h);
        // The resting floor is a minority of the row's height...
        assert!(g.bar_min_h < g.bar_max_h * 0.25);
        // ...and a quiet passage already clears it by more than a device pixel,
        // with room above for a loud one to go on growing.
        assert!(at(0.3) - at(0.0) > 1.0);
        assert!(at(1.0) - at(0.3) > 1.0);
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

    /// Draw a mode at the shipped size and sample the row through its middle:
    /// the most saturated pixel across the left edge's anti-aliased falloff, and
    /// one inside the body clear of both that edge and the bar row.
    fn edge_and_body(draw: fn(&mut Pixmap)) -> (PremultipliedColorU8, PremultipliedColorU8) {
        let mut pm = Pixmap::new(PILL_W, PILL_H).unwrap();
        draw(&mut pm);
        let y = PILL_H / 2;
        let edge = (0..4)
            .map(|x| pm.pixel(x, y).unwrap())
            .max_by_key(|p| brightest(*p))
            .unwrap();
        (edge, pm.pixel(8, y).unwrap())
    }

    fn brightest(p: PremultipliedColorU8) -> u8 {
        p.red().max(p.green()).max(p.blue())
    }

    // The near-black body is invisible on a black desktop, and fill alpha is not
    // the dial that fixes it — a light edge is. Every mode has to carry one,
    // including the ones that draw an accent of their own over it.
    #[test]
    fn every_mode_draws_a_light_edge_around_a_dark_body() {
        let modes: [Mode; 5] = [
            ("recording", |pm| draw_recording(pm, 1.0, &FLAT)),
            // The dimmest point of the breath, on a settled handoff: the mode's
            // own border is at its faintest here, so this is where the hairline
            // earns its keep.
            ("processing", |pm| draw_processing(pm, 1.0, &FLAT, 0.0, 1.0)),
            ("success", |pm| draw_success(pm, 1.0, &FLAT, 1.0)),
            ("error", |pm| draw_error(pm, 1.0, &FLAT, 1.0)),
            // Mid-fade, where the whole pill is half transparent.
            ("success fading", |pm| draw_success(pm, 1.0, &FLAT, 0.5)),
        ];
        for (name, draw) in modes {
            let (edge, body) = edge_and_body(draw);
            let (edge, body) = (brightest(edge), brightest(body));
            assert!(edge > 60, "{name}: edge too dark to separate ({edge})");
            assert!(
                edge > body * 3,
                "{name}: edge ({edge}) barely differs from the body ({body})"
            );
        }
    }

    // The handoff is 320 ms of animation, not a swap with animation either side
    // of it: at the mode change Processing has to draw what recording drew, or
    // the border and the bar row both step on the frame the fall begins.
    #[test]
    fn processing_starts_the_handoff_looking_exactly_like_recording() {
        let bars = [0.4, 0.8, 0.6, 1.0, 0.5, 0.7, 0.3];
        let mut rec = Pixmap::new(PILL_W, PILL_H).unwrap();
        draw_recording(&mut rec, 1.0, &bars);
        let mut proc = Pixmap::new(PILL_W, PILL_H).unwrap();
        // Any pulse: at zero progress the breath is faded out entirely.
        draw_processing(&mut proc, 1.0, &bars, 1.0, 0.0);
        assert_eq!(rec.data(), proc.data());
    }

    // The flash is the one thing the user reads at a glance, and it survives
    // being a 1px hairline only because the two colours are unmistakable.
    #[test]
    fn the_green_and_red_flashes_stay_far_apart() {
        let edge_rg = |draw: fn(&mut Pixmap)| {
            let p = edge_and_body(draw).0;
            (p.red(), p.green())
        };
        let (sr, sg) = edge_rg(|pm| draw_success(pm, 1.0, &FLAT, 1.0));
        let (er, eg) = edge_rg(|pm| draw_error(pm, 1.0, &FLAT, 1.0));
        assert!(sg > sr + 40, "success reads green: r{sr} g{sg}");
        assert!(er > eg + 40, "error reads red: r{er} g{eg}");
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
