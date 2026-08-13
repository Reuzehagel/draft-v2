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
// sits at a fixed envelope big enough for the largest mode, so a 36x10 nub, a
// 62x28 recording pill and a 118x32 button bar are the same window with
// different pixels in it — nothing is resized, moved, or reallocated to run an
// animation.
//
// The one thing the Geom does not carry is which button the cursor is on: that
// is per-button state a single whole-pill Geom cannot express (#29), so it
// arrives beside it as a `Slot` per button.

use crate::pill::core::{island_centre, slab, Button, BUTTONS, BUTTON_COUNT, CENTRE, GLYPH_BOX};
use crate::pill::geom::{Geom, Slot};
use crate::pill::icons;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

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

pub fn draw(
    pm: &mut Pixmap,
    scale: f32,
    geom: &Geom,
    bar_heights: &[f32],
    slots: &[Slot; BUTTON_COUNT],
) {
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

    let islands = islands(pm, geom, scale, border_w);
    // Behind the body, because that is what "slide out from behind Dictate"
    // means: at the start of the fold-out the flankers are underneath it.
    if geom.buttons > 0.0 {
        draw_hit_strip(pm, scale, geom.buttons);
        for (i, island) in islands.iter().enumerate() {
            if i != CENTRE {
                fill_island(pm, geom, island, border_w, geom.buttons);
            }
        }
    }

    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, x, y, body_w, body_h, radius);
    let Some(path) = pb.finish() else {
        return;
    };

    let mut fill = Paint::default();
    fill.set_color_rgba8(geom.fill.0, geom.fill.1, geom.fill.2, alpha_u8(geom.fill_a));
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

    // On top of every island, its own and the body's alike — the indicator sits
    // inside the shape and the glyph on top of that.
    if geom.buttons > 0.0 {
        for (i, island) in islands.iter().enumerate() {
            draw_indicator(pm, island, scale, slots[i].hover * geom.buttons);
            draw_glyph(pm, &BUTTONS[i], island, scale, geom.buttons, slots[i]);
        }
    }
}

/// The bar's hit strip: the slabs, painted at the faintest alpha that is not a
/// hole.
///
/// **This is a hit region, not a visual.** A layered window is hit-tested by
/// per-pixel alpha, so the bare desktop between two islands is not merely
/// undecorated — it is a hole, and mouse input in it goes to whatever is
/// behind the pill. Which makes the 3px gaps exactly the dead zones the slabs
/// exist to abolish: sliding from Dictate to Settings would leave the window
/// (winit reports `CursorLeft`, the indicator goes out and comes back), and a
/// click landing in a gap would go to another app.
///
/// [`HIT_A`] is opaque enough for Windows to hit-test and far too little to
/// see: over any desktop it is about one percent of a shade.
///
/// It spans the slabs and no further, so the end padding stays a hole — inert
/// in the strongest sense, since it is not even the pill's to receive. And it
/// only appears once the fold-out is mostly done: a bar that swallowed clicks
/// across its full width before it had drawn itself would be catching them for
/// buttons that are not there yet.
fn draw_hit_strip(pm: &mut Pixmap, scale: f32, progress: f32) {
    if progress < 0.5 {
        return;
    }
    let (lo, hi) = (slab(0).0 * scale, slab(BUTTONS.len() - 1).1 * scale);
    let h = crate::pill::core::BAR_H * scale;
    let (cx, cy) = (pm.width() as f32 / 2.0, pm.height() as f32 / 2.0);
    let Some(rect) = Rect::from_ltrb(cx + lo, cy - h / 2.0, cx + hi, cy + h / 2.0) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, HIT_A);
    pm.fill_rect(rect, &paint, Transform::identity(), None);
}

/// The hit strip's alpha. Not 1: the surface is rendered at 4x and halved twice
/// on the way to the screen, and an alpha that rounds to nothing somewhere in
/// that chain is a hole again.
const HIT_A: u8 = 3;

/// One button's drawn shape this frame, in device pixels.
struct Island {
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
    r: f32,
}

/// Where every island is right now.
///
/// The centre island is the pill's own body — whatever size the morph has it
/// at — and the flankers are at `island_centre(i)` scaled by the growth
/// progress, which is `geom.buttons`. **Offset is a pure function of that one
/// number**: no per-button clock, so a frame mid-fold-out is still derived from
/// the Geom alone.
fn islands(pm: &Pixmap, geom: &Geom, scale: f32, border_w: f32) -> [Island; BUTTON_COUNT] {
    let (cx, cy) = (pm.width() as f32 / 2.0, pm.height() as f32 / 2.0);
    let (_, body_w, body_h) = body_of(geom, scale);
    // The same transparent margin the body gets, so a flanker's edge has room
    // to fade into instead of stair-stepping.
    let inset = border_w * 0.5 + scale;
    let progress = geom.buttons.clamp(0.0, 1.0);
    std::array::from_fn(|i| {
        if i == CENTRE {
            return Island {
                cx,
                cy,
                w: body_w,
                h: body_h,
                r: (geom.radius * scale).min(body_h / 2.0).min(body_w / 2.0),
            };
        }
        let b = &BUTTONS[i];
        let w = (b.w * scale - 2.0 * inset).max(0.0);
        let h = (b.height() * scale - 2.0 * inset).max(0.0);
        Island {
            cx: cx + island_centre(i) * scale * progress,
            cy,
            w,
            h,
            r: (b.radius() * scale).min(h / 2.0).min(w / 2.0),
        }
    })
}

fn island_path(island: &Island) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    rounded_rect(
        &mut pb,
        island.cx - island.w / 2.0,
        island.cy - island.h / 2.0,
        island.w,
        island.h,
        island.r,
    );
    pb.finish()
}

/// A flanker, in the body's own colours at `opacity` — it fades in as it slides
/// out, so the pair reads as one gesture.
fn fill_island(pm: &mut Pixmap, geom: &Geom, island: &Island, border_w: f32, opacity: f32) {
    if island.w <= 0.0 || island.h <= 0.0 {
        return;
    }
    let Some(path) = island_path(island) else {
        return;
    };
    let o = opacity.clamp(0.0, 1.0);
    let mut fill = Paint::default();
    fill.set_color_rgba8(
        geom.fill.0,
        geom.fill.1,
        geom.fill.2,
        alpha_u8(geom.fill_a * o),
    );
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    let mut edge = Paint::default();
    edge.set_color_rgba8(
        geom.border.0,
        geom.border.1,
        geom.border.2,
        alpha_u8(geom.border_a * o),
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
}

/// How far the hover indicator sits inside its island, in logical pixels.
const INDICATOR_INSET: f32 = 3.0;

/// The hover indicator's fill alpha at full strength.
const INDICATOR_A: f32 = 28.0;

/// The hovered button's fill: a rounded rect inset from the island's edges,
/// radius half its height — a shape inside a shape at any slot width, so the
/// circle and the stadium need no separate treatment.
fn draw_indicator(pm: &mut Pixmap, island: &Island, scale: f32, strength: f32) {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.0 {
        return;
    }
    let inset = INDICATOR_INSET * scale;
    let (w, h) = (island.w - 2.0 * inset, island.h - 2.0 * inset);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let mut pb = PathBuilder::new();
    rounded_rect(
        &mut pb,
        island.cx - w / 2.0,
        island.cy - h / 2.0,
        w,
        h,
        h / 2.0,
    );
    let Some(path) = pb.finish() else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, alpha_u8(INDICATOR_A * strength));
    paint.anti_alias = true;
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// The glyph's alpha at full attention: the hovered button's glyph is at full
/// opacity, and the two shares below are read against that.
const GLYPH_A: f32 = 255.0;

/// An unhovered glyph's share of that — present, but not competing with the one
/// under the cursor.
const GLYPH_IDLE: f32 = 0.7;

/// A disabled glyph's share. Faint enough to read as "not now" rather than as a
/// button that simply didn't respond.
const GLYPH_DISABLED: f32 = 0.35;

/// One button's icon, at the emphasis its slot is at.
fn draw_glyph(
    pm: &mut Pixmap,
    button: &Button,
    island: &Island,
    scale: f32,
    opacity: f32,
    slot: Slot,
) {
    let box_px = GLYPH_BOX * scale;
    let Some(path) = icons::glyph(button.icon, box_px) else {
        return;
    };
    let emphasis = if slot.enabled {
        GLYPH_IDLE + (1.0 - GLYPH_IDLE) * slot.hover.clamp(0.0, 1.0)
    } else {
        GLYPH_DISABLED
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        255,
        255,
        255,
        alpha_u8(GLYPH_A * emphasis * opacity.clamp(0.0, 1.0)),
    );
    paint.anti_alias = true;
    pm.stroke_path(
        &path,
        &paint,
        &Stroke {
            width: icons::stroke_width(box_px),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        },
        Transform::from_translate(island.cx, island.cy),
        None,
    );
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
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
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

    /// Every button live and none of them lit: the bar at rest, and what the
    /// modes that draw no bar at all are handed.
    const NO_SLOTS: [Slot; BUTTON_COUNT] = [Slot {
        hover: 0.0,
        enabled: true,
    }; BUTTON_COUNT];

    const REC: PillMode = PillMode::Recording {
        origin: Origin::Hotkey,
    };

    fn envelope() -> Pixmap {
        Pixmap::new(ENVELOPE_W, ENVELOPE_H).unwrap()
    }

    /// Draw a mode into a fresh envelope-sized pixmap at 1x.
    fn frame(mode: PillMode, bars: &[f32]) -> Pixmap {
        let mut pm = envelope();
        draw(&mut pm, 1.0, &Geom::of(mode), bars, &NO_SLOTS);
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
        assert_eq!(
            pm.pixel(1, cy).unwrap().alpha(),
            0,
            "the envelope is padding"
        );
        assert_eq!(pm.pixel(1, 1).unwrap().alpha(), 0);
        let (edge, body) = edge_and_body(&pm, left, cy);
        assert!(edge > 60, "nub edge too dark to separate ({edge})");
        assert!(
            edge > body * 3,
            "nub edge ({edge}) barely differs from its body ({body})"
        );
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
        draw(&mut pm, 1.0, &half, &FLAT, &NO_SLOTS);
        let cy = ENVELOPE_H / 2;
        let width = (0..ENVELOPE_W)
            .filter(|&x| pm.pixel(x, cy).unwrap().alpha() > 8)
            .count();
        assert!((36..=62).contains(&width), "mid-morph width {width}");
        assert!(
            width > 40 && width < 58,
            "mid-morph is at an endpoint: {width}"
        );
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
            assert!(
                g.bar_max_h <= h,
                "{w}x{h}: bars overflow the body vertically"
            );
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
        draw(&mut one, 1.0, &g, &FLAT, &NO_SLOTS);
        draw(&mut two, 2.0, &g, &FLAT, &NO_SLOTS);
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

    /// Draw the expanded bar at 1x, with `slots`.
    fn bar(slots: &[Slot; BUTTON_COUNT]) -> Pixmap {
        let mut pm = envelope();
        draw(&mut pm, 1.0, &Geom::of(PillMode::Expanded), &FLAT, slots);
        pm
    }

    /// Runs of drawn pixels across the row through the pill's centre.
    fn runs(pm: &Pixmap, y: u32) -> Vec<(u32, u32)> {
        let mut out: Vec<(u32, u32)> = Vec::new();
        for x in 0..pm.width() {
            if pm.pixel(x, y).unwrap().alpha() > 8 {
                match out.last_mut() {
                    Some(run) if run.1 + 1 >= x => run.1 = x,
                    _ => out.push((x, x)),
                }
            }
        }
        out
    }

    /// Three islands with bare desktop between them — not one wide body. The
    /// gaps are measured off the drawn pixels, so a renderer that filled
    /// between them would fail here whatever the button list said.
    #[test]
    fn the_expanded_bar_draws_three_islands_over_bare_desktop() {
        let pm = bar(&NO_SLOTS);
        let runs = runs(&pm, ENVELOPE_H / 2);
        assert_eq!(runs.len(), 3, "{runs:?}");
        let widths: Vec<u32> = runs.iter().map(|(a, b)| b - a + 1).collect();
        // 32, 48, 32, to within the ~1px margin each edge fades into.
        for (got, want) in widths.iter().zip([32, 48, 32]) {
            assert!(
                (got.abs_diff(want)) <= 2,
                "island widths {widths:?} against [32, 48, 32]"
            );
        }
        // And the whole bar spans its derived width, centred.
        let span = runs[2].1 - runs[0].0 + 1;
        assert!(
            span.abs_diff(crate::pill::core::bar_width() as u32) <= 2,
            "bar span {span}"
        );
    }

    /// Mid-fold-out the flankers are *behind* the centre, and they arrive at
    /// their slots by the end. The offset is the growth progress and nothing
    /// else, so this is the whole of the animation.
    #[test]
    fn the_flankers_slide_out_from_behind_the_centre() {
        let expanded = Geom::of(PillMode::Expanded);
        let at = |t: f32| {
            let mut pm = envelope();
            draw(
                &mut pm,
                1.0,
                &Geom::of(PillMode::Idle).lerp(expanded, t),
                &FLAT,
                &NO_SLOTS,
            );
            runs(&pm, ENVELOPE_H / 2)
        };
        // A quarter of the way in they have barely left, and are dim enough to
        // still be one silhouette with the body.
        let early = at(0.25);
        assert_eq!(
            early.len(),
            1,
            "the flankers left the body early: {early:?}"
        );
        // At the end, three islands at full separation.
        assert_eq!(at(1.0).len(), 3);
    }

    /// The indicator lights the hovered button and nothing else.
    #[test]
    fn the_hover_indicator_lights_only_the_hovered_button() {
        let mut slots = NO_SLOTS;
        let dark = bar(&slots);
        slots[0].hover = 1.0;
        let lit = bar(&slots);
        let cy = ENVELOPE_H / 2;
        // Inside the Copy island, clear of its glyph: the indicator's fill.
        let x = (ENVELOPE_W as f32 / 2.0 + crate::pill::core::island_centre(0)) as u32;
        let inside =
            |pm: &Pixmap, dx: i32| brightest(pm.pixel((x as i32 + dx) as u32, cy).unwrap());
        assert!(
            inside(&lit, -12) > inside(&dark, -12),
            "the hovered button did not light"
        );
        // The other two are untouched.
        for i in [1usize, 2] {
            let ox = (ENVELOPE_W as f32 / 2.0 + crate::pill::core::island_centre(i)) as u32;
            let (a, b) = (
                brightest(lit.pixel(ox - 12, cy).unwrap()),
                brightest(dark.pixel(ox - 12, cy).unwrap()),
            );
            assert_eq!(a, b, "button {i} lit with the cursor on another");
        }
    }

    /// A disabled Copy is drawn faint — visibly "not now" rather than a button
    /// that simply didn't respond.
    #[test]
    fn a_disabled_button_draws_a_faint_glyph() {
        let live = bar(&NO_SLOTS);
        let mut slots = NO_SLOTS;
        slots[0].enabled = false;
        let dead = bar(&slots);
        let cx = (ENVELOPE_W as f32 / 2.0 + crate::pill::core::island_centre(0)) as u32;
        let cy = ENVELOPE_H / 2;
        // The brightest pixel anywhere in the glyph box, which is the glyph.
        let glyph = |pm: &Pixmap| {
            (cx - 11..cx + 11)
                .flat_map(|x| (cy - 11..cy + 11).map(move |y| (x, y)))
                .map(|(x, y)| brightest(pm.pixel(x, y).unwrap()))
                .max()
                .unwrap()
        };
        assert!(
            (glyph(&dead) as f32) < glyph(&live) as f32 * 0.6,
            "disabled {} against live {}",
            glyph(&dead),
            glyph(&live)
        );
        assert!(glyph(&dead) > 20, "the disabled glyph vanished");
        // And only that button dims: Settings is untouched.
        let sx = (ENVELOPE_W as f32 / 2.0 + crate::pill::core::island_centre(2)) as u32;
        let settings = |pm: &Pixmap| {
            (sx - 11..sx + 11)
                .flat_map(|x| (cy - 11..cy + 11).map(move |y| (x, y)))
                .map(|(x, y)| brightest(pm.pixel(x, y).unwrap()))
                .max()
                .unwrap()
        };
        assert_eq!(settings(&dead), settings(&live));
    }

    /// The gaps between islands are drawn, faintly, and survive the trip to the
    /// screen.
    ///
    /// A layered window is hit-tested by per-pixel alpha, so an alpha of zero
    /// in the gap is a hole the mouse falls through — which would make the gaps
    /// the dead zones the slabs exist to abolish. The surface is rendered at 4x
    /// and halved twice on the way out, so the alpha has to survive that too:
    /// this draws through the same chain `PillWindow` uses.
    #[test]
    fn the_gaps_between_islands_are_not_holes() {
        const SS: u32 = 4;
        let mut hi = Pixmap::new(ENVELOPE_W * SS, ENVELOPE_H * SS).unwrap();
        draw(
            &mut hi,
            SS as f32,
            &Geom::of(PillMode::Expanded),
            &FLAT,
            &NO_SLOTS,
        );
        // 4x → 2x → 1x, exactly as `blit_and_present` does it.
        let paint = tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        };
        let half = Transform::from_scale(0.5, 0.5);
        let mut mid = Pixmap::new(ENVELOPE_W * 2, ENVELOPE_H * 2).unwrap();
        mid.draw_pixmap(0, 0, hi.as_ref(), &paint, half, None);
        let mut out = Pixmap::new(ENVELOPE_W, ENVELOPE_H).unwrap();
        out.draw_pixmap(0, 0, mid.as_ref(), &paint, half, None);

        let cy = ENVELOPE_H / 2;
        // The middle of each gap: between Copy and Dictate, and between
        // Dictate and Settings.
        for (i, b) in BUTTONS.iter().enumerate().take(BUTTONS.len() - 1) {
            let mid_x = (island_centre(i) + b.w / 2.0 + crate::pill::core::BAR_GAP / 2.0)
                + ENVELOPE_W as f32 / 2.0;
            let p = out.pixel(mid_x.round() as u32, cy).unwrap();
            assert!(p.alpha() > 0, "gap {i} is a hole: {p:?}");
            // And invisible: it must not read as a bridge between two islands.
            assert!(p.alpha() < 16, "gap {i} is visible: {p:?}");
        }
        // Past the last island, the end padding stays a hole.
        let outside = (ENVELOPE_W as f32 / 2.0 + crate::pill::core::bar_width() / 2.0 + 1.0) as u32;
        assert_eq!(out.pixel(outside, cy).unwrap().alpha(), 0);
    }

    /// Nothing but the nub is drawn when `buttons` is 0 — the bar has no
    /// presence at all in the modes that don't carry it.
    #[test]
    fn the_nub_draws_no_buttons() {
        let pm = frame(PillMode::Idle, &FLAT);
        assert_eq!(runs(&pm, ENVELOPE_H / 2).len(), 1);
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
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );

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
