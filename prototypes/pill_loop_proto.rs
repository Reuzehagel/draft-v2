// PROTOTYPE — THROWAWAY. Not production code, not wired into `draft`.
//
// Answers wayfinder ticket "The expanded pill's whole interaction loop" (#33).
// This is the first harness that puts the *entire* mouse path on screen at
// once — idle nub -> hover -> button bar -> click Dictate -> cancel or confirm
// -> back to idle — with a real cursor driving it, real `UpdateLayeredWindow`
// frames at 60 Hz, real Lucide geometry and real text.
//
// It is deliberately NOT scriptable-only. Rounds 1-3 of #18 could be judged
// from replays because nothing in them was mouse-driven; everything new here
// is. Use the scripts to compare two settings back to back, then put the mouse
// on it, because hover chatter and "did that click do anything" only exist
// live.
//
// Run:  cargo run --bin pill-loop-proto
//
// Terminal keys (type the letter, press enter):
//   d   Dictate button size     UNIFORM-22 / DOMINANT-28 / DOMINANT-30-BRIGHT   (Q1)
//   t   Expanded -> Recording   MORPH / SWAP                                    (Q2)
//   s   render scale            NATIVE / 1.00 / 1.25 / 1.50 / 2.00              (Q3)
//   i   icon proof strip        every glyph at all four scales, side by side    (Q3)
//   v   hover indicator         FADE / SLIDE / DWELL                            (Q4)
//   l   label motion            CROSSFADE / SLIDE / BLANK                       (Q5)
//   n   label over end padding  on / off                                        (Q5)
//   c   cancel treatment        SILENT / COLLAPSE / NEUTRAL-FLASH               (Q6)
//   y   "Copied" duration       700 / 1000 / 1600 ms                            (Q7)
//   e   history empty           toggles Copy's disabled look
//   f   flip the terminal flash between ok and failed
//   1   replay  Idle -> Expanded -> Idle
//   2   replay  a whole CLICK session, confirmed
//   3   replay  a whole CLICK session, cancelled
//   4   replay  a whole HOTKEY session (bare bars, no buttons)
//   5   replay  hover -> copy click -> "Copied" -> idle
//   0   park in Expanded and hold it (hover polling off) — for staring at pixels
//   p   dump the current frame to pill-frame.png
//   r   reset to Idle
//   q   quit
//
// Mouse (this is the point):
//   hover the nub          -> it expands
//   hover along the bar    -> the indicator and the label follow the slab
//   click Dictate          -> a click-started Recording, [x] ~~~~~ [check]
//   click x / check        -> cancel / finish
//   click Copy             -> "Copied" in the label
//   click Settings         -> prints a line (it does not launch anything here)
//
// Judge it over a BLACK desktop and a WHITE one. #18's hairline exists because
// the near-black body vanishes on black; 22px stroked icons at alpha have the
// same exposure and that is question 3.
//
// It copies (rather than imports) the layered-window plumbing from
// `src/pill/window.rs`, because the crate has no lib target. Delete this file
// once the decision is recorded.

#![cfg(windows)]
#![allow(clippy::too_many_arguments)]

use anyhow::{anyhow, Result};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

// The window never resizes. It is wide enough for the icon proof strip and
// tall enough for the label surface #29 put above the pill — which is the
// number that reopens #21's 104x42 envelope, so it is worth watching what this
// actually needs.
const BOX_W: u32 = 360;
const BOX_H: u32 = 170;
/// #27: measured from `rcWork`, not the full monitor rect.
const BOTTOM_MARGIN: u32 = 24;
const SUPERSAMPLE: u32 = 4;
const BAR_COUNT: usize = 7;

// Settled upstream, held fixed here.
const IDLE_W: f32 = 36.0;
const IDLE_H: f32 = 10.0;
const IDLE_R: f32 = 5.0;
const IDLE_FILL_A: f32 = 140.0; // #27
const REC_W: f32 = 62.0;
const REC_H: f32 = 28.0;
const REC_R: f32 = 14.0;

const BODY: (f32, f32, f32) = (13.0, 13.0, 13.0);
const HAIRLINE: (f32, f32, f32) = (220.0, 224.0, 232.0); // #18 CLEAR
const HAIRLINE_A: f32 = 120.0;
const BORDER_PROCESSING: (f32, f32, f32) = (190.0, 192.0, 200.0);
const BORDER_SUCCESS: (f32, f32, f32) = (74.0, 188.0, 120.0);
const BORDER_ERROR: (f32, f32, f32) = (214.0, 96.0, 96.0);
const BORDER_NEUTRAL: (f32, f32, f32) = (150.0, 152.0, 160.0);

// #29's arithmetic. `BTN_D` is the *flanker* diameter; the centre is an axis.
const BTN_D: f32 = 22.0;
/// Cancel and Confirm, on the click-started recording pill. Deliberately
/// smaller than any hover-bar button: they are a stop and an abort on a pill
/// that is already busy, not a menu of things to pick from.
const REC_BTN_D: f32 = 20.0;
/// The recording pill spends its width differently from the hover bar: less
/// inert padding at the ends, more gap in the middle, so Cancel and Confirm
/// sit out near the pill's ends rather than crowding the waveform. The pill's
/// overall width is unchanged — 4px moves from each end into each gap.
/// How far the UNIFIED hover indicator sits inside the pill's top and bottom
/// edges. Non-zero so it reads as a shape within a shape rather than a band
/// slicing the pill into thirds.
const INDICATOR_INSET: f32 = 3.0;
const REC_END_PAD: f32 = 7.0;
const REC_GAP: f32 = 12.0;
const BTN_GAP: f32 = 8.0;
const BTN_PAD: f32 = 11.0;
const EXP_H: f32 = 32.0;
const EXP_R: f32 = 16.0;
/// The bar cluster, treated as the centre "button" of the click-recording
/// layout so #18's width arithmetic still produces the number. ASSUMPTION —
/// see the note at the bottom of the ticket comment.
const BARS_W: f32 = 34.0;

const LABEL_PX: f32 = 11.5;
const LABEL_PAD_X: f32 = 8.0;
const LABEL_PAD_Y: f32 = 4.5;
const LABEL_GAP: f32 = 8.0; // vertical gap between label box and pill top
const LABEL_R: f32 = 6.0;

// ---------------------------------------------------------------------------
// Icons — real Lucide geometry
// ---------------------------------------------------------------------------

/// The Lucide sources, verbatim from `lucide-icons/lucide@main/icons/*.svg`,
/// on the 24x24 grid, stroke-2, round caps and joins.
///
/// FINDING FOR #29, and it is not cosmetic: **Lucide icons are not all
/// `<path>`**. `mic` carries a `<rect rx="3">` and `copy` a `<rect rx="2">`;
/// `sliders-horizontal`, `x` and `check` are pure paths. #29 said "embedded as
/// SVG path `d` strings" — which means either a hand conversion of the rects
/// (done here, marked below) or a shape-to-path step at embed time. Anything
/// that pastes the raw file contents will silently drop the rect and render a
/// mic with no capsule.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Icon {
    Copy,
    Mic,
    Sliders,
    X,
    Check,
}

impl Icon {
    fn name(self) -> &'static str {
        match self {
            Icon::Copy => "copy",
            Icon::Mic => "mic",
            Icon::Sliders => "sliders-horizontal",
            Icon::X => "x",
            Icon::Check => "check",
        }
    }

    /// One `d` string per icon — subpaths concatenated, which `svgtypes`
    /// handles because a `d` may contain many `M`s.
    fn d(self) -> &'static str {
        match self {
            // rect x=9 y=2 w=6 h=13 rx=3 -> hand-converted capsule (rx == w/2)
            Icon::Mic => {
                "M12 19v3 \
                 M19 10v2a7 7 0 0 1-14 0v-2 \
                 M9 5a3 3 0 0 1 6 0v7a3 3 0 0 1-6 0z"
            }
            // rect x=8 y=8 w=14 h=14 rx=2 -> hand-converted rounded rect
            Icon::Copy => {
                "M10 8h10a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2V10a2 2 0 0 1 2-2z \
                 M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"
            }
            Icon::Sliders => {
                "M10 5H3 M12 19H3 M14 3v4 M16 17v4 M21 12h-9 M21 19h-5 M21 5h-7 M8 10v4 M8 12H3"
            }
            Icon::X => "M18 6 6 18 M6 6l12 12",
            Icon::Check => "M20 6 9 17l-5-5",
        }
    }
}

/// Parsed once per icon on the 24-grid; each frame transforms a clone into
/// place. #29 said "cached per (icon, scale)" — caching the unit path and
/// transforming is equivalent, cheaper, and one dimension less to key on.
struct IconCache {
    paths: HashMap<Icon, Path>,
}

impl IconCache {
    fn new() -> Self {
        let mut paths = HashMap::new();
        for icon in [Icon::Copy, Icon::Mic, Icon::Sliders, Icon::X, Icon::Check] {
            match parse_d(icon.d()) {
                Some(p) => {
                    paths.insert(icon, p);
                }
                None => eprintln!("icon {} failed to parse", icon.name()),
            }
        }
        Self { paths }
    }

    fn get(&self, icon: Icon) -> Option<&Path> {
        self.paths.get(&icon)
    }
}

/// `svgtypes::PathParser` -> `tiny_skia::Path`. Arcs are the only awkward
/// segment: tiny-skia's `PathBuilder` has no arc primitive, so they are
/// converted to cubics here. Two of our five icons need it, so this ~50 lines
/// is not optional for the Lucide route.
fn parse_d(d: &str) -> Option<Path> {
    use svgtypes::{PathParser, PathSegment};
    let mut pb = PathBuilder::new();
    let (mut cx, mut cy) = (0.0f64, 0.0f64);
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    // Last cubic control point, for the smooth variants.
    let (mut px, mut py) = (0.0f64, 0.0f64);
    let mut have_prev_cubic = false;

    for seg in PathParser::from(d) {
        let seg = seg.ok()?;
        match seg {
            PathSegment::MoveTo { abs, x, y } => {
                let (x, y) = if abs { (x, y) } else { (cx + x, cy + y) };
                pb.move_to(x as f32, y as f32);
                cx = x;
                cy = y;
                sx = x;
                sy = y;
                have_prev_cubic = false;
            }
            PathSegment::LineTo { abs, x, y } => {
                let (x, y) = if abs { (x, y) } else { (cx + x, cy + y) };
                pb.line_to(x as f32, y as f32);
                cx = x;
                cy = y;
                have_prev_cubic = false;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                let x = if abs { x } else { cx + x };
                pb.line_to(x as f32, cy as f32);
                cx = x;
                have_prev_cubic = false;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                let y = if abs { y } else { cy + y };
                pb.line_to(cx as f32, y as f32);
                cy = y;
                have_prev_cubic = false;
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let (ox, oy) = if abs { (0.0, 0.0) } else { (cx, cy) };
                let (x1, y1, x2, y2, x, y) =
                    (ox + x1, oy + y1, ox + x2, oy + y2, ox + x, oy + y);
                pb.cubic_to(
                    x1 as f32, y1 as f32, x2 as f32, y2 as f32, x as f32, y as f32,
                );
                px = x2;
                py = y2;
                cx = x;
                cy = y;
                have_prev_cubic = true;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let (ox, oy) = if abs { (0.0, 0.0) } else { (cx, cy) };
                let (x2, y2, x, y) = (ox + x2, oy + y2, ox + x, oy + y);
                let (x1, y1) = if have_prev_cubic {
                    (2.0 * cx - px, 2.0 * cy - py)
                } else {
                    (cx, cy)
                };
                pb.cubic_to(
                    x1 as f32, y1 as f32, x2 as f32, y2 as f32, x as f32, y as f32,
                );
                px = x2;
                py = y2;
                cx = x;
                cy = y;
                have_prev_cubic = true;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let (ox, oy) = if abs { (0.0, 0.0) } else { (cx, cy) };
                let (x1, y1, x, y) = (ox + x1, oy + y1, ox + x, oy + y);
                pb.quad_to(x1 as f32, y1 as f32, x as f32, y as f32);
                cx = x;
                cy = y;
                have_prev_cubic = false;
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                let (ox, oy) = if abs { (0.0, 0.0) } else { (cx, cy) };
                let (x, y) = (ox + x, oy + y);
                pb.line_to(x as f32, y as f32); // no Lucide icon needs the real form
                cx = x;
                cy = y;
                have_prev_cubic = false;
            }
            PathSegment::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x,
                y,
            } => {
                let (x, y) = if abs { (x, y) } else { (cx + x, cy + y) };
                arc_to_cubics(
                    &mut pb,
                    cx,
                    cy,
                    rx,
                    ry,
                    x_axis_rotation,
                    large_arc,
                    sweep,
                    x,
                    y,
                );
                cx = x;
                cy = y;
                have_prev_cubic = false;
            }
            PathSegment::ClosePath { .. } => {
                pb.close();
                cx = sx;
                cy = sy;
                have_prev_cubic = false;
            }
        }
    }
    pb.finish()
}

/// SVG endpoint-parameterised arc -> a run of cubic Béziers. Textbook
/// implementation (F.6.5 in the SVG spec) plus the usual radius correction.
fn arc_to_cubics(
    pb: &mut PathBuilder,
    x0: f64,
    y0: f64,
    rx: f64,
    ry: f64,
    rot_deg: f64,
    large: bool,
    sweep: bool,
    x1: f64,
    y1: f64,
) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-9 || ry < 1e-9 || (x0 - x1).abs() < 1e-12 && (y0 - y1).abs() < 1e-12 {
        pb.line_to(x1 as f32, y1 as f32);
        return;
    }
    let phi = rot_deg.to_radians();
    let (cos_p, sin_p) = (phi.cos(), phi.sin());

    let dx2 = (x0 - x1) / 2.0;
    let dy2 = (y0 - y1) / 2.0;
    let x1p = cos_p * dx2 + sin_p * dy2;
    let y1p = -sin_p * dx2 + cos_p * dy2;

    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }

    let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = (num / den).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;

    let cx = cos_p * cxp - sin_p * cyp + (x0 + x1) / 2.0;
    let cy = sin_p * cxp + cos_p * cyp + (y0 + y1) / 2.0;

    let ang = |ux: f64, uy: f64, vx: f64, vy: f64| -> f64 {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let theta1 = ang(1.0, 0.0, ux, uy);
    let mut dtheta = ang(ux, uy, vx, vy);
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }

    let segs = (dtheta.abs() / (std::f64::consts::FRAC_PI_2)).ceil().max(1.0) as usize;
    let delta = dtheta / segs as f64;
    let t = 4.0 / 3.0 * (delta / 4.0).tan();

    let mut th = theta1;
    for _ in 0..segs {
        let (c1, s1) = (th.cos(), th.sin());
        let th2 = th + delta;
        let (c2, s2) = (th2.cos(), th2.sin());

        let p1 = (rx * c1, ry * s1);
        let p2 = (rx * c2, ry * s2);
        let d1 = (-rx * s1 * t, ry * c1 * t);
        let d2 = (rx * s2 * t, -ry * c2 * t);

        let map = |p: (f64, f64)| (cos_p * p.0 - sin_p * p.1 + cx, sin_p * p.0 + cos_p * p.1 + cy);
        let mapv = |p: (f64, f64)| (cos_p * p.0 - sin_p * p.1, sin_p * p.0 + cos_p * p.1);

        let a = map(p1);
        let b = map(p2);
        let da = mapv(d1);
        let db = mapv(d2);
        pb.cubic_to(
            (a.0 + da.0) as f32,
            (a.1 + da.1) as f32,
            (b.0 + db.0) as f32,
            (b.1 + db.1) as f32,
            b.0 as f32,
            b.1 as f32,
        );
        th = th2;
    }
}

// ---------------------------------------------------------------------------
// Text — the label surface needs a rasteriser the shipped pill does not have
// ---------------------------------------------------------------------------

/// FINDING FOR #29: the label surface is the first thing in the pill that is
/// **text**, and `UpdateLayeredWindow` with per-pixel alpha rules out both
/// routes you would reach for first — GDI `DrawTextW` writes zero alpha into
/// the DIB (the text comes out invisible under `ULW_ALPHA`), and a prerendered
/// bitmap is fixed-size. So the pill needs a glyph rasteriser producing a
/// coverage mask.
///
/// This uses `fontdue` (pure Rust, no font database, ~no transitive deps) over
/// a face loaded from `C:\Windows\Fonts` at runtime — so the binary carries no
/// font bytes, which matters given the release-size want.
struct TextRenderer {
    font: Option<fontdue::Font>,
    which: String,
}

impl TextRenderer {
    fn load() -> Self {
        const CANDIDATES: &[&str] = &[
            r"C:\Windows\Fonts\segoeui.ttf",
            r"C:\Windows\Fonts\SegoeUI.ttf",
            r"C:\Windows\Fonts\arial.ttf",
            r"C:\Windows\Fonts\tahoma.ttf",
        ];
        for path in CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            if let Ok(font) = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                return Self {
                    font: Some(font),
                    which: (*path).to_string(),
                };
            }
        }
        eprintln!("no system font found — the label will render as an empty box");
        Self {
            font: None,
            which: "<none>".into(),
        }
    }

    /// Where the string's *ink* actually starts and ends, relative to the pen
    /// origin. Advance widths include side bearings, so centring on them
    /// leaves the visible text off-centre — by different amounts per string,
    /// which is what makes it look sloppy rather than merely shifted.
    fn ink_span(&self, text: &str, px: f32) -> (f32, f32) {
        let Some(f) = &self.font else { return (0.0, 0.0) };
        let (mut pen, mut lo, mut hi) = (0.0f32, f32::MAX, f32::MIN);
        for ch in text.chars() {
            let m = f.metrics(ch, px);
            if m.width > 0 {
                lo = lo.min(pen + m.xmin as f32);
                hi = hi.max(pen + m.xmin as f32 + m.width as f32);
            }
            pen += m.advance_width;
        }
        if lo > hi {
            (0.0, pen)
        } else {
            (lo, hi)
        }
    }

    /// Height of a capital, for optical vertical centring. Centring the cap
    /// band rather than the ink keeps "Dictate" and "Copy last transcript" on
    /// the same line despite one having descenders.
    fn cap_height(&self, px: f32) -> f32 {
        self.font
            .as_ref()
            .map(|f| f.metrics('H', px).height as f32)
            .unwrap_or(px * 0.7)
    }

    /// Draw `text` with its left edge at `x` and its baseline at `baseline`,
    /// compositing a coverage mask into the pixmap. All device px.
    fn draw(&self, pm: &mut Pixmap, text: &str, x: f32, baseline: f32, px: f32, alpha: f32) {
        let Some(f) = &self.font else { return };
        if alpha <= 0.004 {
            return;
        }
        let mut pen = x;
        for ch in text.chars() {
            let (m, bitmap) = f.rasterize(ch, px);
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline - (m.height as f32 + m.ymin as f32)).round() as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    let cov = bitmap[row * m.width + col] as f32 / 255.0;
                    if cov > 0.002 {
                        blend_px(
                            pm,
                            gx + col as i32,
                            gy + row as i32,
                            (255.0, 255.0, 255.0),
                            cov * alpha,
                        );
                    }
                }
            }
            pen += m.advance_width;
        }
    }
}

/// Straight source-over of a single premultiplied pixel.
fn blend_px(pm: &mut Pixmap, x: i32, y: i32, rgb: (f32, f32, f32), a: f32) {
    if x < 0 || y < 0 || x >= pm.width() as i32 || y >= pm.height() as i32 {
        return;
    }
    let a = a.clamp(0.0, 1.0);
    let idx = y as usize * pm.width() as usize + x as usize;
    let pixels = pm.pixels_mut();
    let dst = pixels[idx];
    let (dr, dg, db, da) = (
        dst.red() as f32 / 255.0,
        dst.green() as f32 / 255.0,
        dst.blue() as f32 / 255.0,
        dst.alpha() as f32 / 255.0,
    );
    // Source is premultiplied by `a`; both sides already premultiplied.
    let sr = rgb.0 / 255.0 * a;
    let sg = rgb.1 / 255.0 * a;
    let sb = rgb.2 / 255.0 * a;
    let out_a = a + da * (1.0 - a);
    let out_r = sr + dr * (1.0 - a);
    let out_g = sg + dg * (1.0 - a);
    let out_b = sb + db * (1.0 - a);
    if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(
        (out_r * 255.0).round().clamp(0.0, 255.0) as u8,
        (out_g * 255.0).round().clamp(0.0, 255.0) as u8,
        (out_b * 255.0).round().clamp(0.0, 255.0) as u8,
        (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
    ) {
        pixels[idx] = p;
    }
}

// ---------------------------------------------------------------------------
// The axes being swept
// ---------------------------------------------------------------------------

/// Q1 — does Dictate want to be bigger than its flankers?
///
/// Judged 2026-08-11, and neither candidate on the old axis was the answer.
/// Both were about *scale* — one diameter for all three, or a bigger centre
/// circle. What came back was about **proportion**: Dictate gets **wider**,
/// and Copy and Settings step **down**. So the hierarchy runs in width and
/// height at once, and the centre never becomes a bigger circle.
///
/// That forces a split this model did not have: **the glyph box is separate
/// from the button.** Widening Dictate must not widen the mic. Before this,
/// one number did both jobs, which is exactly how a 34-wide Dictate would
/// have dragged a 34-wide microphone along with it.
struct Proportions {
    name: &'static str,
    /// The centre button — under ISLANDS this is the island itself.
    centre_w: f32,
    /// The 24-unit icon grid's box inside the centre button.
    centre_glyph: f32,
    flank_w: f32,
    flank_glyph: f32,
    /// Alpha of the centre button's own disc, before hover. Only meaningful
    /// under UNIFIED — under ISLANDS the island *is* the indicator.
    ring_a: f32,
    note: &'static str,
}

/// Reopened 2026-08-11 — the axis is live again and nothing on it is marked
/// chosen. UNIFORM leads because it is the default to think from, not because
/// it won. The glyph-box split below stays either way: it is what makes any
/// non-uniform option expressible at all.
const PROPORTIONS: &[Proportions] = &[
    Proportions {
        name: "WIDE DICTATE 48",
        centre_w: 48.0,
        centre_glyph: 22.0,
        flank_w: 32.0,
        flank_glyph: 22.0,
        ring_a: 0.0,
        note: "flankers stay uniform circles; Dictate alone stretches into a long stadium",
    },
    Proportions {
        name: "UNIFORM 32",
        centre_w: 32.0,
        centre_glyph: 22.0,
        flank_w: 32.0,
        flank_glyph: 22.0,
        ring_a: 0.0,
        note: "#18 as settled — three equal buttons, nothing marked as primary",
    },
    Proportions {
        name: "HIERARCHY 34/26",
        centre_w: 34.0,
        centre_glyph: 22.0,
        flank_w: 26.0,
        flank_glyph: 20.0,
        ring_a: 0.0,
        note: "Dictate is a 34x32 stadium; the flankers step down to 26px circles",
    },
    Proportions {
        name: "DOMINANT CIRCLE 38/32",
        centre_w: 38.0,
        centre_glyph: 26.0,
        flank_w: 32.0,
        flank_glyph: 22.0,
        ring_a: 34.0,
        note: "the centre grows as a circle instead of a stadium — the old Wispr read",
    },
];

/// NEW AXIS, and a bigger one than it looks: is the button bar **one body**,
/// or **three islands** with real desktop between them?
///
/// #29 assumed one body without ever asking — its slab argument ("circles
/// would make the gaps dead zones where hover flickers off") is an argument
/// *for* one body, and islands turn it inside out: with a visible gap, losing
/// hover in the gap is honest rather than a flicker. So this axis reopens Q4
/// rather than sitting beside it.
///
/// The pleasant accident: at `pad = 5` a 22px button becomes a **32px island**,
/// which at height 32 is exactly a circle. The bars island stays a stadium.
struct BodyStyle {
    name: &'static str,
    islands: bool,
    /// Padding around each button inside its own island.
    pad: f32,
    /// Gap of bare desktop between islands.
    gap: f32,
    note: &'static str,
}

/// Judged 2026-08-11: **two states, not three.** ISLANDS-AIRY is gone — the
/// choice is islands or one body, and it is a toggle rather than a radio
/// because both states are complete and neither can be put into a bad
/// configuration. Islands is the default; the user said plainly they prefer it.
/// `pad` is 0 because the proportions above now give the island *directly* —
/// the number in the spec is the number on screen. It was 5 when the axis
/// described a bare button that then grew padding, which meant the spec said
/// 22 and the screen showed 32.
const ISLAND_BODY: BodyStyle = BodyStyle {
    name: "ISLANDS",
    islands: true,
    pad: 0.0,
    gap: 3.0,
    note: "each button is its own island, with bare desktop showing between them",
};

/// Named rather than indexed, because the click-started recording pill is
/// always this one regardless of what the toggle says.
const UNIFIED_BODY: BodyStyle = BodyStyle {
    name: "UNIFIED",
    islands: false,
    pad: 0.0,
    gap: 0.0,
    note: "#29 as settled — one pill body, hover slabs so the gaps are live",
};

/// Q3, second half. #29 said "22px Lucide icons", which quietly conflates two
/// numbers: the *button* is 22px, and the icon's 24-unit grid has to be mapped
/// onto something. Mapping the grid 1:1 onto the button makes the glyph fill
/// ~82% of it — legible, but with no ring of air, so the hover disc is exactly
/// the glyph's size. Shrinking the glyph buys air and costs stroke weight
/// (stroke is `2 x grid_px / 24`), which is precisely the muddiness this
/// question is about. It only became visible once real Lucide geometry was on
/// screen, so it is an axis rather than a constant.
struct GlyphSize {
    name: &'static str,
    /// Fraction of the button diameter the 24-grid is mapped onto.
    frac: f32,
    note: &'static str,
}

/// Judged 2026-08-11: **AIR-0.72 wins and the axis is closed.** The other two
/// were called bad outright, so they stay only as the reference the choice was
/// made against — first in the list is the default, and that is now 0.72.
const GLYPH_SIZES: &[GlyphSize] = &[
    GlyphSize {
        name: "AIR-0.72  (CHOSEN)",
        frac: 0.72,
        note: "conventional icon-button proportions; stroke 1.32px on a 22 button",
    },
    GlyphSize {
        name: "AIR-0.86  (rejected)",
        frac: 0.86,
        note: "a ring of air inside the hover disc; stroke drops to 1.57px",
    },
    GlyphSize {
        name: "FULL (grid = button)  (rejected)",
        frac: 1.0,
        note: "stroke 1.83px — heaviest, and the glyph touches the disc edge",
    },
];

/// Q2 — the hinge of the whole mouse path. Clicking Dictate has to turn
/// `[copy] [dictate] [settings]` into `[x] ~~~~~ [check]`.
#[derive(Clone, Copy, PartialEq)]
enum Handover {
    /// Flankers hold position and crossfade their glyphs; the centre blooms
    /// from mic into bars. Cheap, continuous, one tween.
    Morph,
    /// Flankers fold back into the centre and fade out, then a new pair folds
    /// out. Honest that these are different controls; costs a beat.
    Swap,
}

const HANDOVERS: &[(Handover, &str, u32, &str)] = &[
    (
        Handover::Morph,
        "MORPH",
        170,
        "glyphs crossfade in place — continuous, but copy *becoming* cancel may read as a lie",
    ),
    (
        Handover::Swap,
        "SWAP",
        220,
        "fold in, fold out — two 110ms halves; honest, but is the beat a stutter?",
    ),
];

/// Q4 — sliding the cursor along the bar changes the lit button three times in
/// ~100px. With a fade on each, a fast sweep may smear.
#[derive(Clone, Copy, PartialEq)]
enum Indicator {
    /// One tween per button: the old fades out as the new fades in.
    Fade,
    /// A single disc that *travels* between slabs.
    Slide,
    /// Fade, but a new slab has to be held before it lights.
    Dwell,
}

const INDICATORS: &[(Indicator, &str, &str)] = &[
    (
        Indicator::Fade,
        "FADE",
        "#29 as written — 90ms out-cubic per button. The one that might smear.",
    ),
    (
        Indicator::Slide,
        "SLIDE",
        "one disc that moves and resizes between slabs — no crossfade to smear",
    ),
    (
        Indicator::Dwell,
        "DWELL",
        "FADE plus a 70ms hold before switching — kills chatter, adds lag",
    ),
];

/// Q5 — what the label does as the cursor crosses between buttons.
#[derive(Clone, Copy, PartialEq)]
enum LabelMotion {
    /// Box stays centred over the pill; text crossfades, box width lerps.
    Crossfade,
    /// Box travels to sit over the hovered slab while the text crossfades.
    Slide,
    /// Old text goes immediately; new arrives after a gap.
    Blank,
}

const LABEL_MOTIONS: &[(LabelMotion, &str, u32, &str)] = &[
    (
        LabelMotion::Crossfade,
        "CROSSFADE",
        110,
        "centred over the pill, text dissolves — calm, but the box breathes as widths change",
    ),
    (
        LabelMotion::Slide,
        "SLIDE",
        140,
        "the label tracks the slab — points at what it names, more motion to notice",
    ),
    (
        LabelMotion::Blank,
        "BLANK",
        160,
        "hard cut to nothing, then in — the least smear, the most flicker",
    ),
];

/// Q6 — `x` returns straight to Idle with no flash, by design. But "the pill
/// just goes away" is also what a crash looks like.
#[derive(Clone, Copy, PartialEq)]
enum Cancel {
    /// #29 as written: collapse on the normal hover-out motion, nothing else.
    Silent,
    /// Same, but slower and deliberate, so the departure is legible as a move.
    Collapse,
    /// A brief neutral (grey, not red) flash. Reports "stopped", not "failed".
    NeutralFlash,
}

const CANCELS: &[(Cancel, &str, u32, &str)] = &[
    (
        Cancel::Silent,
        "SILENT",
        110,
        "#29 as written — straight back to the nub on the collapse motion",
    ),
    (
        Cancel::Collapse,
        "COLLAPSE",
        220,
        "same, deliberately slower — does legibility need only time?",
    ),
    (
        Cancel::NeutralFlash,
        "NEUTRAL-FLASH",
        200,
        "a grey flash first — amends #29's no-flash rule if the silence reads as a bug",
    ),
];

const COPY_MS: &[u32] = &[1000, 700, 1600];

/// Q3 — the icons are stroke-2 on a 24 grid, scaled to 22 and composited with
/// per-pixel alpha. This forces the rasterisation scale so all four DPIs can
/// be seen on one machine. NOTE: it changes the pill's *physical* size on
/// screen as a side effect — what is being judged is pixels-per-logical-unit,
/// not how big the pill looks.
const SCALES: &[(&str, Option<f32>)] = &[
    ("NATIVE", None),
    ("1.00 (100% DPI)", Some(1.0)),
    ("1.25 (125% DPI)", Some(1.25)),
    ("1.50 (150% DPI)", Some(1.5)),
    ("2.00 (200% DPI)", Some(2.0)),
];

// ---------------------------------------------------------------------------
// Modes and geometry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Idle,
    Expanded,
    /// #29: `Recording { origin: Hotkey }` — bare 62x28 bars, no buttons, and
    /// click-through stays *on*.
    RecHotkey,
    /// #29: `Recording { origin: Click }` — `[x] ~~~~~ [check]`.
    RecClick,
    Processing,
    Done,
    /// Only reachable under the NEUTRAL-FLASH cancel treatment.
    Cancelled,
}

impl Mode {
    /// #29's rule, verbatim: click-through is off only while the pill is
    /// showing buttons.
    fn shows_buttons(self) -> bool {
        matches!(self, Mode::Expanded | Mode::RecClick)
    }
}

#[derive(Clone, Copy)]
struct Geom {
    w: f32,
    h: f32,
    radius: f32,
    fill_rgb: (f32, f32, f32),
    fill_a: f32,
    border_rgb: (f32, f32, f32),
    border_a: f32,
    border_w: f32,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (lerp(a.0, b.0, t), lerp(a.1, b.1, t), lerp(a.2, b.2, t))
}

impl Geom {
    fn lerp(self, to: Geom, t: f32) -> Geom {
        Geom {
            w: lerp(self.w, to.w, t),
            h: lerp(self.h, to.h, t),
            radius: lerp(self.radius, to.radius, t),
            fill_rgb: lerp3(self.fill_rgb, to.fill_rgb, t),
            fill_a: lerp(self.fill_a, to.fill_a, t),
            border_rgb: lerp3(self.border_rgb, to.border_rgb, t),
            border_a: lerp(self.border_a, to.border_a, t),
            border_w: lerp(self.border_w, to.border_w, t),
        }
    }
}

fn out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// A three-slot layout: two flankers around a centre, auto-sized by #18's
/// arithmetic. Both Expanded and click-Recording are instances of it, which is
/// exactly why MORPH is even possible.
#[derive(Clone, Copy)]
struct Layout {
    centre_w: f32,
    flank_w: f32,
    /// The icon grid's box, kept apart from the button width so a wide
    /// Dictate does not drag a wide microphone along with it.
    centre_glyph: f32,
    flank_glyph: f32,
    /// Inert padding at each end, and the gap between the centre and a
    /// flanker. UNIFIED only — under ISLANDS the spacing is `style.gap`.
    /// Per-layout rather than global, because the recording pill wants its
    /// two controls further out than the hover bar wants its three.
    end_pad: f32,
    gap: f32,
    style: &'static BodyStyle,
}

impl Layout {
    fn width(&self) -> f32 {
        if self.style.islands {
            (0..3).map(|i| self.island_w(i)).sum::<f32>() + 2.0 * self.style.gap
        } else {
            2.0 * self.end_pad + self.centre_w + 2.0 * self.flank_w + 2.0 * self.gap
        }
    }
    /// The island a button sits in. Under UNIFIED there are no islands, but
    /// the number is still the button plus its share of the padding, which is
    /// what the fold animates.
    fn island_w(&self, i: usize) -> f32 {
        self.slot_w(i) + 2.0 * self.style.pad
    }
    /// x offset of slot `i` (0,1,2) from the pill's centre.
    fn slot_dx(&self, i: usize) -> f32 {
        let side = if self.style.islands {
            self.island_w(1) / 2.0 + self.style.gap + self.island_w(i) / 2.0
        } else {
            self.centre_w / 2.0 + self.gap + self.flank_w / 2.0
        };
        match i {
            0 => -side,
            2 => side,
            _ => 0.0,
        }
    }
    fn slot_w(&self, i: usize) -> f32 {
        if i == 1 {
            self.centre_w
        } else {
            self.flank_w
        }
    }
    fn slot_glyph(&self, i: usize) -> f32 {
        if i == 1 {
            self.centre_glyph
        } else {
            self.flank_glyph
        }
    }
    /// An island is as tall as it is wide, up to the pill's own height — so a
    /// 26px flanker is a circle and a 34px Dictate is a stadium. This is the
    /// half of the hierarchy that runs in height; without it a narrower
    /// flanker would be a *vertical* stadium, which reads as a mistake.
    fn island_h(&self, i: usize) -> f32 {
        self.island_w(i).min(EXP_H)
    }
    /// #29: slabs are full pill height, the button plus half the gap either
    /// side, and the 11px end padding is inert. Islands invert that — the
    /// hit region is the island, and the gap between islands really is dead,
    /// because you can see through it.
    fn slab(&self, i: usize) -> (f32, f32) {
        let dx = self.slot_dx(i);
        let w = if self.style.islands {
            self.island_w(i)
        } else {
            self.slot_w(i) + self.gap
        };
        (dx - w / 2.0, dx + w / 2.0)
    }
}

/// What is drawn inside the pill on this frame.
#[derive(Clone, Copy)]
enum Item {
    Icon(Icon),
    Bars,
}

#[derive(Clone, Copy)]
struct Drawn {
    item: Item,
    dx: f32,
    /// The button/island width. Drives the resting disc and the bar cluster.
    size: f32,
    /// The icon grid's box. Equals `size` for anything that is not an icon.
    glyph: f32,
    alpha: f32,
    /// Index into the *interactive* slot set, for the hover indicator.
    slot: Option<usize>,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct Anim {
    from: Geom,
    to: Geom,
    from_mode: Mode,
    start: Instant,
    dur: Duration,
}

struct App {
    win: Option<PillWindow>,
    icons: IconCache,
    text: TextRenderer,

    dictate: usize,
    islands: bool,
    glyph: usize,
    handover: usize,
    indicator: usize,
    label_motion: usize,
    cancel: usize,
    copy_ms: usize,
    scale_ix: usize,
    label_over_pad: bool,
    strip: bool,
    history_empty: bool,
    ok: bool,

    mode: Mode,
    mode_since: Instant,
    anim: Option<Anim>,
    queue: VecDeque<(Mode, u64)>,
    next_at: Option<Instant>,
    /// Script 5 fires a copy click at this instant, so the acknowledgement can
    /// be judged without hitting a 22px target by hand.
    queue_copy_at: Option<Instant>,
    wave_epoch: Instant,

    /// Whether the cursor is anywhere inside the pill at all.
    cursor_inside: bool,
    /// Which slab the cursor is over, and when it got there.
    hovered: Option<usize>,
    hover_since: Instant,
    /// The slab the *indicator* is currently showing (DWELL lags `hovered`).
    lit: Option<usize>,
    lit_prev: Option<usize>,
    lit_since: Instant,

    label_cur: Option<(String, f32)>,
    label_prev: Option<(String, f32)>,
    label_since: Instant,
    copied_until: Option<Instant>,

    click_transparent: bool,
    /// `0` parks a state so it can be stared at; hover polling is suspended.
    parked: bool,
    /// Dumps the next composed frame to a PNG.
    save_next: bool,
    /// First frame, for the headless capture hook.
    started: Option<Instant>,
    /// Where the last PNG went, echoed in the panel.
    saved_note: String,
    /// A one-line log of what the last click did, so the panel says what
    /// happened instead of the terminal.
    last_action: String,
}

fn main() -> eframe::Result<()> {
    let start = Instant::now();
    let app = App {
        win: None,
        icons: IconCache::new(),
        text: TextRenderer::load(),
        dictate: 0,
        islands: true,
        glyph: 0,
        handover: 0,
        indicator: 0,
        label_motion: 0,
        cancel: 0,
        copy_ms: 0,
        scale_ix: 0,
        strip: false,
        label_over_pad: false,
        history_empty: false,
        ok: true,
        mode: Mode::Idle,
        mode_since: start,
        anim: None,
        queue: VecDeque::new(),
        next_at: None,
        queue_copy_at: None,
        wave_epoch: start,
        cursor_inside: false,
        hovered: None,
        hover_since: start,
        lit: None,
        lit_prev: None,
        lit_since: start,
        label_cur: None,
        label_prev: None,
        label_since: start,
        copied_until: None,
        click_transparent: true,
        parked: false,
        save_next: false,
        started: None,
        saved_note: String::new(),
        last_action: "nothing yet — hover the pill at the bottom of the screen".into(),
    };

    // The control panel is an ordinary eframe window; the pill is a raw Win32
    // layered window created alongside it. eframe owns the message loop, and
    // because both live on the same thread the pill's wndproc still gets its
    // messages.
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([470.0, 820.0])
            .with_position([40.0, 40.0])
            .with_title("Draft pill — interaction loop prototype"),
        ..Default::default()
    };
    eframe::run_native(
        "draft-pill-loop-proto",
        opts,
        Box::new(|_cc| Ok(Box::new(app))),
    )
}

impl App {
    fn dictate(&self) -> &'static Proportions {
        &PROPORTIONS[self.dictate]
    }
    fn handover(&self) -> Handover {
        HANDOVERS[self.handover].0
    }
    fn handover_ms(&self) -> u32 {
        HANDOVERS[self.handover].2
    }
    fn indicator(&self) -> Indicator {
        INDICATORS[self.indicator].0
    }
    fn label_motion(&self) -> LabelMotion {
        LABEL_MOTIONS[self.label_motion].0
    }
    fn label_ms(&self) -> u32 {
        LABEL_MOTIONS[self.label_motion].2
    }
    fn cancel(&self) -> Cancel {
        CANCELS[self.cancel].0
    }

    fn body(&self) -> &'static BodyStyle {
        if self.islands {
            &ISLAND_BODY
        } else {
            &UNIFIED_BODY
        }
    }
    /// The two body styles do not want the same numbers, and pretending they
    /// do is what made UNIFIED 150px wide for three 22px glyphs.
    ///
    /// Under ISLANDS a slot width is a **shape you can see**, so Q1's numbers
    /// are used as given. Under UNIFIED nothing is drawn at the slot's edge —
    /// the width is only an invisible hover slab — so paying 48px for the
    /// centre buys air and no hierarchy. There the slot collapses onto the
    /// glyph box, which is #18's original arithmetic, and Dictate has to be
    /// marked some other way than by shape.
    fn expanded_layout(&self) -> Layout {
        let p = self.dictate();
        let islands = self.body().islands;
        Layout {
            centre_w: if islands { p.centre_w } else { p.centre_glyph },
            flank_w: if islands { p.flank_w } else { p.flank_glyph },
            centre_glyph: p.centre_glyph,
            flank_glyph: p.flank_glyph,
            end_pad: BTN_PAD,
            gap: BTN_GAP,
            style: self.body(),
        }
    }
    /// Always UNIFIED, whatever Q0 says. Islands are the *resident pill's
    /// hover bar* — a shelf of things you might pick. A session in progress is
    /// one object with a stop and a cancel on it, and breaking it into three
    /// floating pieces would say the recording is three things.
    fn recclick_layout(&self) -> Layout {
        Layout {
            centre_w: BARS_W,
            flank_w: REC_BTN_D,
            centre_glyph: BARS_W,
            flank_glyph: REC_BTN_D,
            end_pad: REC_END_PAD,
            gap: REC_GAP,
            style: &UNIFIED_BODY,
        }
    }

    /// 1 = drawn as islands, 0 = drawn as one body. Only Expanded is ever
    /// islands; Idle is a single nub either way, so it inherits whatever it is
    /// travelling to or from and the crossfade never fires on a hover.
    fn island_mix(&self, now: Instant) -> f32 {
        if !self.body().islands {
            return 0.0;
        }
        let islandy = |m: Mode| matches!(m, Mode::Expanded | Mode::Idle) as i32 as f32;
        let to = islandy(self.mode);
        match &self.anim {
            Some(a) if self.anim_t(now) < 1.0 => {
                lerp(islandy(a.from_mode), to, out_cubic(self.anim_t(now)))
            }
            _ => to,
        }
    }

    /// How far the button bar is unfolded, 0 (everything collapsed into the
    /// centre) to 1 (fully out). The island bodies need it as a scalar; the
    /// items each carry it as their own alpha and offset.
    fn fold_at(&self, now: Instant) -> f32 {
        let raw = self.anim_t(now);
        if self.anim.is_none() || raw >= 1.0 {
            return if self.mode.shows_buttons() { 1.0 } else { 0.0 };
        }
        let t = out_cubic(raw);
        let from = self.anim.as_ref().map(|a| a.from_mode);
        if from == Some(Mode::Expanded) && self.mode == Mode::RecClick {
            return match self.handover() {
                // Nothing folds — the glyphs swap in place.
                Handover::Morph => 1.0,
                Handover::Swap => {
                    if raw < 0.5 {
                        1.0 - out_cubic(raw * 2.0)
                    } else {
                        out_cubic((raw - 0.5) * 2.0)
                    }
                }
            };
        }
        match (
            from.map(|m| m.shows_buttons()).unwrap_or(false),
            self.mode.shows_buttons(),
        ) {
            (false, true) => t,
            (true, false) => 1.0 - t,
            (true, true) => 1.0,
            (false, false) => 0.0,
        }
    }
    fn layout_for(&self, mode: Mode) -> Option<Layout> {
        match mode {
            Mode::Expanded => Some(self.expanded_layout()),
            Mode::RecClick => Some(self.recclick_layout()),
            _ => None,
        }
    }

    fn geom_of(&self, mode: Mode) -> Geom {
        let base = Geom {
            w: IDLE_W,
            h: IDLE_H,
            radius: IDLE_R,
            fill_rgb: BODY,
            fill_a: IDLE_FILL_A,
            border_rgb: HAIRLINE,
            border_a: HAIRLINE_A,
            border_w: 1.0,
        };
        match mode {
            Mode::Idle => base,
            Mode::Expanded => Geom {
                w: self.expanded_layout().width(),
                h: EXP_H,
                radius: EXP_R,
                fill_a: 235.0,
                ..base
            },
            Mode::RecClick => Geom {
                w: self.recclick_layout().width(),
                h: EXP_H,
                radius: EXP_R,
                fill_a: 245.0,
                ..base
            },
            Mode::RecHotkey => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                ..base
            },
            Mode::Processing => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                border_rgb: BORDER_PROCESSING,
                border_a: 165.0,
                ..base
            },
            Mode::Cancelled => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                border_rgb: BORDER_NEUTRAL,
                border_a: 220.0,
                border_w: 2.0,
                ..base
            },
            Mode::Done => {
                let colour = if self.ok {
                    BORDER_SUCCESS
                } else {
                    BORDER_ERROR
                };
                Geom {
                    w: REC_W,
                    h: REC_H,
                    radius: REC_R,
                    fill_a: 245.0,
                    border_rgb: colour,
                    border_a: 235.0,
                    border_w: 2.5,
                    ..base
                }
            }
        }
    }

    /// Durations, from #18's SNAPPY plus the axes this ticket owns.
    fn dur_ms(&self, from: Mode, to: Mode) -> u32 {
        match (from, to) {
            (Mode::Idle, Mode::Expanded) => 110,
            (Mode::Expanded, Mode::Idle) => 90,
            (Mode::Expanded, Mode::RecClick) => self.handover_ms(),
            // #18: the 320ms handoff, and #29: both buttons leave on it.
            (_, Mode::Processing) => 320,
            (_, Mode::Cancelled) => 90,
            (Mode::Cancelled, Mode::Idle) => 110,
            (Mode::RecClick, Mode::Idle) => CANCELS[self.cancel].2,
            (_, Mode::RecClick) | (_, Mode::RecHotkey) => 110,
            (_, Mode::Done) => 0,
            (_, Mode::Idle) => 160,
            _ => 110,
        }
    }

    fn anim_t(&self, now: Instant) -> f32 {
        match &self.anim {
            None => 1.0,
            Some(a) => {
                if a.dur.is_zero() {
                    1.0
                } else {
                    (now.duration_since(a.start).as_secs_f32() / a.dur.as_secs_f32())
                        .clamp(0.0, 1.0)
                }
            }
        }
    }

    fn current_geom(&self, now: Instant) -> Geom {
        let base = match &self.anim {
            Some(a) => a.from.lerp(a.to, out_cubic(self.anim_t(now))),
            None => self.geom_of(self.mode),
        };
        if self.mode == Mode::Processing && self.anim_t(now) >= 1.0 {
            let e = now.duration_since(self.mode_since).as_secs_f32();
            let pulse = 0.5 - 0.5 * (e * std::f32::consts::TAU * 0.8).cos();
            return Geom {
                border_a: 110.0 + 110.0 * pulse,
                ..base
            };
        }
        base
    }

    /// SWAP's mid-point width: everything folded into the centre. Interpolating
    /// the pill through this is what makes the fold read as a fold rather than
    /// a resize.
    fn swap_pinch_w(&self) -> f32 {
        2.0 * BTN_PAD + self.dictate().centre_w.max(BARS_W)
    }

    /// The pill's width this frame — normally the geometry lerp, except under
    /// SWAP, which pinches through a narrower middle.
    fn current_width(&self, now: Instant, g: &Geom) -> f32 {
        let Some(a) = &self.anim else { return g.w };
        if !(a.from_mode == Mode::Expanded && self.mode == Mode::RecClick) {
            return g.w;
        }
        if self.handover() != Handover::Swap {
            return g.w;
        }
        let t = self.anim_t(now);
        let pinch = self.swap_pinch_w();
        if t < 0.5 {
            lerp(
                self.expanded_layout().width(),
                pinch,
                out_cubic((t * 2.0).clamp(0.0, 1.0)),
            )
        } else {
            lerp(
                pinch,
                self.recclick_layout().width(),
                out_cubic(((t - 0.5) * 2.0).clamp(0.0, 1.0)),
            )
        }
    }

    /// Everything drawn inside the pill this frame, as explicit items. This is
    /// the model change #29 flagged: per-button state does not fit inside a
    /// single interpolable `Geom`, so it lives here instead of being contorted
    /// into one.
    fn items(&self, now: Instant) -> Vec<Drawn> {
        let t = out_cubic(self.anim_t(now));
        let raw_t = self.anim_t(now);
        let from_mode = self.anim.as_ref().map(|a| a.from_mode);
        let to_mode = self.mode;

        let exp = self.expanded_layout();
        let rec = self.recclick_layout();

        // Fully settled states first.
        if self.anim.is_none() || raw_t >= 1.0 {
            return match to_mode {
                Mode::Expanded => self.fold(&exp, 1.0, [Icon::Copy, Icon::Mic, Icon::Sliders], true),
                Mode::RecClick => self.fold(&rec, 1.0, [Icon::X, Icon::Mic, Icon::Check], false),
                Mode::RecHotkey | Mode::Processing | Mode::Done | Mode::Cancelled => vec![Drawn {
                    item: Item::Bars,
                    dx: 0.0,
                    // BARS_W, not REC_W: the cluster sits *inside* the pill.
                    size: BARS_W,
                    glyph: BARS_W,
                    alpha: 1.0,
                    slot: None,
                }],
                Mode::Idle => vec![],
            };
        }

        match (from_mode, to_mode) {
            // The fold-out and the fold-back-in. #29: flankers slide out of the
            // Dictate button and fade in, on the pill's own 110ms.
            (Some(Mode::Idle), Mode::Expanded) => {
                self.fold(&exp, t, [Icon::Copy, Icon::Mic, Icon::Sliders], true)
            }
            (Some(Mode::Expanded), Mode::Idle) => {
                self.fold(&exp, 1.0 - t, [Icon::Copy, Icon::Mic, Icon::Sliders], true)
            }

            // Q2, the hinge.
            (Some(Mode::Expanded), Mode::RecClick) => match self.handover() {
                Handover::Morph => {
                    // Positions lerp between the two layouts; glyphs crossfade
                    // in place; the mic dissolves into the bars.
                    let mut out = Vec::new();
                    for (i, (a_icon, b_icon)) in
                        [(Icon::Copy, Icon::X), (Icon::Mic, Icon::Mic), (Icon::Sliders, Icon::Check)]
                            .into_iter()
                            .enumerate()
                    {
                        let dx = lerp(exp.slot_dx(i), rec.slot_dx(i), t);
                        // Both the button and the glyph box travel, because
                        // the two layouts no longer agree on either one.
                        let size = lerp(exp.slot_w(i), rec.slot_w(i), t);
                        let glyph = lerp(exp.slot_glyph(i), rec.slot_glyph(i), t);
                        if i == 1 {
                            out.push(Drawn {
                                item: Item::Icon(a_icon),
                                dx,
                                size,
                                glyph,
                                alpha: (1.0 - t * 2.0).max(0.0),
                                slot: None,
                            });
                            out.push(Drawn {
                                item: Item::Bars,
                                dx,
                                size,
                                glyph: size,
                                alpha: ((t - 0.4) / 0.6).clamp(0.0, 1.0),
                                slot: None,
                            });
                        } else {
                            out.push(Drawn {
                                item: Item::Icon(a_icon),
                                dx,
                                size,
                                glyph,
                                alpha: 1.0 - t,
                                slot: None,
                            });
                            out.push(Drawn {
                                item: Item::Icon(b_icon),
                                dx,
                                size,
                                glyph,
                                alpha: t,
                                slot: None,
                            });
                        }
                    }
                    out
                }
                Handover::Swap => {
                    if raw_t < 0.5 {
                        let k = 1.0 - out_cubic((raw_t * 2.0).clamp(0.0, 1.0));
                        self.fold(&exp, k, [Icon::Copy, Icon::Mic, Icon::Sliders], true)
                    } else {
                        let k = out_cubic(((raw_t - 0.5) * 2.0).clamp(0.0, 1.0));
                        self.fold(&rec, k, [Icon::X, Icon::Mic, Icon::Check], false)
                    }
                }
            },

            // #29: both buttons leave on the 320ms handoff, and the bars stay.
            (Some(Mode::RecClick), Mode::Processing) => {
                let mut out = self.fold(&rec, 1.0 - t, [Icon::X, Icon::Mic, Icon::Check], false);
                for d in &mut out {
                    if matches!(d.item, Item::Bars) {
                        d.alpha = 1.0;
                        d.dx = 0.0;
                    }
                }
                out
            }

            // Cancel: the buttons leave with the pill.
            (Some(Mode::RecClick), _) => {
                self.fold(&rec, 1.0 - t, [Icon::X, Icon::Mic, Icon::Check], false)
            }

            (_, Mode::RecHotkey) | (_, Mode::Processing) | (_, Mode::Done) | (_, Mode::Cancelled) => {
                vec![Drawn {
                    item: Item::Bars,
                    dx: 0.0,
                    // BARS_W, not REC_W: the cluster sits *inside* the pill.
                    size: BARS_W,
                    glyph: BARS_W,
                    alpha: t,
                    slot: None,
                }]
            }
            (_, Mode::Expanded) => self.fold(&exp, t, [Icon::Copy, Icon::Mic, Icon::Sliders], true),
            (_, Mode::RecClick) => self.fold(&rec, t, [Icon::X, Icon::Mic, Icon::Check], false),
            (_, Mode::Idle) => vec![],
        }
    }

    /// The fold-out at progress `k`: flankers travel from the centre to their
    /// slots and fade in together. `centre_is_mic` picks between the Dictate
    /// glyph and the live bars.
    fn fold(&self, l: &Layout, k: f32, icons: [Icon; 3], centre_is_mic: bool) -> Vec<Drawn> {
        let k = k.clamp(0.0, 1.0);
        let mut out = Vec::with_capacity(3);
        for i in 0..3 {
            let dx = lerp(0.0, l.slot_dx(i), k);
            if i == 1 {
                if centre_is_mic {
                    out.push(Drawn {
                        item: Item::Icon(icons[1]),
                        dx,
                        size: l.centre_w,
                        glyph: l.centre_glyph,
                        alpha: k,
                        slot: Some(1),
                    });
                } else {
                    out.push(Drawn {
                        item: Item::Bars,
                        dx,
                        size: l.centre_w,
                        glyph: l.centre_w,
                        alpha: k,
                        slot: None,
                    });
                }
            } else {
                out.push(Drawn {
                    item: Item::Icon(icons[i]),
                    dx,
                    size: l.flank_w,
                    glyph: l.flank_glyph,
                    alpha: k,
                    slot: Some(i),
                });
            }
        }
        out
    }

    // -- hover / labels -----------------------------------------------------

    fn label_for(&self, slot: usize) -> Option<&'static str> {
        match self.mode {
            Mode::Expanded => Some(match slot {
                0 => {
                    if self.history_empty {
                        "Nothing to copy"
                    } else {
                        "Copy last transcript"
                    }
                }
                1 => "Dictate",
                _ => "Settings",
            }),
            Mode::RecClick => match slot {
                0 => Some("Cancel"),
                2 => Some("Finish"),
                _ => None,
            },
            _ => None,
        }
    }

    fn set_label(&mut self, text: Option<(String, f32)>, now: Instant) {
        let same = match (&self.label_cur, &text) {
            (Some(a), Some(b)) => a.0 == b.0,
            (None, None) => true,
            _ => false,
        };
        if same {
            // Under SLIDE the box still has to move when the slab changes.
            if let (Some(cur), Some(new)) = (self.label_cur.as_mut(), text.as_ref()) {
                cur.1 = new.1;
            }
            return;
        }
        self.label_prev = self.label_cur.take();
        self.label_cur = text;
        self.label_since = now;
    }

    /// Cursor polling. #20 settled that real winit events take over while
    /// expanded, but polling is what survives the cursor leaving fast, so
    /// hover is polled here and only *clicks* come from winit.
    fn poll_hover(&mut self, now: Instant) {
        if self.parked {
            return;
        }
        if !self.queue.is_empty() {
            self.hovered = None;
            return;
        }
        let Some(win) = self.win.as_ref() else { return };
        let Some((cx, cy)) = cursor_pos() else { return };

        match self.mode {
            Mode::Idle => {
                let g = self.geom_of(Mode::Idle);
                if win.contains(&g, cx, cy) {
                    self.go(Mode::Expanded, now);
                }
                self.hovered = None;
                self.cursor_inside = false;
            }
            Mode::Expanded | Mode::RecClick => {
                let g = self.geom_of(self.mode);
                let inside = win.contains(&g, cx, cy);
                self.cursor_inside = inside;
                if !inside {
                    if self.mode == Mode::Expanded {
                        self.go(Mode::Idle, now);
                    }
                    self.hovered = None;
                } else {
                    let l = self.layout_for(self.mode).unwrap();
                    let local = win.local_x(&g, cx);
                    let mut found = None;
                    for i in 0..3 {
                        let (a, b) = l.slab(i);
                        if local >= a && local <= b {
                            found = Some(i);
                        }
                    }
                    if found != self.hovered {
                        self.hovered = found;
                        self.hover_since = now;
                    }
                }
            }
            _ => {
                self.hovered = None;
                self.cursor_inside = false;
            }
        }

        // The indicator can lag the cursor (DWELL) or track it (FADE/SLIDE).
        let target = match self.indicator() {
            Indicator::Dwell => {
                if now.duration_since(self.hover_since) >= Duration::from_millis(70) {
                    self.hovered
                } else {
                    self.lit
                }
            }
            _ => self.hovered,
        };
        // Recording's centre slab is bars, not a button — nothing to light.
        let target = match (self.mode, target) {
            (Mode::RecClick, Some(1)) => None,
            _ => target,
        };
        if target != self.lit {
            self.lit_prev = self.lit;
            self.lit = target;
            self.lit_since = now;
        }

        // Labels follow the *lit* slab, not the raw cursor, so DWELL damps both.
        let copied = self.copied_until.is_some_and(|t| now < t);
        let next = if copied {
            Some(("Copied".to_string(), 0.0))
        } else {
            match (self.lit, self.layout_for(self.mode)) {
                (Some(i), Some(l)) => self.label_for(i).map(|s| (s.to_string(), l.slot_dx(i))),
                // Q5's awkward corner: the cursor is on the pill but on no
                // button — the inert end padding, or the gap between islands.
                // Off, the label vanishes; on, the last one holds, so drifting
                // into dead space doesn't flick it away.
                (None, Some(_)) if self.cursor_inside && self.label_over_pad => {
                    self.label_cur.clone()
                }
                _ => None,
            }
        };
        self.set_label(next, now);
    }

    fn click(&mut self, now: Instant) {
        let Some(slot) = self.hovered else { return };
        match (self.mode, slot) {
            (Mode::Expanded, 0) => {
                if self.history_empty {
                    println!("  [copy] disabled — history is empty");
                } else {
                    println!("  [copy] last transcript -> clipboard");
                    self.copied_until =
                        Some(now + Duration::from_millis(COPY_MS[self.copy_ms] as u64));
                }
            }
            (Mode::Expanded, 1) => {
                println!("  [dictate] click-started session");
                self.go(Mode::RecClick, now);
            }
            (Mode::Expanded, 2) => println!("  [settings] would launch the settings subprocess"),
            (Mode::RecClick, 0) => {
                println!("  [x] cancelled — no transcript, no history, no flash");
                match self.cancel() {
                    Cancel::NeutralFlash => {
                        self.go(Mode::Cancelled, now);
                        self.queue = [(Mode::Idle, 0)].into_iter().collect();
                        self.next_at = Some(now + Duration::from_millis(90 + 200));
                    }
                    _ => {
                        self.go(Mode::Idle, now);
                    }
                }
            }
            (Mode::RecClick, 2) => {
                println!("  [check] finished — same as releasing the hotkey");
                let dur = self.go(Mode::Processing, now);
                self.queue = [(Mode::Done, 900), (Mode::Idle, 0)].into_iter().collect();
                self.next_at = Some(now + dur + Duration::from_millis(1100));
            }
            _ => {}
        }
    }

    fn go(&mut self, mode: Mode, now: Instant) -> Duration {
        let dur = Duration::from_millis(self.dur_ms(self.mode, mode) as u64);
        self.anim = Some(Anim {
            from: self.current_geom(now),
            to: self.geom_of(mode),
            from_mode: self.mode,
            start: now,
            dur,
        });
        self.mode = mode;
        self.mode_since = now;
        if !mode.shows_buttons() {
            self.hovered = None;
            self.lit = None;
        }
        dur
    }

    fn play(&mut self, script: u8, now: Instant) {
        // `0` parks: no queue, so the state holds still until `r` or the mouse
        // moves it. Stills are how the *pixels* get judged; the scripts are how
        // the motion does.
        if script == 0 || script == 9 {
            self.queue.clear();
            self.next_at = None;
            self.queue_copy_at = None;
            self.parked = true;
            let (mode, slot, label) = if script == 0 {
                (Mode::Expanded, 1usize, "Dictate")
            } else {
                (Mode::RecClick, 2usize, "Finish")
            };
            self.go(mode, now);
            self.lit = Some(slot);
            self.lit_prev = None;
            self.lit_since = now - Duration::from_millis(200);
            let dx = self.layout_for(mode).map(|l| l.slot_dx(slot)).unwrap_or(0.0);
            self.set_label(
                Some((label.into(), dx)),
                now - Duration::from_millis(300),
            );
            return;
        }
        let (start, steps): (Mode, Vec<(Mode, u64)>) = match script {
            1 => (Mode::Idle, vec![(Mode::Expanded, 2600), (Mode::Idle, 0)]),
            2 => (
                Mode::Idle,
                vec![
                    (Mode::Expanded, 600),
                    (Mode::RecClick, 1800),
                    (Mode::Processing, 1000),
                    (Mode::Done, 900),
                    (Mode::Idle, 0),
                ],
            ),
            3 => (
                Mode::Idle,
                match self.cancel() {
                    Cancel::NeutralFlash => vec![
                        (Mode::Expanded, 600),
                        (Mode::RecClick, 1500),
                        (Mode::Cancelled, 200),
                        (Mode::Idle, 0),
                    ],
                    _ => vec![
                        (Mode::Expanded, 600),
                        (Mode::RecClick, 1500),
                        (Mode::Idle, 0),
                    ],
                },
            ),
            4 => (
                Mode::Idle,
                vec![
                    (Mode::RecHotkey, 1800),
                    (Mode::Processing, 1000),
                    (Mode::Done, 900),
                    (Mode::Idle, 0),
                ],
            ),
            _ => (Mode::Idle, vec![(Mode::Expanded, 2600), (Mode::Idle, 0)]),
        };
        self.parked = false;
        self.mode = start;
        self.anim = None;
        self.mode_since = now;
        self.hovered = None;
        self.lit = None;
        self.label_cur = None;
        self.label_prev = None;
        self.copied_until = None;
        self.queue = steps.into_iter().collect();
        self.next_at = Some(now);
        if script == 5 {
            // Fake the copy click 900ms in so the acknowledgement can be judged
            // without hitting a 22px target by hand.
            self.copied_until = None;
            self.queue_copy_at = Some(now + Duration::from_millis(900));
        }
    }

    fn pump_queue(&mut self, now: Instant) {
        if let Some(at) = self.queue_copy_at {
            if now >= at {
                self.queue_copy_at = None;
                self.lit = Some(0);
                self.copied_until =
                    Some(now + Duration::from_millis(COPY_MS[self.copy_ms] as u64));
            }
        }
        let Some(due) = self.next_at else { return };
        if now < due {
            return;
        }
        match self.queue.pop_front() {
            Some((mode, dwell)) => {
                let dur = self.go(mode, now);
                self.next_at = Some(now + dur + Duration::from_millis(dwell));
            }
            None => self.next_at = None,
        }
    }

    fn report(&self) {
        let d = self.dictate();
        let (_, hn, hms, hnote) = HANDOVERS[self.handover];
        let (_, vn, vnote) = INDICATORS[self.indicator];
        let (_, ln, lms, lnote) = LABEL_MOTIONS[self.label_motion];
        let (_, cn, cms, cnote) = CANCELS[self.cancel];
        let exp = self.expanded_layout();
        let rec = self.recclick_layout();

        // The arithmetic printed is the arithmetic *in force* — the two body
        // styles do not share a formula, and printing #18's unified one
        // regardless is how a width of 98 got explained by a sum that makes
        // 124.
        let b = self.body();
        println!("\n=== Q0 body      {}  —  {}", b.name, b.note);
        println!("=== Q1 sizes     {}  —  {}", d.name, d.note);
        if b.islands {
            println!(
                "      expanded = {:.0} + {:.0} gap + {:.0} + {:.0} gap + {:.0} = {:.0} wide",
                exp.island_w(0),
                b.gap,
                exp.island_w(1),
                b.gap,
                exp.island_w(2),
                exp.width()
            );
            println!(
                "      islands  = copy {:.0}x{:.0}   dictate {:.0}x{:.0}   settings {:.0}x{:.0}",
                exp.island_w(0),
                exp.island_h(0),
                exp.island_w(1),
                exp.island_h(1),
                exp.island_w(2),
                exp.island_h(2)
            );
        } else {
            println!(
                "      expanded = 2x{:.0} + {:.0} centre + 2x{:.0} flank + 2x{:.0} gap = {:.0}x{:.0}",
                exp.end_pad,
                d.centre_w,
                d.flank_w,
                exp.gap,
                exp.width(),
                EXP_H
            );
        }
        println!(
            "      glyph box: {:.0} in the centre, {:.0} in the flankers",
            exp.centre_glyph, exp.flank_glyph
        );
        println!(
            "      rec-click = 2x{:.0} + {:.0} bars + 2x{:.0} flank + 2x{:.0} gap = {:.0}x{:.0}  (always one body)",
            rec.end_pad,
            BARS_W,
            rec.flank_w,
            rec.gap,
            rec.width(),
            EXP_H
        );
        println!(
            "      slabs (from pill centre): [{:.0}..{:.0}] [{:.0}..{:.0}] [{:.0}..{:.0}]",
            exp.slab(0).0,
            exp.slab(0).1,
            exp.slab(1).0,
            exp.slab(1).1,
            exp.slab(2).0,
            exp.slab(2).1
        );
        println!("=== Q2 handover  {} ({}ms)  —  {}", hn, hms, hnote);
        println!("=== Q4 indicator {}  —  {}", vn, vnote);
        println!("=== Q5 label     {} ({}ms)  —  {}", ln, lms, lnote);
        println!(
            "      {}px Segoe-class face from {} ; shows over the inert end padding: {}",
            LABEL_PX,
            self.text.which,
            if self.label_over_pad { "YES" } else { "no" }
        );
        println!("=== Q6 cancel    {} ({}ms)  —  {}", cn, cms, cnote);
        println!("=== Q7 copied    {}ms in the label", COPY_MS[self.copy_ms]);
        let gs = &GLYPH_SIZES[self.glyph];
        println!(
            "=== Q3 scale     {}   icon proof strip: {}",
            SCALES[self.scale_ix].0,
            if self.strip { "ON" } else { "off" }
        );
        println!("=== Q3 glyph     {}  —  {}", gs.name, gs.note);
        println!(
            "      centre: 24-grid on {:.1}px of a {:.0} box, stroke {:.2}px",
            exp.centre_glyph * gs.frac,
            exp.centre_glyph,
            2.0 * exp.centre_glyph * gs.frac / 24.0
        );
        println!(
            "      flank:  24-grid on {:.1}px of a {:.0} box, stroke {:.2}px",
            exp.flank_glyph * gs.frac,
            exp.flank_glyph,
            2.0 * exp.flank_glyph * gs.frac / 24.0
        );
        println!(
            "    history empty: {}    flash outcome: {}",
            if self.history_empty { "YES" } else { "no" },
            if self.ok { "ok (green)" } else { "failed (red)" }
        );
        // No key list here any more — the control panel replaced the terminal
        // keys, and this dump gets pasted into the ticket, so it must not
        // advertise an interface that is gone.
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.win.is_none() {
            match PillWindow::create() {
                Ok(w) => self.win = Some(w),
                Err(e) => {
                    eprintln!("pill window creation failed: {e}");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            }
        }
        let started = *self.started.get_or_insert_with(Instant::now);
        let shooting = self.shot_step(ctx, started);
        self.tick();
        if !shooting {
            self.panel(ctx);
        }
        // The pill animates whether or not the panel is touched.
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

impl App {
    /// Headless capture, so the stills on the ticket can be regenerated rather
    /// than re-taken by hand:
    ///
    /// ```text
    /// DRAFT_PROTO_SHOT=expanded|recording|hotkey|strip
    /// DRAFT_PROTO_BODY=0|1|2   DRAFT_PROTO_OUT=shot.png
    /// ```
    ///
    /// Parks the state, saves one frame, quits.
    fn shot_step(&mut self, ctx: &egui::Context, started: Instant) -> bool {
        let Ok(which) = std::env::var("DRAFT_PROTO_SHOT") else {
            return false;
        };
        if let Ok(b) = std::env::var("DRAFT_PROTO_BODY") {
            self.islands = b.trim() != "0";
        }
        let elapsed = started.elapsed();
        if elapsed < Duration::from_millis(500) {
            let now = Instant::now();
            match which.as_str() {
                "recording" => self.play(9, now),
                "hotkey" => {
                    self.parked = true;
                    self.go(Mode::RecHotkey, now);
                }
                "strip" => self.strip = true,
                _ => self.play(0, now),
            }
        } else if elapsed < Duration::from_millis(900) {
            self.save_next = true;
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        true
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if take_pill_click() {
            self.click(now);
        }
        self.pump_queue(now);
        self.poll_hover(now);

        // #29: click-through is off only while the pill is showing buttons.
        // #20: a bare `SetWindowLongPtrW`, no `SWP_FRAMECHANGED`, never
        // winit's `set_cursor_hittest`.
        let want_transparent = !self.mode.shows_buttons();
        if want_transparent != self.click_transparent {
            if let Some(w) = self.win.as_ref() {
                set_click_through(w.hwnd(), want_transparent);
            }
            self.click_transparent = want_transparent;
        }

        let g = self.current_geom(now);
        let width = self.current_width(now, &g);
        let items = self.items(now);
        let frame = Frame {
            geom: Geom { w: width, ..g },
            items,
            lit: self.lit,
            lit_prev: self.lit_prev,
            lit_p: {
                let e = now.duration_since(self.lit_since).as_secs_f32() * 1000.0;
                out_cubic((e / 90.0).clamp(0.0, 1.0))
            },
            slide: self.indicator() == Indicator::Slide,
            layout: self.layout_for(self.mode),
            bars: self.bar_amps(now),
            label_cur: self.label_cur.clone(),
            label_prev: self.label_prev.clone(),
            label_p: {
                let e = now.duration_since(self.label_since).as_secs_f32() * 1000.0;
                (e / self.label_ms() as f32).clamp(0.0, 1.0)
            },
            label_motion: self.label_motion(),
            history_empty: self.history_empty,
            dictate_ring_a: self.dictate().ring_a,
            fold: self.fold_at(now),
            island_mix: self.island_mix(now),
            island_layout: self.expanded_layout(),
            glyph_frac: GLYPH_SIZES[self.glyph].frac,
            strip: self.strip,
        };
        let scale_override = SCALES[self.scale_ix].1;
        let dbg = format!("{:?} {:.0}x{:.0}", self.mode, frame.geom.w, frame.geom.h);
        if let Some(w) = self.win.as_mut() {
            if let Err(e) = w.render(&frame, &self.icons, &self.text, scale_override) {
                eprintln!("render failed: {e}");
            }
            if self.save_next {
                self.save_next = false;
                let out = std::env::var("DRAFT_PROTO_OUT")
                    .unwrap_or_else(|_| "pill-frame.png".into());
                self.saved_note = match w.save_png(&out) {
                    Ok(path) => format!("saved {dbg} -> {path}"),
                    Err(e) => format!("save failed: {e}"),
                };
            }
        }
    }
}

/// One titled group per question: the question in words, why it is a question,
/// then the candidates with their trade-offs under the selected one.
fn question<'a>(
    ui: &mut egui::Ui,
    title: &str,
    why: &str,
    value: &mut usize,
    options: impl Iterator<Item = (&'a str, &'a str)>,
) {
    ui.group(|ui| {
        ui.label(egui::RichText::new(title).strong());
        ui.label(egui::RichText::new(why).small().weak());
        ui.add_space(2.0);
        let mut notes: Vec<&str> = Vec::new();
        for (i, (name, note)) in options.enumerate() {
            ui.radio_value(value, i, name);
            notes.push(note);
        }
        if let Some(note) = notes.get(*value) {
            ui.add_space(2.0);
            ui.label(egui::RichText::new(*note).small().italics().weak());
        }
    });
}

/// The control panel. Its job is to make the seven questions the ticket asks
/// *legible* — one titled group each, with every candidate named and the
/// trade-off written under it — rather than a wall of one-letter keys. What is
/// being judged is on screen at the bottom; what is being chosen is here.
impl App {
    fn panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Draft pill — the whole interaction loop");
                ui.label(
                    egui::RichText::new(
                        "The pill is at the bottom of the screen. Hover it, click Dictate, \
                         then cancel or confirm. Everything here changes it live.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!(
                        "now: {:?}  ·  {}",
                        self.mode, self.last_action
                    ))
                    .monospace()
                    .small(),
                );
                ui.add_space(8.0);

                ui.group(|ui| {
                    ui.label(egui::RichText::new("Replay").strong());
                    ui.horizontal_wrapped(|ui| {
                        let now = Instant::now();
                        if ui.button("hover in / out").clicked() {
                            self.play(1, now);
                        }
                        if ui.button("click session ✓").clicked() {
                            self.play(2, now);
                        }
                        if ui.button("click session ✗").clicked() {
                            self.play(3, now);
                        }
                        if ui.button("hotkey session").clicked() {
                            self.play(4, now);
                        }
                        if ui.button("copy acknowledgement").clicked() {
                            self.play(5, now);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        let now = Instant::now();
                        if ui.button("hold expanded").clicked() {
                            self.play(0, now);
                        }
                        if ui.button("hold recording").clicked() {
                            self.play(9, now);
                        }
                        if ui.button("reset").clicked() {
                            self.queue.clear();
                            self.next_at = None;
                            self.copied_until = None;
                            self.parked = false;
                            self.go(Mode::Idle, now);
                        }
                        if ui.button("save PNG").clicked() {
                            self.save_next = true;
                        }
                        if ui.button("print spec").clicked() {
                            self.report();
                        }
                    });
                    if !self.saved_note.is_empty() {
                        ui.label(egui::RichText::new(&self.saved_note).small().weak());
                    }
                });

                ui.add_space(4.0);
                ui.group(|ui| {
                    ui.label(
                        egui::RichText::new("Q0 · Is the hover bar one body, or three islands?")
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Islands apply to the resident pill's hover bar ONLY; a click-started \
                             recording stays one body, so clicking Dictate has a shape change to \
                             make as well. They also invert #29's slab argument: with a visible \
                             gap, losing hover between buttons is honest rather than a flicker.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.add_space(2.0);
                    ui.checkbox(&mut self.islands, "Islands (off = one unified body)");
                    ui.label(
                        egui::RichText::new(self.body().note)
                            .small()
                            .weak(),
                    );
                });
                question(
                    ui,
                    "Q1 · Does Dictate want to be bigger than its flankers?  (REOPENED)",
                    "Each option sets four numbers — the two button widths and the two glyph \
                     boxes inside them, which are no longer the same thing, so a wider Dictate \
                     does not drag a wider microphone along with it. Back on UNIFORM by \
                     default; nothing here is marked chosen.",
                    &mut self.dictate,
                    PROPORTIONS.iter().map(|d| (d.name, d.note)),
                );
                question(
                    ui,
                    "Q2 · Expanded → Recording: morph or swap?",
                    "Clicking Dictate has to turn [copy][dictate][settings] into [×] ~~~ [✓].",
                    &mut self.handover,
                    HANDOVERS.iter().map(|h| (h.1, h.3)),
                );
                question(
                    ui,
                    "Q3a · How big is the glyph inside the button?  (CLOSED)",
                    "The icon's 24-unit grid has to map onto some fraction of the glyph box \
                     Q1 gives it. Smaller = more air, thinner stroke. Judged: AIR-0.72. The \
                     other two are kept only as the reference that was judged against.",
                    &mut self.glyph,
                    GLYPH_SIZES.iter().map(|g| (g.name, g.note)),
                );
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Q3b · Do the icons survive at every DPI?").strong());
                    ui.label(
                        egui::RichText::new(
                            "Forcing the scale fakes the DPI — the pill changes physical size \
                             as a side effect. For a true test, change Windows display scaling \
                             and leave this on NATIVE.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        for (i, (name, _)) in SCALES.iter().enumerate() {
                            ui.radio_value(&mut self.scale_ix, i, *name);
                        }
                    });
                    ui.checkbox(
                        &mut self.strip,
                        "icon proof sheet — every glyph at all four DPIs at once",
                    );
                });
                question(
                    ui,
                    "Q4 · Does hover chatter as the cursor sweeps the bar?",
                    "Three buttons change in ~100px. Slide the cursor fast along the pill.",
                    &mut self.indicator,
                    INDICATORS.iter().map(|i| (i.1, i.2)),
                );
                question(
                    ui,
                    "Q5 · What does the label do between buttons?",
                    "It sits above the pill and names what the cursor is on.",
                    &mut self.label_motion,
                    LABEL_MOTIONS.iter().map(|l| (l.1, l.3)),
                );
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut self.label_over_pad,
                        "…keep the label up over dead space",
                    );
                });
                ui.label(
                    egui::RichText::new(
                        "Dead space is the inert 11px end padding under UNIFIED, or the gap \
                         between islands. Off, the label vanishes the moment the cursor leaves \
                         a button; on, the last one holds until it reaches another. Drift the \
                         cursor to the very end of the bar to see the difference.",
                    )
                    .small()
                    .weak(),
                );
                question(
                    ui,
                    "Q6 · Does a silent cancel read as a cancel?",
                    "× returns straight to Idle with no flash — but 'the pill just went away' \
                     is also what a crash looks like.",
                    &mut self.cancel,
                    CANCELS.iter().map(|c| (c.1, c.3)),
                );
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Q7 · Does \"Copied\" land?").strong());
                    ui.label(
                        egui::RichText::new(
                            "The clipboard usually already holds that text, so the click is \
                             invisible without the label.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.horizontal(|ui| {
                        for (i, ms) in COPY_MS.iter().enumerate() {
                            ui.radio_value(&mut self.copy_ms, i, format!("{ms} ms"));
                        }
                    });
                });

                ui.add_space(4.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Other states").strong());
                    ui.checkbox(&mut self.history_empty, "history empty — Copy's disabled look");
                    ui.checkbox(&mut self.ok, "session succeeded (uncheck for the red flash)");
                });

                ui.add_space(6.0);
                let exp = self.expanded_layout();
                let rec = self.recclick_layout();
                ui.label(
                    egui::RichText::new(format!(
                        "expanded {:.0}×{:.0}   ·   recording(click) {:.0}×{:.0}   ·   \
                         glyph {:.1}px, stroke {:.2}px",
                        exp.width(),
                        EXP_H,
                        rec.width(),
                        EXP_H,
                        BTN_D * GLYPH_SIZES[self.glyph].frac,
                        2.0 * BTN_D * GLYPH_SIZES[self.glyph].frac / 24.0,
                    ))
                    .monospace()
                    .small()
                    .weak(),
                );
                ui.label(
                    egui::RichText::new(
                        "Judge it over a black desktop and a white one — the hairline and the \
                         glyph alpha have the same exposure.",
                    )
                    .small()
                    .weak(),
                );
            });
        });
    }

    fn bar_amps(&self, now: Instant) -> [f32; BAR_COUNT] {
        let wave_t = now.duration_since(self.wave_epoch).as_secs_f32();
        let recording = matches!(self.mode, Mode::RecClick | Mode::RecHotkey);
        if recording {
            return live_waveform(wave_t);
        }
        // #18/#29: the bars fall flat over the 320ms handoff.
        if self.mode == Mode::Processing {
            let since = now.duration_since(self.mode_since).as_secs_f32();
            let k = out_cubic((since * 1000.0 / 320.0).clamp(0.0, 1.0));
            let live = live_waveform(wave_t);
            let mut out = [0.0; BAR_COUNT];
            for i in 0..BAR_COUNT {
                out[i] = lerp(live[i], 0.0, k);
            }
            return out;
        }
        [0.0; BAR_COUNT]
    }
}

fn live_waveform(t: f32) -> [f32; BAR_COUNT] {
    let mut out = [0.0; BAR_COUNT];
    for (i, v) in out.iter_mut().enumerate() {
        let p = i as f32 * 0.9;
        *v = (0.5 + 0.5 * ((t * 6.0 + p).sin() * 0.6 + (t * 11.0 + p * 1.7).sin() * 0.4))
            .clamp(0.0, 1.0);
    }
    out
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

struct Frame {
    geom: Geom,
    items: Vec<Drawn>,
    lit: Option<usize>,
    lit_prev: Option<usize>,
    lit_p: f32,
    slide: bool,
    layout: Option<Layout>,
    bars: [f32; BAR_COUNT],
    label_cur: Option<(String, f32)>,
    label_prev: Option<(String, f32)>,
    label_p: f32,
    label_motion: LabelMotion,
    history_empty: bool,
    dictate_ring_a: f32,
    fold: f32,
    island_mix: f32,
    island_layout: Layout,
    glyph_frac: f32,
    strip: bool,
}

fn draw(
    pm: &mut Pixmap,
    scale: f32,
    f: &Frame,
    icons: &IconCache,
    text: &TextRenderer,
    pill_bottom: f32,
) {
    pm.fill(tiny_skia::Color::TRANSPARENT);
    let g = &f.geom;

    let w = pm.width() as f32;
    // The proof sheet takes the whole surface — it is a measuring instrument,
    // not part of the pill, and overlapping the two makes both unreadable.
    if f.strip {
        draw_proof_strip(pm, icons, text, w, scale, f.glyph_frac);
        return;
    }
    let sw = g.w.max(1.0) * scale;
    let sh = g.h.max(1.0) * scale;
    let border_w = (g.border_w * scale).max(1.0);
    let m = border_w * 0.5 + 1.0 * scale;

    let y = pill_bottom - sh + m;
    let rh = (sh - 2.0 * m).max(1.0);
    let cx = w / 2.0;
    let cy = y + rh / 2.0;

    // Islands and the single body coexist during a handover: the hover bar is
    // islands, the recording pill is not, so clicking Dictate has to get from
    // one to the other. They crossfade while the glyphs travel.
    let mix = f.island_mix;
    if mix < 0.999 {
        let x = cx - sw / 2.0 + m;
        let rw = (sw - 2.0 * m).max(1.0);
        let r = (g.radius * scale).min(rh / 2.0);
        body_shape(pm, g, x, y, rw, rh, r, border_w, 1.0 - mix);

        // The hover indicator, behind the glyphs.
        if let Some(l) = f.layout {
            draw_indicator(pm, scale, f, l, cx, cy, rh, 1.0 - mix);
        }
    }
    if mix > 0.001 {
        // Three bodies instead of one. The centre island interpolates from the
        // *whole pill's* current width, so at fold 0 it is exactly whatever
        // single shape the pill would otherwise be — the nub on the way in —
        // and no special case is needed at either end.
        let l = f.island_layout;
        for i in [0usize, 2, 1] {
            let target = l.island_w(i);
            let iw = if i == 1 {
                lerp(g.w, target, f.fold)
            } else {
                lerp(0.0, target, f.fold)
            } * scale;
            if iw <= 1.0 {
                continue;
            }
            // Islands shrink in height as well as width, so a 26px flanker is
            // a circle rather than a vertical stadium. They stay centred on
            // the pill's own axis, and the centre island travels from the
            // pill's current height so the nub grows out of it cleanly.
            let ih = lerp(g.h, l.island_h(i), f.fold) * scale;
            let dx = lerp(0.0, l.slot_dx(i), f.fold) * scale;
            let ix = cx + dx - iw / 2.0 + m;
            let irw = (iw - 2.0 * m).max(1.0);
            let irh = (ih - 2.0 * m).max(1.0).min(rh);
            let iy = cy - irh / 2.0;
            body_shape(pm, g, ix, iy, irw, irh, irw.min(irh) / 2.0, border_w, mix);
            // The island *is* the hover indicator — there is no gap for a
            // separate disc to distinguish itself from.
            let a = if f.lit == Some(i) {
                28.0 * f.lit_p
            } else if f.lit_prev == Some(i) {
                28.0 * (1.0 - f.lit_p)
            } else {
                0.0
            };
            let a = a * mix;
            if a > 0.5 {
                let mut pb = PathBuilder::new();
                rounded_rect(&mut pb, ix, iy, irw, irh, irw.min(irh) / 2.0);
                if let Some(p) = pb.finish() {
                    let mut paint = Paint::default();
                    paint.set_color_rgba8(255, 255, 255, a as u8);
                    paint.anti_alias = true;
                    pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
    }

    // Contents.
    for d in &f.items {
        if d.alpha <= 0.004 {
            continue;
        }
        let gx = cx + d.dx * scale;
        match d.item {
            Item::Bars => draw_bars(pm, scale, d.alpha, gx, cy, rh, &f.bars, d.size),
            Item::Icon(icon) => {
                // The Dictate button's resting disc, only when it is dominant.
                if d.slot == Some(1) && f.dictate_ring_a > 0.5 {
                    disc(
                        pm,
                        gx,
                        cy,
                        d.size * scale / 2.0,
                        (255.0, 255.0, 255.0),
                        f.dictate_ring_a / 255.0 * d.alpha,
                    );
                }
                // The recording pill's two controls carry their own chrome.
                // Confirm is the primary action of a click-started session,
                // and a stroked glyph among stroked glyphs has no way to say
                // so; a filled disc does. Cancel gets a faint one — present,
                // round, plainly secondary. Both are round, matching the
                // pill's own fully-rounded ends.
                let r = d.size * scale / 2.0;
                match icon {
                    Icon::Check => disc(pm, gx, cy, r, (255.0, 255.0, 255.0), d.alpha * 0.94),
                    Icon::X => disc(pm, gx, cy, r, (255.0, 255.0, 255.0), d.alpha * 0.10),
                    _ => {}
                }
                let dim = if f.history_empty && icon == Icon::Copy {
                    0.35
                } else if f.lit == d.slot && d.slot.is_some() {
                    1.0
                } else {
                    0.70
                };
                // Knocked out of the filled disc rather than drawn over it.
                let ink = if icon == Icon::Check {
                    BODY
                } else {
                    (255.0, 255.0, 255.0)
                };
                let dim = if icon == Icon::Check { 1.0 } else { dim };
                draw_icon_in(
                    pm,
                    icons,
                    icon,
                    gx,
                    cy,
                    d.glyph * scale * f.glyph_frac,
                    d.alpha * dim,
                    ink,
                );
            }
        }
    }

    // The label surface, above the pill.
    draw_label(pm, scale, f, text, w / 2.0, y);

}

/// One body — the whole pill under UNIFIED, one island under ISLANDS. Fill and
/// hairline both come from the animated `Geom`, so an island carries the same
/// hairline every state does (#18), three times over.
fn body_shape(
    pm: &mut Pixmap,
    g: &Geom,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    border_w: f32,
    alpha: f32,
) {
    let fill_a = g.fill_a * alpha;
    let border_a = g.border_a * alpha;
    if fill_a < 0.5 && border_a < 0.5 {
        return;
    }
    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, x, y, w, h, r);
    let Some(path) = pb.finish() else { return };

    let mut fill = Paint::default();
    fill.set_color_rgba8(
        g.fill_rgb.0.clamp(0.0, 255.0) as u8,
        g.fill_rgb.1.clamp(0.0, 255.0) as u8,
        g.fill_rgb.2.clamp(0.0, 255.0) as u8,
        fill_a.clamp(0.0, 255.0) as u8,
    );
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    if border_a >= 0.5 {
        let mut border = Paint::default();
        border.set_color_rgba8(
            g.border_rgb.0.clamp(0.0, 255.0) as u8,
            g.border_rgb.1.clamp(0.0, 255.0) as u8,
            g.border_rgb.2.clamp(0.0, 255.0) as u8,
            border_a.clamp(0.0, 255.0) as u8,
        );
        border.anti_alias = true;
        pm.stroke_path(
            &path,
            &border,
            &Stroke {
                width: border_w,
                ..Default::default()
            },
            Transform::identity(),
            None,
        );
    }
}

fn draw_indicator(
    pm: &mut Pixmap,
    scale: f32,
    f: &Frame,
    l: Layout,
    cx: f32,
    cy: f32,
    rh: f32,
    alpha: f32,
) {
    // The indicator used to be a *circle sized to the slot width*, which is
    // fine only while every slot is as wide as the pill is tall. A 48-wide
    // Dictate slot drew a 48px circle inside a 32px pill: it overflowed top
    // and bottom and bulged in the middle, and the flankers' circles ran into
    // the pill's own rounded ends. The `let _ = rh;` at the bottom was the
    // tell — the pill's height was not an input at all.
    //
    // It is now a rounded rect that lives *inside* the pill: full slot width,
    // inset from the top and bottom edges, radius half its height. It reads as
    // a pill within a pill at any slot width, and can never fight the outer
    // corners because it never reaches them.
    let inset = INDICATOR_INSET * scale;
    let h = (rh - 2.0 * inset).max(1.0);
    let ind = |i: usize| -> (f32, f32) { (cx + l.slot_dx(i) * scale, l.slot_w(i) * scale) };
    let white = (255.0, 255.0, 255.0);
    // #29: a fill behind the glyph at white @ ~28.
    let a = 28.0 / 255.0 * alpha;
    let mut hi = |x: f32, w: f32, a: f32| {
        if a <= 0.004 || w <= 1.0 {
            return;
        }
        let mut pb = PathBuilder::new();
        rounded_rect(&mut pb, x - w / 2.0, cy - h / 2.0, w, h, h / 2.0);
        let Some(p) = pb.finish() else { return };
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            white.0 as u8,
            white.1 as u8,
            white.2 as u8,
            (a.clamp(0.0, 1.0) * 255.0) as u8,
        );
        paint.anti_alias = true;
        pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    };
    if f.slide {
        match (f.lit, f.lit_prev) {
            (Some(to), Some(from)) => {
                let (x0, w0) = ind(from);
                let (x1, w1) = ind(to);
                hi(lerp(x0, x1, f.lit_p), lerp(w0, w1, f.lit_p), a);
            }
            (Some(to), None) => {
                let (x1, w1) = ind(to);
                hi(x1, w1, a * f.lit_p);
            }
            (None, Some(from)) => {
                let (x0, w0) = ind(from);
                hi(x0, w0, a * (1.0 - f.lit_p));
            }
            (None, None) => {}
        }
    } else {
        if let Some(to) = f.lit {
            let (x1, w1) = ind(to);
            hi(x1, w1, a * f.lit_p);
        }
        if let Some(from) = f.lit_prev {
            if Some(from) != f.lit {
                let (x0, w0) = ind(from);
                hi(x0, w0, a * (1.0 - f.lit_p));
            }
        }
    }
}

fn disc(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, rgb: (f32, f32, f32), a: f32) {
    if a <= 0.004 || r <= 0.5 {
        return;
    }
    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, cx - r, cy - r, r * 2.0, r * 2.0, r);
    let Some(p) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        rgb.0 as u8,
        rgb.1 as u8,
        rgb.2 as u8,
        (a.clamp(0.0, 1.0) * 255.0) as u8,
    );
    paint.anti_alias = true;
    pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
}

/// The whole of question 3 in one function: a 24-grid Lucide path scaled to the
/// button's diameter, stroked at 2 x scale, round caps and joins.
fn draw_icon(
    pm: &mut Pixmap,
    icons: &IconCache,
    icon: Icon,
    cx: f32,
    cy: f32,
    box_px: f32,
    alpha: f32,
) {
    draw_icon_in(pm, icons, icon, cx, cy, box_px, alpha, (255.0, 255.0, 255.0));
}

/// As `draw_icon`, but in a given colour — Confirm is a filled white disc with
/// its glyph knocked out in the pill's own body colour, which is the only way
/// a stroked glyph sitting among other stroked glyphs can say "primary".
#[allow(clippy::too_many_arguments)]
fn draw_icon_in(
    pm: &mut Pixmap,
    icons: &IconCache,
    icon: Icon,
    cx: f32,
    cy: f32,
    box_px: f32,
    alpha: f32,
    rgb: (f32, f32, f32),
) {
    let Some(base) = icons.get(icon) else { return };
    // Lucide draws inside a 24 box; the glyph occupies roughly 20 of it, so a
    // 22px button wants the whole grid mapped to ~ the button diameter.
    let k = box_px / 24.0;
    let t = Transform::from_scale(k, k).post_translate(cx - box_px / 2.0, cy - box_px / 2.0);
    let Some(path) = base.clone().transform(t) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        rgb.0 as u8,
        rgb.1 as u8,
        rgb.2 as u8,
        (alpha.clamp(0.0, 1.0) * 235.0) as u8,
    );
    paint.anti_alias = true;
    let stroke = Stroke {
        width: 2.0 * k,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

fn draw_bars(
    pm: &mut Pixmap,
    scale: f32,
    alpha: f32,
    cx: f32,
    cy: f32,
    rh: f32,
    amps: &[f32; BAR_COUNT],
    cluster_w: f32,
) {
    let total = cluster_w * scale;
    let n = BAR_COUNT as f32;
    let bar_w = (total / (2.0 * n - 1.0)).max(1.0);
    let gap = bar_w;
    let min_h = bar_w * 2.5;
    let max_h = (rh - 8.0 * scale).max(min_h);
    let start_x = cx - total / 2.0;

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, (235.0 * alpha).clamp(0.0, 255.0) as u8);
    paint.anti_alias = true;

    let mut pb = PathBuilder::new();
    for (i, &amp) in amps.iter().enumerate() {
        let bh = min_h + amp.clamp(0.0, 1.0) * (max_h - min_h);
        let bx = start_x + i as f32 * (bar_w + gap);
        rounded_rect(&mut pb, bx, cy - bh / 2.0, bar_w, bh, bar_w / 2.0);
    }
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// #29's label surface: a second shape in the *same* layered window, grown
/// upward. Everything about it is a guess this ticket has to confirm — the
/// gap, the width, the type size, and what it does when the cursor moves.
fn draw_label(
    pm: &mut Pixmap,
    scale: f32,
    f: &Frame,
    text: &TextRenderer,
    centre_x: f32,
    pill_top: f32,
) {
    let px = LABEL_PX * scale;
    let pad_x = LABEL_PAD_X * scale;
    let pad_y = LABEL_PAD_Y * scale;
    let bottom = pill_top - LABEL_GAP * scale;

    let mut one = |t: &str, dx: f32, alpha: f32, box_alpha: f32| {
        if alpha <= 0.004 && box_alpha <= 0.004 {
            return;
        }
        let (ink_lo, ink_hi) = text.ink_span(t, px);
        let bw = (ink_hi - ink_lo) + 2.0 * pad_x;
        let bh = px * 1.35 + 2.0 * pad_y;
        let bx = centre_x + dx * scale - bw / 2.0;
        let by = bottom - bh;
        let mut pb = PathBuilder::new();
        rounded_rect(&mut pb, bx, by, bw, bh, LABEL_R * scale);
        if let Some(p) = pb.finish() {
            let mut fill = Paint::default();
            fill.set_color_rgba8(13, 13, 13, (235.0 * box_alpha).clamp(0.0, 255.0) as u8);
            fill.anti_alias = true;
            pm.fill_path(&p, &fill, FillRule::Winding, Transform::identity(), None);
            let mut border = Paint::default();
            border.set_color_rgba8(
                HAIRLINE.0 as u8,
                HAIRLINE.1 as u8,
                HAIRLINE.2 as u8,
                (HAIRLINE_A * box_alpha).clamp(0.0, 255.0) as u8,
            );
            border.anti_alias = true;
            pm.stroke_path(
                &p,
                &border,
                &Stroke {
                    width: 1.0 * scale,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
        // Centre the cap band in the box, and the ink between the paddings.
        let baseline = by + bh / 2.0 + text.cap_height(px) / 2.0;
        text.draw(pm, t, bx + pad_x - ink_lo, baseline, px, alpha);
    };

    let p = f.label_p;
    match f.label_motion {
        LabelMotion::Crossfade => {
            // The box sits still and centred; only the text dissolves.
            if let Some((t, _)) = &f.label_prev {
                one(t, 0.0, 1.0 - p, if f.label_cur.is_some() { 0.0 } else { 1.0 - p });
            }
            if let Some((t, _)) = &f.label_cur {
                one(t, 0.0, p, 1.0);
            }
        }
        LabelMotion::Slide => {
            match (&f.label_prev, &f.label_cur) {
                (Some((pt, pdx)), Some((ct, cdx))) => {
                    let dx = lerp(*pdx, *cdx, out_cubic(p));
                    one(pt, dx, 1.0 - p, 0.0);
                    one(ct, dx, p, 1.0);
                }
                (None, Some((ct, cdx))) => one(ct, *cdx, p, p),
                (Some((pt, pdx)), None) => one(pt, *pdx, 1.0 - p, 1.0 - p),
                (None, None) => {}
            }
        }
        LabelMotion::Blank => {
            if let Some((t, dx)) = &f.label_cur {
                let k = ((p - 0.45) / 0.55).clamp(0.0, 1.0);
                one(t, *dx, k, k);
            }
        }
    }
}

/// Q3, made unambiguous: every glyph at all four DPI scales in one frame, so
/// "does 22px Lucide survive per-pixel alpha" is a comparison rather than a
/// memory of what it looked like at the last setting.
fn draw_proof_strip(
    pm: &mut Pixmap,
    icons: &IconCache,
    text: &TextRenderer,
    w: f32,
    scale: f32,
    glyph_frac: f32,
) {
    const SHOW: &[Icon] = &[Icon::Copy, Icon::Mic, Icon::Sliders, Icon::X, Icon::Check];
    let dpis = [1.0f32, 1.25, 1.5, 2.0];

    // On the pill's own body, at the pill's own alpha — glyphs floating on
    // nothing would be judged against the desktop instead of against the pill.
    let mut pb = PathBuilder::new();
    rounded_rect(
        &mut pb,
        4.0 * scale,
        4.0 * scale,
        w - 8.0 * scale,
        pm.height() as f32 - 8.0 * scale,
        12.0 * scale,
    );
    if let Some(p) = pb.finish() {
        let mut fill = Paint::default();
        fill.set_color_rgba8(13, 13, 13, 235);
        fill.anti_alias = true;
        pm.fill_path(&p, &fill, FillRule::Winding, Transform::identity(), None);
    }

    // A 22-logical-px button at `dpi` lands on `22 * dpi` *final* pixels, and
    // this pixmap is supersampled, so the row is drawn at `22 * dpi * SS` and
    // reaches the screen through exactly the pill's own downsample.
    let ss = SUPERSAMPLE as f32;
    let mut y = 8.0 * scale;
    for dpi in dpis {
        let box_px = BTN_D * dpi * ss;
        let cell = 31.0 * dpi * ss;
        let total = cell * SHOW.len() as f32;
        let mut x = (w - total) / 2.0 + 22.0 * scale;
        text.draw(
            pm,
            &format!("{dpi:.2}x"),
            8.0 * scale,
            y + box_px * 0.62,
            11.0 * scale,
            0.8,
        );
        for icon in SHOW {
            draw_icon(
                pm,
                icons,
                *icon,
                x + cell / 2.0,
                y + box_px / 2.0,
                box_px * glyph_frac,
                1.0,
            );
            x += cell;
        }
        y += box_px + 9.0 * scale;
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
    // Cubic, not quadratic. A quad with its control point on the corner is a
    // poor circle: at r = w/2 — every disc, and every island now that the
    // flankers are round — it visibly reads as a squircle rather than a
    // circle. `K` is the standard circle-from-cubics constant.
    const K: f32 = 0.552_284_7;
    let c = r * K;
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

fn pixmap_to_premul_bgra(pm: &Pixmap, dst: &mut [u8]) {
    let src = pm.data();
    let pixels = src.len().min(dst.len()) / 4;
    for i in 0..pixels {
        dst[i * 4] = src[i * 4 + 2];
        dst[i * 4 + 1] = src[i * 4 + 1];
        dst[i * 4 + 2] = src[i * 4];
        dst[i * 4 + 3] = src[i * 4 + 3];
    }
}

fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}

// ---------------------------------------------------------------------------
// Layered window (lifted from src/pill/window.rs)
// ---------------------------------------------------------------------------

struct PillWindow {
    scale: f32,
    win_x: i32,
    win_y: i32,
    pixmap: Pixmap,
    hires: Pixmap,
    mid: Pixmap,
    layered: LayeredSurface,
}

impl PillWindow {
    /// A raw Win32 layered window, not a winit one — the control panel owns
    /// the event loop now, and the pill has never needed anything winit
    /// provides beyond an HWND. It is created on the same thread, so the
    /// panel's message pump dispatches to `pill_proc` for free.
    fn create() -> Result<Self> {
        use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
        };

        let scale = unsafe {
            let dc = GetDC(None);
            let dpi = GetDeviceCaps(dc, LOGPIXELSX);
            ReleaseDC(None, dc);
            (dpi as f32 / 96.0).max(1.0)
        };

        let phys_w = (BOX_W as f32 * scale) as i32;
        let phys_h = (BOX_H as f32 * scale) as i32;
        let margin = (BOTTOM_MARGIN as f32 * scale) as i32;
        // #27 measures the margin from `rcWork`, so the taskbar comes off first.
        let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let x = (sw - phys_w) / 2;
        let y = sh - phys_h - margin - taskbar_h();

        let hwnd = create_pill_hwnd(x, y, phys_w, phys_h)?;
        let (w, h) = (phys_w.max(1) as u32, phys_h.max(1) as u32);
        let pixmap = Pixmap::new(w, h).ok_or_else(|| anyhow!("pixmap"))?;
        let hires =
            Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE).ok_or_else(|| anyhow!("hires"))?;
        let mid = Pixmap::new(w * 2, h * 2).ok_or_else(|| anyhow!("mid"))?;
        let layered = LayeredSurface::new(hwnd, w, h)?;

        Ok(Self {
            scale,
            win_x: x,
            win_y: y,
            pixmap,
            hires,
            mid,
            layered,
        })
    }

    fn hwnd(&self) -> HWND {
        self.layered.hwnd
    }

    /// Dumps the composed frame, alpha and all, so a still can be attached to
    /// the ticket instead of described.
    fn save_png(&self, name: &str) -> Result<String> {
        self.pixmap
            .save_png(name)
            .map_err(|e| anyhow!("save_png: {e}"))?;
        Ok(std::fs::canonicalize(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| name.to_string()))
    }

    /// Hit test a physical cursor position against the shape `g` would occupy.
    fn contains(&self, g: &Geom, cx: i32, cy: i32) -> bool {
        let s = self.scale;
        let slop = 3.0 * s;
        let sw = g.w * s;
        let sh = g.h * s;
        let box_w = BOX_W as f32 * s;
        let box_h = BOX_H as f32 * s;
        let left = self.win_x as f32 + (box_w - sw) / 2.0 - slop;
        let right = left + sw + 2.0 * slop;
        let bottom = self.win_y as f32 + box_h;
        let top = bottom - sh - slop;
        let (cx, cy) = (cx as f32, cy as f32);
        cx >= left && cx <= right && cy >= top && cy <= bottom
    }

    /// The cursor's x in the pill's own logical coordinates, measured from the
    /// pill's centre — which is the space #29's slabs are defined in.
    fn local_x(&self, _g: &Geom, cx: i32) -> f32 {
        let s = self.scale;
        let centre = self.win_x as f32 + BOX_W as f32 * s / 2.0;
        (cx as f32 - centre) / s
    }

    fn render(
        &mut self,
        f: &Frame,
        icons: &IconCache,
        text: &TextRenderer,
        scale_override: Option<f32>,
    ) -> Result<()> {
        let logical_scale = scale_override.unwrap_or(self.scale);
        let ss = logical_scale * SUPERSAMPLE as f32;
        // The pill's bottom edge sits at the box's bottom, in the hires pixmap.
        let bottom = self.hires.height() as f32;
        draw(&mut self.hires, ss, f, icons, text, bottom);

        let paint = tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        };
        let half = tiny_skia::Transform::from_scale(0.5, 0.5);

        self.mid.fill(tiny_skia::Color::TRANSPARENT);
        self.mid
            .draw_pixmap(0, 0, self.hires.as_ref(), &paint, half, None);
        self.pixmap.fill(tiny_skia::Color::TRANSPARENT);
        self.pixmap
            .draw_pixmap(0, 0, self.mid.as_ref(), &paint, half, None);

        self.layered.present(&self.pixmap)
    }
}

fn taskbar_h() -> i32 {
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe {
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CYSCREEN};
            return GetSystemMetrics(SM_CYSCREEN) - rect.bottom;
        }
    }
    0
}

/// A left-click landed on the pill. The wndproc runs inside the panel's
/// message pump, so it just raises a flag the next tick consumes.
static PILL_CLICKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn take_pill_click() -> bool {
    PILL_CLICKED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// #20's cost, load-bearing: `WS_EX_NOACTIVATE` has a documented
/// hover-to-activate hole, so the pill needs a wndproc answering
/// `WM_MOUSEACTIVATE`. Without it, clicking the pill can steal focus — which
/// breaks the whole product, since Draft pastes into whatever is focused.
unsafe extern "system" fn pill_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, MA_NOACTIVATE, WM_LBUTTONDOWN, WM_MOUSEACTIVATE,
    };
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            PILL_CLICKED.store(true, std::sync::atomic::Ordering::Relaxed);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

fn create_pill_hwnd(x: i32, y: i32, w: i32, h: i32) -> Result<HWND> {
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, RegisterClassExW, ShowWindow, CS_HREDRAW, CS_VREDRAW, SW_SHOWNOACTIVATE,
        WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP,
    };

    let class: Vec<u16> = "DraftPillProtoClass\0".encode_utf16().collect();
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(pill_proc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        // A duplicate registration is fine; only the first one counts.
        RegisterClassExW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW
                | WS_EX_TOPMOST,
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            hinstance,
            None,
        )?;
        if hwnd.is_invalid() {
            return Err(anyhow!("CreateWindowExW returned null"));
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

/// #20: a bare `SetWindowLongPtrW` on the event-loop thread, no
/// `SWP_FRAMECHANGED`, never winit's `set_cursor_hittest`.
fn set_click_through(hwnd: windows::Win32::Foundation::HWND, on: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT,
    };
    unsafe {
        let bit = WS_EX_TRANSPARENT.0 as isize;
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new = if on { cur | bit } else { cur & !bit };
        if new != cur {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
        }
    }
}

struct LayeredSurface {
    hwnd: windows::Win32::Foundation::HWND,
    mem_dc: windows::Win32::Graphics::Gdi::HDC,
    dib: windows::Win32::Graphics::Gdi::HBITMAP,
    bits: *mut u8,
    w: u32,
    h: u32,
}

impl LayeredSurface {
    fn new(hwnd: HWND, w: u32, h: u32) -> Result<Self> {
        let (mem_dc, dib, bits) = create_dib(w, h)?;
        Ok(Self {
            hwnd,
            mem_dc,
            dib,
            bits,
            w,
            h,
        })
    }

    fn present(&mut self, pm: &Pixmap) -> Result<()> {
        let byte_count = self.w as usize * self.h as usize * 4;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, byte_count) };
        pixmap_to_premul_bgra(pm, dst);
        unsafe {
            if self.update_layered().is_err() {
                rearm_layered(self.hwnd);
                self.update_layered()?;
            }
        }
        Ok(())
    }

    unsafe fn update_layered(&self) -> Result<()> {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            GetDC, ReleaseDC, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

        let screen_dc = GetDC(None);
        let size = windows::Win32::Foundation::SIZE {
            cx: self.w as i32,
            cy: self.h as i32,
        };
        let src_pt = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let res = UpdateLayeredWindow(
            self.hwnd,
            screen_dc,
            None,
            Some(&size),
            self.mem_dc,
            Some(&src_pt),
            windows::Win32::Foundation::COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(None, screen_dc);
        res.map_err(|e| anyhow!("UpdateLayeredWindow: {e}"))
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteDC, DeleteObject};
        unsafe {
            if !self.mem_dc.is_invalid() {
                let _ = DeleteDC(self.mem_dc);
            }
            if !self.dib.is_invalid() {
                let _ = DeleteObject(self.dib);
            }
        }
    }
}

unsafe fn rearm_layered(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED,
    };
    let layered = WS_EX_LAYERED.0 as isize;
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !layered);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | layered);
}

fn create_dib(
    w: u32,
    h: u32,
) -> Result<(
    windows::Win32::Graphics::Gdi::HDC,
    windows::Win32::Graphics::Gdi::HBITMAP,
    *mut u8,
)> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, GetDC, ReleaseDC, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    unsafe {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(screen_dc);
        ReleaseDC(None, screen_dc);
        if mem_dc.is_invalid() {
            return Err(anyhow!("CreateCompatibleDC failed"));
        }
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(
            mem_dc,
            &bi as *const _,
            DIB_RGB_COLORS,
            &mut bits as *mut _,
            None,
            0,
        )
        .map_err(|e| anyhow!("CreateDIBSection: {e}"))?;
        if dib.is_invalid() || bits.is_null() {
            return Err(anyhow!("CreateDIBSection returned null"));
        }
        SelectObject(mem_dc, dib);
        Ok((mem_dc, dib, bits as *mut u8))
    }
}
