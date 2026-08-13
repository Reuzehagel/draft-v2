// The button bar's glyphs: Lucide icons as SVG path `d` strings, parsed at
// runtime into `tiny_skia` paths and stroked with round caps and joins.
//
// Why not a font, and why not a PNG: the pill reaches the screen through
// `UpdateLayeredWindow` with per-pixel alpha, at whatever scale the home
// monitor happens to be. A font would need a text stack for three glyphs, and a
// fixed-size bitmap is wrong on every monitor but the one it was authored for.
// Paths are the one form that is right at arbitrary DPI (#29).
//
// Lucide because its 24-grid with stroke-2 round caps maps almost exactly onto
// the pill's glyph box, and `copy` / `mic` / `sliders-horizontal` are exactly
// the three the bar needs. ISC licensed.
//
// Parsing is cheap but not free, and the scale only changes on a DPI change, so
// a built path is **cached per `(icon, scale)`** — the render path asks for one
// per glyph per frame.

use crate::pill::core::Icon;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use tiny_skia::{Path, PathBuilder, Transform};

/// The grid Lucide draws on.
const GRID: f32 = 24.0;

/// Lucide's stroke width, in grid units.
const STROKE: f32 = 2.0;

/// The share of the glyph box the 24-grid is mapped onto. Less than 1 because
/// a Lucide icon fills its grid edge to edge: at 1.0 the strokes would touch
/// the box, and the box is what the island's padding is measured against.
///
/// On the settled 22px box that is 15.84px of glyph and a 1.32px stroke.
const GRID_FILL: f32 = 0.72;

/// The Lucide source for each glyph, as one `d` string.
///
/// Lucide authors some of these as `<rect>`/`<line>` elements rather than
/// paths; those are written out here as the path they describe, which is the
/// only editing the upstream data gets.
fn source(icon: Icon) -> &'static str {
    match icon {
        // `copy`: the front sheet (a rounded rect) with the back one behind it.
        Icon::Copy => {
            "M10 8h10a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2V10a2 2 0 0 1 2-2z\
             M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"
        }
        // `mic`: the capsule, its cradle, and the stand.
        Icon::Mic => {
            "M9 5a3 3 0 0 1 6 0v7a3 3 0 0 1-6 0z\
             M19 10v2a7 7 0 0 1-14 0v-2\
             M12 19v3"
        }
        // `sliders-horizontal`: three tracks, three handles.
        Icon::Sliders => {
            "M21 4h-7M10 4H3M21 12h-9M8 12H3M21 20h-5M12 20H3\
             M14 2v4M8 10v4M16 18v4"
        }
        // `x`: two strokes through the centre. Cancel.
        Icon::X => "M18 6 6 18M6 6l12 12",
        // `check`: the tick. Confirm — and the one glyph drawn *out* of a
        // filled disc rather than on top of a body, so its strokes carry the
        // shape of the button as much as the disc does.
        Icon::Check => "M20 6 9 17l-5-5",
    }
}

/// The stroke width for a glyph drawn in a `box_px` box — Lucide's stroke-2,
/// through the same mapping the path itself gets.
pub fn stroke_width(box_px: f32) -> f32 {
    box_px * GRID_FILL / GRID * STROKE
}

/// The glyph for `icon`, sized to a `box_px` box and centred on the origin, so
/// the caller translates it to wherever the button is this frame.
///
/// `None` only if the path data failed to parse, which is a bug in the constant
/// above rather than anything a user can reach — the caller draws no glyph.
pub fn glyph(icon: Icon, box_px: f32) -> Option<Arc<Path>> {
    /// Keyed by `(icon, scale)`, where the scale is the box's bit pattern —
    /// the only exact key a float has. `None` is cached too: a path that failed
    /// to parse will fail again, and retrying it per frame would log per frame.
    type Cache = HashMap<(Icon, u32), Option<Arc<Path>>>;
    thread_local! {
        static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
    }
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry((icon, box_px.to_bits()))
            .or_insert_with(|| build(icon, box_px).map(Arc::new))
            .clone()
    })
}

/// Parse one `d` string and place it: the 24-grid scaled onto `box_px` and
/// centred on the origin.
fn build(icon: Icon, box_px: f32) -> Option<Path> {
    let path = parse(source(icon))?;
    let scale = box_px * GRID_FILL / GRID;
    let half = GRID / 2.0;
    path.transform(Transform::from_translate(-half, -half).post_scale(scale, scale))
}

/// The subset of SVG path syntax the glyphs above are written in: move, line,
/// horizontal and vertical line, cubic, elliptical arc and close, absolute and
/// relative. Anything else is a data change that has to arrive with the code to
/// draw it, so it fails loudly here rather than dropping a stroke silently.
fn parse(d: &str) -> Option<Path> {
    use svgtypes::PathSegment as Seg;
    let mut pb = PathBuilder::new();
    // The current point and the subpath's start, both in grid units — a
    // relative segment is an offset from the former, and `z` returns to the
    // latter.
    let (mut cur, mut start) = ((0.0f64, 0.0f64), (0.0f64, 0.0f64));
    for seg in svgtypes::PathParser::from(d) {
        let seg = match seg {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "glyph path failed to parse");
                return None;
            }
        };
        // Absolute is the point itself; relative is the offset from where we
        // are. One place, so no segment below has to think about it.
        let at = |abs: bool, x: f64, y: f64| {
            if abs {
                (x, y)
            } else {
                (cur.0 + x, cur.1 + y)
            }
        };
        match seg {
            Seg::MoveTo { abs, x, y } => {
                cur = at(abs, x, y);
                start = cur;
                pb.move_to(cur.0 as f32, cur.1 as f32);
            }
            Seg::LineTo { abs, x, y } => {
                cur = at(abs, x, y);
                pb.line_to(cur.0 as f32, cur.1 as f32);
            }
            Seg::HorizontalLineTo { abs, x } => {
                cur = (if abs { x } else { cur.0 + x }, cur.1);
                pb.line_to(cur.0 as f32, cur.1 as f32);
            }
            Seg::VerticalLineTo { abs, y } => {
                cur = (cur.0, if abs { y } else { cur.1 + y });
                pb.line_to(cur.0 as f32, cur.1 as f32);
            }
            Seg::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let (c1, c2) = (at(abs, x1, y1), at(abs, x2, y2));
                cur = at(abs, x, y);
                pb.cubic_to(
                    c1.0 as f32,
                    c1.1 as f32,
                    c2.0 as f32,
                    c2.1 as f32,
                    cur.0 as f32,
                    cur.1 as f32,
                );
            }
            Seg::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x,
                y,
            } => {
                let to = at(abs, x, y);
                arc_to(&mut pb, cur, rx, ry, x_axis_rotation, large_arc, sweep, to);
                cur = to;
            }
            Seg::ClosePath { .. } => {
                pb.close();
                cur = start;
            }
            other => {
                tracing::error!(?other, "unsupported glyph path segment");
                return None;
            }
        }
    }
    pb.finish()
}

/// Append an SVG elliptical arc as cubics, since `tiny_skia` has no arc.
///
/// The endpoint parameterisation the `d` string uses is converted to a centre
/// and a sweep (SVG 1.1 F.6.5), then split into segments of at most a quarter
/// turn — beyond that a cubic approximation of an ellipse visibly deviates.
#[allow(clippy::too_many_arguments)]
fn arc_to(
    pb: &mut PathBuilder,
    from: (f64, f64),
    rx: f64,
    ry: f64,
    x_rotation_deg: f64,
    large_arc: bool,
    sweep: bool,
    to: (f64, f64),
) {
    use std::f64::consts::PI;
    // A zero-length arc, or one with a collapsed radius, is a line — both are
    // what the spec says and what a degenerate glyph should draw.
    let (rx, ry) = (rx.abs(), ry.abs());
    if (from.0 - to.0).abs() < f64::EPSILON && (from.1 - to.1).abs() < f64::EPSILON {
        return;
    }
    if rx == 0.0 || ry == 0.0 {
        pb.line_to(to.0 as f32, to.1 as f32);
        return;
    }
    let phi = x_rotation_deg.to_radians();
    let (cos_phi, sin_phi) = (phi.cos(), phi.sin());

    let dx = (from.0 - to.0) / 2.0;
    let dy = (from.1 - to.1) / 2.0;
    let x1 = cos_phi * dx + sin_phi * dy;
    let y1 = -sin_phi * dx + cos_phi * dy;

    // Radii too small to reach the endpoint are scaled up until they just do.
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    let (rx, ry) = if lambda > 1.0 {
        (rx * lambda.sqrt(), ry * lambda.sqrt())
    } else {
        (rx, ry)
    };

    let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let coeff = sign * (num / den).sqrt();
    let cx1 = coeff * rx * y1 / ry;
    let cy1 = -coeff * ry * x1 / rx;
    let cx = cos_phi * cx1 - sin_phi * cy1 + (from.0 + to.0) / 2.0;
    let cy = sin_phi * cx1 + cos_phi * cy1 + (from.1 + to.1) / 2.0;

    let angle = |ux: f64, uy: f64| uy.atan2(ux);
    let theta1 = angle((x1 - cx1) / rx, (y1 - cy1) / ry);
    let theta2 = angle((-x1 - cx1) / rx, (-y1 - cy1) / ry);
    let mut sweep_angle = theta2 - theta1;
    if !sweep && sweep_angle > 0.0 {
        sweep_angle -= 2.0 * PI;
    } else if sweep && sweep_angle < 0.0 {
        sweep_angle += 2.0 * PI;
    }

    let steps = (sweep_angle.abs() / (PI / 2.0)).ceil().max(1.0) as u32;
    let step = sweep_angle / steps as f64;
    // The control-point distance that makes a cubic match an arc of `step`.
    let alpha = 4.0 / 3.0 * (step / 4.0).tan();
    let point = |t: f64| {
        (
            cx + rx * t.cos() * cos_phi - ry * t.sin() * sin_phi,
            cy + rx * t.cos() * sin_phi + ry * t.sin() * cos_phi,
        )
    };
    let derivative = |t: f64| {
        (
            -rx * t.sin() * cos_phi - ry * t.cos() * sin_phi,
            -rx * t.sin() * sin_phi + ry * t.cos() * cos_phi,
        )
    };
    for i in 0..steps {
        let (t1, t2) = (theta1 + step * i as f64, theta1 + step * (i + 1) as f64);
        let (p1, p2) = (point(t1), point(t2));
        let (d1, d2) = (derivative(t1), derivative(t2));
        pb.cubic_to(
            (p1.0 + alpha * d1.0) as f32,
            (p1.1 + alpha * d1.1) as f32,
            (p2.0 - alpha * d2.0) as f32,
            (p2.1 - alpha * d2.1) as f32,
            p2.0 as f32,
            p2.1 as f32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pill::core::{BUTTONS, CHECK_BUTTONS, CHECK_GLYPH_BOX, GLYPH_BOX};

    /// Every glyph either button list names parses, and lands inside its box.
    /// A path that silently failed would be a button with no icon on it.
    ///
    /// The share of the box each list has to fill differs, and that is a fact
    /// about Lucide rather than a slackened assertion: `copy`, `mic` and
    /// `sliders` are drawn edge to edge on the 24-grid, while `x` and `check`
    /// are marks in the middle of it. Scaling the latter pair up to match
    /// would draw them heavier than every other icon in the app.
    #[test]
    fn every_button_glyph_parses_and_fits_its_box() {
        for (buttons, box_px, fills) in [
            (&BUTTONS[..], GLYPH_BOX, 0.5),
            (&CHECK_BUTTONS[..], CHECK_GLYPH_BOX, 0.3),
        ] {
            for b in buttons {
                let path = glyph(b.icon, box_px).unwrap_or_else(|| panic!("{:?}", b.icon));
                let r = path.bounds();
                let half = box_px / 2.0;
                // Centred on the origin: the renderer translates it to the button.
                assert!(
                    r.left() >= -half && r.right() <= half,
                    "{:?} is wider than its box: {r:?}",
                    b.icon
                );
                assert!(
                    r.top() >= -half && r.bottom() <= half,
                    "{:?} is taller than its box: {r:?}",
                    b.icon
                );
                // And it actually fills its share — a glyph shrunk to a dot
                // would pass the bounds check above. Only on its longer side: a
                // mic is a tall icon and does not fill the box across.
                assert!(
                    r.width().max(r.height()) > box_px * fills,
                    "{:?} does not fill its box: {r:?}",
                    b.icon
                );
                assert!(r.width() > 1.0 && r.height() > 1.0, "{:?}: {r:?}", b.icon);
            }
        }
    }

    /// The mapping the ticket states: the 24-grid over 0.72 of the box, and
    /// Lucide's stroke-2 scaled by the same factor.
    #[test]
    fn the_grid_maps_onto_the_settled_share_of_the_glyph_box() {
        assert!((GLYPH_BOX * GRID_FILL - 15.84).abs() < 1e-4);
        assert!((stroke_width(GLYPH_BOX) - 1.32).abs() < 1e-4);
    }

    /// Cached per `(icon, scale)` — the render path asks once per glyph per
    /// frame, and parsing on each of those would be work for nothing.
    #[test]
    fn a_glyph_is_built_once_per_icon_and_scale() {
        let a = glyph(Icon::Mic, 22.0).unwrap();
        let b = glyph(Icon::Mic, 22.0).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        // A different scale is a different glyph, not a rescale of that one.
        let c = glyph(Icon::Mic, 44.0).unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(c.bounds().width() > a.bounds().width() * 1.9);
    }

    /// The arc conversion, against the shape it is actually used for: the mic's
    /// capsule is two semicircles, and every point on them has to sit on the
    /// circle they claim to be.
    #[test]
    fn arcs_land_on_the_circle_they_describe() {
        // A unit circle drawn as two semicircular arcs, in grid units.
        let mut pb = PathBuilder::new();
        pb.move_to(-1.0, 0.0);
        arc_to(&mut pb, (-1.0, 0.0), 1.0, 1.0, 0.0, false, true, (1.0, 0.0));
        arc_to(&mut pb, (1.0, 0.0), 1.0, 1.0, 0.0, false, true, (-1.0, 0.0));
        let path = pb.finish().unwrap();
        let b = path.bounds();
        // A circle of radius 1, to within a cubic's approximation error.
        for v in [b.left(), b.top()] {
            assert!((v + 1.0).abs() < 0.01, "{b:?}");
        }
        for v in [b.right(), b.bottom()] {
            assert!((v - 1.0).abs() < 0.01, "{b:?}");
        }
    }
}
