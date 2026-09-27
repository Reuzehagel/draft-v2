// The pill's pixels, from a Geom to the device-resolution premultiplied RGBA
// that goes into the layered window — the supersample chain `PillWindow` used
// to run inline, with a window-free seam so it can be tested and timed (#106).
//
// Rendered at `SUPERSAMPLE`x device resolution, then halved twice (4x → 2x →
// 1x). Each halving is an exact 2x reduction, where bilinear sampling becomes a
// clean 2x2 box average — this avoids both bilinear's undersampling (when
// downscaling >2x in one shot) and bicubic's ringing halos at the high-contrast
// border edge. It must be a power of two so the chain lands exactly on device
// resolution.
//
// **Only what a frame could have changed is touched.** The envelope is 260x80
// and a recording pill is 62x28 of it, so clearing, drawing and halving the
// whole surface every frame was mostly spent on transparent pixels — at 150%
// that is a 1560x480 buffer, ~30 times a second, for as long as anyone talks.
// Instead the renderer reports where it drew (`render::draw`'s bounds), and a
// frame's **damage** is that and whatever the frame before it drew: the one
// has to appear, the other has to go. Outside the damage every buffer is
// already transparent — that is the invariant, and it is why the first frame
// damages everything. The pixels inside it are the same bytes the whole-surface
// chain produced: the halving is the same shader over the same source, only
// asked about fewer destination pixels.

use crate::pill::core::BodyStyle;
use crate::pill::geom::{Geom, Slots};
use crate::pill::label::Fade;
use crate::pill::render;
use tiny_skia::{
    FilterQuality, IntRect, Paint, Pattern, Pixmap, PremultipliedColorU8, SpreadMode, Transform,
};

/// Render at this multiple of device resolution — see the module header.
pub const SUPERSAMPLE: u32 = 4;

pub struct Surface {
    hires: Pixmap,
    /// Intermediate 2x buffer for the halving chain.
    mid: Pixmap,
    out: Pixmap,
    /// Where the last frame drew, in device pixels: the only part of any buffer
    /// that may hold anything but transparency.
    drawn: Option<IntRect>,
    /// No frame drawn yet. The first one damages the whole surface, because
    /// whatever it is presented into has never been written.
    fresh: bool,
}

impl Surface {
    /// A transparent surface of `w`x`h` device pixels, or `None` if the
    /// buffers can't be had.
    pub fn new(w: u32, h: u32) -> Option<Self> {
        Some(Self {
            hires: Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE)?,
            mid: Pixmap::new(w * 2, h * 2)?,
            out: Pixmap::new(w, h)?,
            drawn: None,
            fresh: true,
        })
    }

    pub fn width(&self) -> u32 {
        self.out.width()
    }

    pub fn height(&self) -> u32 {
        self.out.height()
    }

    /// The last frame, at device resolution.
    pub fn pixels(&self) -> &Pixmap {
        &self.out
    }

    /// Draw one frame at `scale` (the home monitor's, not the supersample's),
    /// and say which device pixels of [`Self::pixels`] it changed — `None` if
    /// none could have: nothing drawn now, and nothing the frame before drew
    /// left to clear.
    pub fn draw(
        &mut self,
        scale: f32,
        geom: &Geom,
        bar_heights: &[f32],
        slots: &Slots,
        label: &Fade,
        style: BodyStyle,
    ) -> Option<IntRect> {
        // Everything the last frame drew goes, which leaves the whole hi-res
        // buffer transparent — what `render::draw` asks of it.
        if let Some(r) = self.drawn {
            clear(&mut self.hires, scaled(r, SUPERSAMPLE));
        }
        let bounds = render::draw(
            &mut self.hires,
            scale * SUPERSAMPLE as f32,
            geom,
            bar_heights,
            slots,
            label,
            style,
        );
        let now = bounds.and_then(|b| {
            let ss = SUPERSAMPLE as f32;
            IntRect::from_ltrb(
                ((b.left() / ss).floor() as i32).max(0),
                ((b.top() / ss).floor() as i32).max(0),
                ((b.right() / ss).ceil() as i32).min(self.width() as i32),
                ((b.bottom() / ss).ceil() as i32).min(self.height() as i32),
            )
        });
        let damage = if std::mem::take(&mut self.fresh) {
            IntRect::from_xywh(0, 0, self.width(), self.height())
        } else {
            union(self.drawn, now)
        };
        self.drawn = now;
        let damage = damage?;
        halve(&self.hires, &mut self.mid, scaled(damage, 2));
        halve(&self.mid, &mut self.out, damage);
        Some(damage)
    }
}

/// `r`, in a buffer `k` times the size.
fn scaled(r: IntRect, k: u32) -> IntRect {
    let k32 = k as i32;
    IntRect::from_xywh(r.x() * k32, r.y() * k32, r.width() * k, r.height() * k)
        .expect("a non-empty rect scaled up is non-empty")
}

fn union(a: Option<IntRect>, b: Option<IntRect>) -> Option<IntRect> {
    match (a, b) {
        (Some(a), Some(b)) => IntRect::from_ltrb(
            a.left().min(b.left()),
            a.top().min(b.top()),
            a.right().max(b.right()),
            a.bottom().max(b.bottom()),
        ),
        (a, b) => a.or(b),
    }
}

/// Make `r` transparent. A plain store rather than a paint: there is nothing
/// to blend, and it is most of what a frame does to memory.
fn clear(pm: &mut Pixmap, r: IntRect) {
    let w = pm.width() as usize;
    let (x0, x1) = (r.left() as usize, r.right() as usize);
    let px = pm.pixels_mut();
    for y in r.top() as usize..r.bottom() as usize {
        px[y * w + x0..y * w + x1].fill(PremultipliedColorU8::TRANSPARENT);
    }
}

/// Fill `dst`'s `r` with `src` at half size — a 2x2 box average, since every
/// destination pixel samples exactly between four source ones.
///
/// This is `Pixmap::draw_pixmap(0, 0, src, bilinear, scale(0.5))` asked about
/// `r` alone: that call is this same pattern shader filling the source's whole
/// rect, so the pixels inside `r` are the bytes it would have written.
fn halve(src: &Pixmap, dst: &mut Pixmap, r: IntRect) {
    clear(dst, r);
    let paint = Paint {
        shader: Pattern::new(
            src.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Bilinear,
            1.0,
            Transform::identity(),
        ),
        ..Default::default()
    };
    dst.fill_rect(
        scaled(r, 2).to_rect(),
        &paint,
        Transform::from_scale(0.5, 0.5),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pill::bodies::{ISLANDS, UNIFIED};
    use crate::pill::core::{Origin, PillMode, BUTTON_COUNT};
    use crate::pill::geom::{Slot, ENVELOPE_H, ENVELOPE_W};

    const NO_SLOTS: Slots = [Slot {
        hover: 0.0,
        enabled: true,
    }; BUTTON_COUNT];

    const NO_LABEL: Fade = Fade {
        from: None,
        to: None,
        t: 1.0,
    };

    const REC: PillMode = PillMode::Recording {
        origin: Origin::Hotkey,
    };
    const CLICK_REC: PillMode = PillMode::Recording {
        origin: Origin::Click,
    };

    /// One frame through the whole-surface chain, as `PillWindow` ran it
    /// before #106 — the reference every damaged frame has to match byte for
    /// byte.
    fn whole(w: u32, h: u32, scale: f32, f: &Input) -> Pixmap {
        let mut hires = Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE).unwrap();
        render::draw(
            &mut hires,
            scale * SUPERSAMPLE as f32,
            &f.geom,
            &f.bars,
            &f.slots,
            &f.label,
            f.style,
        );
        let paint = tiny_skia::PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..Default::default()
        };
        let half = Transform::from_scale(0.5, 0.5);
        let mut mid = Pixmap::new(w * 2, h * 2).unwrap();
        mid.draw_pixmap(0, 0, hires.as_ref(), &paint, half, None);
        let mut out = Pixmap::new(w, h).unwrap();
        out.draw_pixmap(0, 0, mid.as_ref(), &paint, half, None);
        out
    }

    struct Input {
        geom: Geom,
        bars: Vec<f32>,
        slots: Slots,
        label: Fade,
        style: BodyStyle,
    }

    fn plain(geom: Geom, style: BodyStyle) -> Input {
        Input {
            geom,
            bars: vec![0.0; crate::pill::BAR_COUNT],
            slots: NO_SLOTS,
            label: NO_LABEL,
            style,
        }
    }

    /// Every leg a frame sequence can take — growing, shrinking, sliding
    /// sideways, fading out to nothing and coming back — so each frame's
    /// damage has to cover both what appeared and what went.
    fn sequence() -> Vec<Input> {
        let legs = [
            (PillMode::Hidden, PillMode::Idle, BodyStyle::Islands),
            (PillMode::Idle, REC, BodyStyle::Islands),
            (REC, PillMode::Idle, BodyStyle::Islands),
            (PillMode::Idle, ISLANDS, BodyStyle::Islands),
            (ISLANDS, CLICK_REC, BodyStyle::Islands),
            (CLICK_REC, PillMode::Idle, BodyStyle::Islands),
            (PillMode::Idle, UNIFIED, BodyStyle::Unified),
            (UNIFIED, PillMode::Hidden, BodyStyle::Unified),
        ];
        let mut frames = Vec::new();
        for (from, to, style) in legs {
            for step in 0..=6 {
                let t = step as f32 / 6.0;
                let mut f = plain(Geom::of(from).lerp(Geom::of(to), t), style);
                f.bars = (0..crate::pill::BAR_COUNT)
                    .map(|b| ((b + step) % 5) as f32 / 4.0)
                    .collect();
                if matches!(to, PillMode::Expanded { .. }) {
                    f.slots[step % BUTTON_COUNT].hover = t;
                    f.label = Fade {
                        from: Some("Copy last transcript"),
                        to: Some("Settings"),
                        t,
                    };
                }
                frames.push(f);
            }
        }
        // A frame with nothing on it, twice: the first clears what the last
        // leg left, the second has nothing to do.
        frames.push(plain(Geom::of(PillMode::Hidden), BodyStyle::Islands));
        frames.push(plain(Geom::of(PillMode::Hidden), BodyStyle::Islands));
        frames
    }

    /// **The damaged chain is the whole chain.** Frame after frame, at every
    /// scale the pill ships at, the pixels match the whole-surface render
    /// exactly — not near enough, the same bytes — so the saving is free.
    #[test]
    fn every_frame_matches_the_whole_surface_chain_exactly() {
        for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            let w = (ENVELOPE_W as f32 * scale).round() as u32;
            let h = (ENVELOPE_H as f32 * scale).round() as u32;
            let mut surface = Surface::new(w, h).unwrap();
            for (i, f) in sequence().iter().enumerate() {
                surface.draw(scale, &f.geom, &f.bars, &f.slots, &f.label, f.style);
                let want = whole(w, h, scale, f);
                assert!(
                    surface.pixels().data() == want.data(),
                    "frame {i} at {scale}x differs from the whole-surface chain"
                );
            }
        }
    }

    /// Damage is what a presenter copies, so it has to cover every pixel that
    /// changed since the frame before — checked against the pixels themselves.
    #[test]
    fn the_damage_covers_every_changed_pixel() {
        let scale = 1.5;
        let (w, h) = (390, 120);
        let mut surface = Surface::new(w, h).unwrap();
        let mut before = Pixmap::new(w, h).unwrap();
        for (i, f) in sequence().iter().enumerate() {
            let damage = surface.draw(scale, &f.geom, &f.bars, &f.slots, &f.label, f.style);
            let now = surface.pixels();
            for y in 0..h {
                for x in 0..w {
                    if now.pixel(x, y) == before.pixel(x, y) {
                        continue;
                    }
                    let (x, y) = (x as i32, y as i32);
                    assert!(
                        damage.is_some_and(|d| x >= d.left()
                            && x < d.right()
                            && y >= d.top()
                            && y < d.bottom()),
                        "frame {i}: ({x}, {y}) changed outside {damage:?}"
                    );
                }
            }
            before = now.clone();
        }
    }

    /// The first frame damages the whole surface — whatever it is presented
    /// into has never been written, so a smaller rect would leave the rest of
    /// it as it came.
    #[test]
    fn the_first_frame_damages_everything() {
        let mut surface = Surface::new(ENVELOPE_W, ENVELOPE_H).unwrap();
        let f = plain(Geom::of(REC), BodyStyle::Islands);
        let d = surface.draw(1.0, &f.geom, &f.bars, &f.slots, &f.label, f.style);
        assert_eq!(
            d,
            IntRect::from_xywh(0, 0, ENVELOPE_W, ENVELOPE_H),
            "the first frame's damage"
        );
    }

    /// And after that, a recording frame damages the pill and not the
    /// envelope — which is the whole saving.
    #[test]
    fn a_recording_frame_damages_the_pill_not_the_envelope() {
        let mut surface = Surface::new(ENVELOPE_W, ENVELOPE_H).unwrap();
        let f = plain(Geom::of(REC), BodyStyle::Islands);
        surface.draw(1.0, &f.geom, &f.bars, &f.slots, &f.label, f.style);
        let d = surface
            .draw(1.0, &f.geom, &f.bars, &f.slots, &f.label, f.style)
            .unwrap();
        let g = Geom::of(REC);
        assert!(d.width() as f32 <= g.w + 4.0, "{d:?}");
        assert!(d.height() as f32 <= g.h + 4.0, "{d:?}");
    }

    /// A surface with nothing on it and nothing to clear changes nothing.
    #[test]
    fn a_blank_frame_after_a_blank_frame_damages_nothing() {
        let mut surface = Surface::new(ENVELOPE_W, ENVELOPE_H).unwrap();
        let f = plain(Geom::of(PillMode::Hidden), BodyStyle::Islands);
        surface.draw(1.0, &f.geom, &f.bars, &f.slots, &f.label, f.style);
        assert_eq!(
            surface.draw(1.0, &f.geom, &f.bars, &f.slots, &f.label, f.style),
            None
        );
    }
}
