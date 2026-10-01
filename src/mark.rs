// The mark: Draft's icon, which is the pill — a stadium with a waveform cut
// out of it, the shape that sits on screen while you dictate. One drawing,
// two dresses: bare in the taskbar's ink for the tray (`tray`), and white on
// a dark tile for everywhere Windows shows an application (`app_rgba`): the
// exe, the settings window, the Start menu and Add/Remove Programs.
//
// Laid out in whole pixels at every size rather than scaled from one grid,
// for the reason `tray::icon_rgba` gives: a scaled grid is only sharp at the
// size it was drawn for. Every length is rounded to the parity of the icon's
// size, so the pill and each bar centre on pixel boundaries and every
// straight edge is a hard one; only the curves are anti-aliased.
//
// The exe's icon is `assets/draft.ico`, checked in because the resource
// compiler wants a file. It is this module's output — regenerate it with
// `cargo test -- --ignored mark::tests::write`, and `the_checked_in_icon_is_
// the_one_drawn_here` fails until you do.

use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Rect, Transform};

use crate::pill::icons::parse;

/// The app tile's fill: the pill's own body colour (`pill::geom::BODY`).
const TILE: [u8; 3] = [13, 13, 13];
/// The tile's rim, so a dark tile on a dark taskbar or Start menu still has an
/// edge.
const TILE_RIM: [u8; 3] = [64, 66, 70];

/// The sizes the `.ico` carries: Windows' small and large icons at each
/// common scale, and 256 for Explorer's large views.
#[cfg(test)]
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// The nearest whole number to `t` with the same parity as `px`, so a length
/// of it centres on pixel boundaries in a `px` square. Ties go down.
fn fit(t: f32, px: u32) -> u32 {
    let n = t.round() as u32;
    if (n + px).is_multiple_of(2) {
        n
    } else if t > n as f32 {
        n + 1
    } else {
        n - 1
    }
}

/// A rectangle with rounded corners as a path; `r` of half the height is a
/// stadium. A zero radius is a plain rectangle, since an arc of radius 0 is
/// not one the parser draws.
fn rounded(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Option<Path> {
    if r <= 0.0 {
        return Rect::from_ltrb(x0, y0, x1, y1).map(PathBuilder::from_rect);
    }
    parse(&format!(
        "M{a} {y0}H{b}A{r} {r} 0 0 1 {x1} {c}V{d}A{r} {r} 0 0 1 {b} {y1}\
         H{a}A{r} {r} 0 0 1 {x0} {d}V{c}A{r} {r} 0 0 1 {a} {y0}Z",
        a = x0 + r,
        b = x1 - r,
        c = y0 + r,
        d = y1 - r,
    ))
}

/// Make a `px` square symmetric about its vertical centre line by copying the
/// left half across. The rasteriser's anti-aliasing of a curve isn't
/// mirror-exact, and at 16px the difference is a visibly lopsided icon.
fn mirror_x<T: Copy>(buf: &mut [T], px: u32) {
    let px = px as usize;
    for y in 0..px {
        for x in px.div_ceil(2)..px {
            buf[y * px + x] = buf[y * px + (px - 1 - x)];
        }
    }
}

/// The same about the horizontal centre line, copying the top half down.
fn mirror_y<T: Copy>(buf: &mut [T], px: u32) {
    let px = px as usize;
    for y in px.div_ceil(2)..px {
        for x in 0..px {
            buf[y * px + x] = buf[(px - 1 - y) * px + x];
        }
    }
}

/// The waveform cut into the pill: each bar's height as a share of the
/// pill's, left to right. Uneven, so it reads as sound rather than as a face;
/// three bars where four would leave cuts too thin to see.
const BARS: [f32; 4] = [0.36, 0.72, 0.5, 0.28];
const BARS_SMALL: [f32; 3] = [0.32, 0.62, 0.32];

/// The pill's coverage in a `px` square, `w` wide and centred; `w` must have
/// the parity of `px`. One byte per pixel: 255 is pill, 0 is not, and the
/// bars are cut clean through it.
fn pill(px: u32, w: u32) -> Vec<u8> {
    let h = fit(0.66 * w as f32, px);
    let (shares, bar_w, gap): (&[f32], u32, u32) = if h >= 16 {
        // Four bars: the row is 4 widths and 3 gaps, so the gap carries the
        // parity that centres it.
        let bar_w = (0.14 * h as f32).round() as u32;
        (&BARS, bar_w, fit(0.11 * h as f32, px).max(2))
    } else {
        // Three: the width carries it.
        let bar_w = fit(h as f32 / 6.0, px).max(2);
        (&BARS_SMALL, bar_w, bar_w + bar_w / 4)
    };

    let s = px as f32;
    let (x0, y0) = ((px - w) as f32 / 2.0, (px - h) as f32 / 2.0);
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;

    let mut pm = Pixmap::new(px, px).expect("non-zero icon size");
    if let Some(body) = rounded(x0, y0, s - x0, s - y0, h as f32 / 2.0) {
        pm.fill_path(
            &body,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let mut body: Vec<u8> = pm.pixels().iter().map(|p| p.alpha()).collect();
    mirror_x(&mut body, px);
    mirror_y(&mut body, px);

    // The bars are drawn apart and knocked out of the body, rather than
    // painted in a second colour, so whatever is behind the icon shows
    // through them. Only mirrored top to bottom: the row is uneven.
    let mut pm = Pixmap::new(px, px).expect("non-zero icon size");
    // Square ends while a bar is too thin for a round one to be anything but
    // blur.
    let r = if bar_w >= 4 { bar_w as f32 / 2.0 } else { 0.0 };
    let n = shares.len() as u32;
    let mut left = (px - (n * bar_w + (n - 1) * gap)) as f32 / 2.0;
    for &share in shares {
        let top = (px - fit(share * h as f32, px)) as f32 / 2.0;
        let right = left + bar_w as f32;
        if let Some(bar) = rounded(left, top, right, s - top, r) {
            pm.fill_path(&bar, &paint, FillRule::Winding, Transform::identity(), None);
        }
        left = right + gap as f32;
    }
    let mut bars: Vec<u8> = pm.pixels().iter().map(|p| p.alpha()).collect();
    mirror_y(&mut bars, px);

    body.iter()
        .zip(&bars)
        .map(|(&b, &cut)| (b as u32 * (255 - cut as u32) / 255) as u8)
        .collect()
}

/// The tray's glyph: the pill nearly edge to edge in a `px` square, as
/// coverage for the caller to ink.
pub fn tray(px: u32) -> Vec<u8> {
    let margin = (px as f32 / 16.0).round().max(1.0) as u32;
    pill(px, px - 2 * margin)
}

/// The application icon at `px`: the pill in white on a dark rounded tile, as
/// straight RGBA.
pub fn app_rgba(px: u32) -> Vec<u8> {
    let s = px as f32;
    let mut pm = Pixmap::new(px, px).expect("non-zero icon size");
    let mut paint = Paint::default();
    let [r, g, b] = TILE_RIM;
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    if let Some(rim) = rounded(0.0, 0.0, s, s, (0.22 * s).round()) {
        pm.fill_path(&rim, &paint, FillRule::Winding, Transform::identity(), None);
    }
    // The rim is the gap between two fills, not a stroke, so it is a whole
    // number of pixels wide like everything else.
    let rim = (s / 64.0).round().max(1.0);
    let [r, g, b] = TILE;
    paint.set_color_rgba8(r, g, b, 255);
    if let Some(tile) = rounded(rim, rim, s - rim, s - rim, (0.22 * s).round() - rim) {
        pm.fill_path(
            &tile,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    let mut rgba: Vec<[u8; 4]> = pm
        .pixels()
        .iter()
        .map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    mirror_x(&mut rgba, px);
    mirror_y(&mut rgba, px);

    // White over the tile by the pill's coverage. The pill sits well inside
    // the tile's opaque middle, so only the colour mixes.
    // Larger in the small tiles, where the tile's margin would leave a pill
    // too small for its cuts to read.
    let share = if px <= 32 { 0.78 } else { 0.66 };
    let glyph = pill(px, fit(share * s, px));
    for (o, &a) in rgba.iter_mut().zip(&glyph) {
        let a = a as u32;
        for c in &mut o[..3] {
            *c = ((255 * a + *c as u32 * (255 - a)) / 255) as u8;
        }
    }
    rgba.into_iter().flatten().collect()
}

/// The application icon as an `.ico`: one PNG entry per size in `ICO_SIZES`,
/// which every Windows since Vista reads. Only the tests call it: the build
/// embeds the checked-in file this writes (see the top of this file).
#[cfg(test)]
fn ico() -> Vec<u8> {
    let pngs: Vec<Vec<u8>> = ICO_SIZES
        .iter()
        .map(|&px| {
            let mut pm = Pixmap::new(px, px).expect("non-zero icon size");
            for (o, p) in pm.pixels_mut().iter_mut().zip(app_rgba(px).chunks_exact(4)) {
                *o = tiny_skia::ColorU8::from_rgba(p[0], p[1], p[2], p[3]).premultiply();
            }
            pm.encode_png().expect("encode icon png")
        })
        .collect();

    let count = ICO_SIZES.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]); // reserved, type 1 = icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * count as u32;
    for (&px, png) in ICO_SIZES.iter().zip(&pngs) {
        // A dimension of 0 means 256.
        let dim = if px >= 256 { 0 } else { px as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]); // no palette, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in pngs {
        out.extend_from_slice(&png);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tray's sizes (`SM_CXSMICON` at 100–250%) and the `.ico`'s.
    fn sizes() -> impl Iterator<Item = u32> {
        [16, 18, 20, 24, 28, 32, 40, 48, 64, 256].into_iter()
    }

    fn ico_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/draft.ico")
    }

    #[test]
    fn fit_keeps_the_parity_of_the_square() {
        assert_eq!(fit(9.24, 16), 10);
        assert_eq!(fit(13.0, 24), 12);
        assert_eq!(fit(7.0, 15), 7);
        assert_eq!(fit(7.4, 16), 8);
    }

    #[test]
    fn the_pill_has_hard_flat_edges_and_clean_cuts() {
        for px in sizes() {
            let c = tray(px);
            let row = |y: u32| &c[(y * px) as usize..((y + 1) * px) as usize];
            // The middle row crosses the body and every bar: bar, gap and body
            // are each whole pixels, so nothing between the caps is half-lit.
            let mid = row(px / 2);
            let inked: Vec<usize> = (0..px as usize).filter(|&x| mid[x] > 0).collect();
            let (first, last) = (inked[0], *inked.last().unwrap());
            let inner = &mid[first + 1..last];
            assert!(
                inner.iter().all(|&a| a == 0 || a == 255),
                "{px}px middle row blurs: {mid:?}"
            );
            let cuts = inner
                .windows(2)
                .filter(|w| w[0] == 255 && w[1] == 0)
                .count();
            assert!((3..=4).contains(&cuts), "{px}px: {mid:?}");
            // The top edge is a full-coverage run, with nothing above it.
            let top = (0..px).find(|&y| row(y).iter().any(|&a| a > 0)).unwrap();
            assert!(row(top).contains(&255), "{px}px top edge blurs");
        }
    }

    #[test]
    fn the_tray_glyph_is_wide_and_stays_off_the_edges() {
        for px in sizes() {
            let c = tray(px);
            let cols = (0..px).filter(|&x| (0..px).any(|y| c[(y * px + x) as usize] > 0));
            let (min, max) = (cols.clone().min().unwrap(), cols.max().unwrap());
            assert!(min > 0 && max < px - 1, "{px}px touches the edge");
            assert!(max - min + 1 >= px * 3 / 4, "{px}px: too narrow");
        }
    }

    #[test]
    fn the_app_icon_is_an_opaque_tile_with_clear_corners() {
        for px in sizes() {
            let rgba = app_rgba(px);
            assert_eq!(rgba.len(), (px * px * 4) as usize);
            assert_eq!(rgba[3], 0, "{px}px corner");
            let centre = ((px / 2 * px + px / 2) * 4) as usize;
            assert_eq!(rgba[centre + 3], 255, "{px}px centre");
        }
    }

    #[test]
    fn the_checked_in_icon_is_the_one_drawn_here() {
        // Compared as pixels, not bytes: Cargo.lock isn't checked in, and a
        // newer PNG encoder may compress the same image differently.
        let ico = std::fs::read(ico_path()).expect("read assets/draft.ico");
        let le = |at: usize, n: usize| {
            ico[at..at + n]
                .iter()
                .rev()
                .fold(0usize, |v, &b| v << 8 | b as usize)
        };
        let stale = "assets/draft.ico is stale: cargo test -- --ignored mark::tests::write";
        assert_eq!(le(4, 2), ICO_SIZES.len(), "{stale}");
        for (i, &px) in ICO_SIZES.iter().enumerate() {
            let entry = 6 + 16 * i;
            let (len, offset) = (le(entry + 8, 4), le(entry + 12, 4));
            let pm = Pixmap::decode_png(&ico[offset..offset + len]).expect("decode entry");
            assert_eq!((pm.width(), pm.height()), (px, px), "{stale}");
            let drawn = app_rgba(px);
            for (p, want) in pm.pixels().iter().zip(drawn.chunks_exact(4)) {
                let c = p.demultiply();
                // Premultiplying on the way in can move a translucent edge
                // pixel's colour by a step, and a clear one has none; the
                // alpha is exact.
                assert_eq!(c.alpha(), want[3], "{px}px: {stale}");
                if want[3] > 0 {
                    for (got, want) in [c.red(), c.green(), c.blue()].iter().zip(&want[..3]) {
                        assert!(got.abs_diff(*want) <= 2, "{px}px: {stale}");
                    }
                }
            }
        }
    }

    /// Writes `assets/draft.ico`, and every size of the app icon and the tray
    /// glyph to `target/mark-preview/` for looking at. Run with
    /// `cargo test -- --ignored mark::tests::write`.
    #[test]
    #[ignore]
    fn write() {
        std::fs::write(ico_path(), ico()).expect("write assets/draft.ico");
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/mark-preview");
        std::fs::create_dir_all(&dir).expect("create preview dir");
        for px in sizes() {
            let mut pm = Pixmap::new(px, px).unwrap();
            for (o, p) in pm.pixels_mut().iter_mut().zip(app_rgba(px).chunks_exact(4)) {
                *o = tiny_skia::ColorU8::from_rgba(p[0], p[1], p[2], p[3]).premultiply();
            }
            pm.save_png(dir.join(format!("app-{px}.png"))).unwrap();
        }
    }
}
