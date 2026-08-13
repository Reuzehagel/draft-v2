// The label's text, as outlines — the same answer `pill::icons` gives for the
// glyphs, for the same reason.
//
// The pill reaches the screen through `UpdateLayeredWindow` with per-pixel
// alpha, at whatever scale the home monitor happens to be, and it is rendered
// at 4x and halved twice on the way out. A GDI `DrawTextW` into the DIB would
// put text outside that chain — no supersampling, ClearType subpixel fringes
// on a surface that has no opaque background to fringe against, and nothing the
// headless renderer tests or `pill::preview` could ever see. So the face is
// read for outlines only: a string becomes one `tiny_skia` path, centred on the
// origin, and is filled like any other shape.
//
// What this is *not* is a text stack. There is no shaping, no bidi, no
// fallback chain, no line breaking: the label draws two short strings from a
// fixed list (see `pill::label`), left to right, at one size. Kerning is left
// on the table with them — Segoe UI kerns through GPOS, which needs a shaper,
// and at 11.5px over twenty characters the difference is under a pixel.
//
// Building a path is cheap but not free, and the size only changes on a DPI
// change, so a built path is **cached per `(text, px)`** — the renderer asks
// for one per label per frame, and during a crossfade for two.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tiny_skia::{Path, PathBuilder};

/// The faces to look for, in order of preference.
///
/// Segoe UI is the Windows shell's own face and has shipped since Vista, so in
/// practice the first entry always wins; the rest are there so a stripped or
/// unusual install degrades to a face rather than to no label at all. "Segoe
/// class" is the whole requirement — a humanist sans at UI weight.
const FACES: [&str; 3] = ["segoeui.ttf", "tahoma.ttf", "arial.ttf"];

/// One laid-out string: the path to fill, and how wide it came out.
///
/// The width is the caller's business as much as the path is — the label's chip
/// is sized to the text it holds, and lerps between two of these mid-crossfade.
pub struct Text {
    /// Centred on the origin, both axes: the caller translates it to wherever
    /// the label is this frame and fills it.
    pub path: Arc<Path>,
    /// Advance width in device pixels — the ink's extent, not the chip's.
    pub width: f32,
}

/// The font file's bytes, read once.
///
/// `None` on a machine with none of [`FACES`] installed, which is not a Windows
/// machine as Draft understands one. The label simply draws nothing rather than
/// falling back to a shape that isn't text — see [`crate::pill::render`], which
/// skips the chip along with it.
fn font_bytes() -> Option<&'static [u8]> {
    static FONT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    FONT.get_or_init(|| {
        let dir = std::path::PathBuf::from(
            std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()),
        )
        .join("Fonts");
        for face in FACES {
            if let Ok(bytes) = std::fs::read(dir.join(face)) {
                tracing::debug!(face, "pill label face");
                return Some(bytes);
            }
        }
        tracing::warn!(?dir, "no label face found; the pill's label will not draw");
        None
    })
    .as_deref()
}

/// `text` laid out at `px` (the em size, in *device* pixels), centred
/// on the origin.
///
/// `None` when there is no face to draw it with, or when the string has no ink
/// at all — both mean "draw nothing", which is a state the label genuinely has.
pub fn label(text: &'static str, px: f32) -> Option<Arc<Text>> {
    /// Keyed by `(text, px)`, where the size is the float's bit pattern — the
    /// only exact key a float has. `None` is cached too: a string that produced
    /// no path will produce none again, and retrying it per frame would read
    /// the face per frame.
    type Cache = HashMap<(&'static str, u32), Option<Arc<Text>>>;
    thread_local! {
        static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        c.borrow_mut()
            .entry((text, px.to_bits()))
            .or_insert_with(|| build(text, px).map(Arc::new))
            .clone()
    })
}

fn build(text: &'static str, px: f32) -> Option<Text> {
    let face = ttf_parser::Face::parse(font_bytes()?, 0).ok()?;
    let upem = face.units_per_em() as f32;
    if upem <= 0.0 || px <= 0.0 {
        return None;
    }
    let s = px / upem;

    // Vertical centring is on the **cap height**, not on the em box or the
    // bounding box of these particular glyphs: "Copied" and "Settings" have to
    // sit on the same line as "Copy last transcript", and a box that moved with
    // the descenders present in one string and not another would rock the label
    // a pixel every time the text changed.
    let cap = face.capital_height().map_or(upem * 0.7, |c| c as f32) * s;

    let mut pb = PathBuilder::new();
    let mut pen = 0.0;
    for ch in text.chars() {
        let Some(id) = face.glyph_index(ch) else {
            continue;
        };
        let mut outline = Outline {
            pb: &mut pb,
            s,
            dx: pen,
            dy: 0.0,
        };
        face.outline_glyph(id, &mut outline);
        pen += face.glyph_hor_advance(id).unwrap_or(0) as f32 * s;
    }
    // A string of nothing but unmapped characters draws nothing.
    let path = pb.finish()?;

    // Shift the whole run so the origin is its visual centre — the caller then
    // translates by a centre point and nothing else.
    let path = path.transform(tiny_skia::Transform::from_translate(-pen / 2.0, cap / 2.0))?;
    Some(Text {
        path: Arc::new(path),
        width: pen,
    })
}

/// Feeds `ttf_parser`'s outline callbacks into a [`PathBuilder`], scaling to
/// device pixels and flipping the y axis on the way — font space is y-up and
/// the pixmap is y-down.
struct Outline<'a> {
    pb: &'a mut PathBuilder,
    s: f32,
    dx: f32,
    dy: f32,
}

impl Outline<'_> {
    fn x(&self, x: f32) -> f32 {
        self.dx + x * self.s
    }
    fn y(&self, y: f32) -> f32 {
        self.dy - y * self.s
    }
}

impl ttf_parser::OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(self.x(x), self.y(y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(self.x(x), self.y(y));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb
            .quad_to(self.x(x1), self.y(y1), self.x(x), self.y(y));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(
            self.x(x1),
            self.y(y1),
            self.x(x2),
            self.y(y2),
            self.x(x),
            self.y(y),
        );
    }

    fn close(&mut self) {
        self.pb.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one fact everything else rests on: there is a face to draw with, and
    /// a string comes back as ink. Windows ships Segoe UI; if this fails the
    /// label is silently absent on screen, which is exactly the failure a
    /// rendering test cannot see.
    #[test]
    fn a_label_becomes_a_path() {
        let t = label("Settings", 23.0).expect("no label face installed");
        assert!(t.width > 0.0);
        assert!(!t.path.bounds().width().is_nan());
    }

    /// Centred on the origin, both axes — the caller translates by a centre
    /// point and nothing else.
    #[test]
    fn a_label_is_centred_on_the_origin() {
        let t = label("Copy last transcript", 46.0).unwrap();
        let b = t.path.bounds();
        let cx = (b.left() + b.right()) / 2.0;
        let cy = (b.top() + b.bottom()) / 2.0;
        // The ink's centre is within a few percent of the advance-width centre:
        // side bearings and the cap-height baseline are the whole difference.
        assert!(cx.abs() < t.width * 0.03, "off centre horizontally: {cx}");
        assert!(cy.abs() < 46.0 * 0.15, "off centre vertically: {cy}");
    }

    /// The label scales with the DPI factor, like everything else the pill
    /// draws — the size is device pixels in, not a fixed bitmap.
    #[test]
    fn a_label_scales_with_its_size() {
        let one = label("Copied", 23.0).unwrap();
        let two = label("Copied", 46.0).unwrap();
        assert!(
            (two.width - one.width * 2.0).abs() < 0.5,
            "{} against {}",
            two.width,
            one.width
        );
    }

    /// Two different strings are two different widths — the chip is sized to
    /// the text it holds, so a measurement that ignored the text would size
    /// every label the same.
    #[test]
    fn a_longer_label_is_wider() {
        let short = label("Settings", 23.0).unwrap();
        let long = label("Copy last transcript", 23.0).unwrap();
        assert!(
            long.width > short.width * 1.5,
            "{} vs {}",
            long.width,
            short.width
        );
    }

    /// The cache hands back the same path rather than re-reading the face.
    #[test]
    fn a_label_is_built_once_per_size() {
        let a = label("Settings", 23.0).unwrap();
        let b = label("Settings", 23.0).unwrap();
        assert!(Arc::ptr_eq(&a.path, &b.path));
        // And a different size is a different entry.
        let c = label("Settings", 24.0).unwrap();
        assert!(!Arc::ptr_eq(&a.path, &c.path));
    }

    /// A string with no ink is `None`, not an empty path — "draw nothing" is a
    /// state the label has, and the renderer skips the chip with it.
    #[test]
    fn a_blank_label_has_no_path() {
        assert!(label("", 23.0).is_none());
        assert!(label("   ", 23.0).is_none());
    }
}
