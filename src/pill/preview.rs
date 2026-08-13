// Renders the pill to PNGs, for looking at.
//
// The pill is judged by eye — #17, #18 and #27 were all settled by staring at
// real frames, and #41's appearance was reviewed the same way. That normally
// means building, launching, and holding a hotkey, which is slow, needs the
// user's own Draft closed, and cannot show a transition at all: the interesting
// 140 ms is over before you can look at it.
//
// These write the frames straight to disk instead. Every mode over a light and
// a dark desktop, and every transition as a filmstrip — the same `Geom`s and
// `Motion`s the real adapter derives, through the same renderer, so what comes
// out is what ships.
//
// They are `#[ignore]`d: they assert nothing, and a `cargo test` run should not
// be writing files. Run them deliberately:
//
//     cargo test --  --ignored pill::preview
//
// and look in `target/pill-preview/`. What they do NOT show is timing — how
// 140 ms *feels*, and whether the breath reads as breathing rather than
// blinking, are still questions for the running app.

#![cfg(test)]

use crate::pill::core::{Origin, PillMode};
use crate::pill::geom::{
    breathe, Geom, Motion, Slot, Slots, Tween, ENVELOPE_H, ENVELOPE_W, HOVER_IN, HOVER_OUT, REVEAL,
    TO_IDLE, TO_RECORDING,
};
use crate::pill::label::{Fade, Label, COPIED};
use crate::pill::render::draw;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tiny_skia::{Pixmap, PremultipliedColorU8};

/// Nearest-neighbour zoom. Nearest rather than smooth on purpose: this is for
/// judging a hairline and an anti-aliased corner, and a resample would show a
/// blur the pill does not have.
///
/// Held at 6 through #46's envelope growth: the sheets are several megapixels
/// each now, and they are disposable files under `target/` — trading the
/// fidelity this tool exists for against their size would be the wrong way
/// round.
const ZOOM: u32 = 6;
/// Space around each pill, in pre-zoom pixels.
const PAD: u32 = 10;
/// Frames per filmstrip, evenly spaced across the transition's duration.
const STEPS: u32 = 7;

/// A believable mid-speech waveform, fixed rather than random so two runs are
/// comparable.
const LIVE: [f32; 7] = [0.35, 0.8, 0.55, 1.0, 0.45, 0.7, 0.25];
/// The flat row every state but recording draws.
const FLAT: [f32; 7] = [0.0; 7];

/// A light desktop and a black one — the two backgrounds the body and the
/// hairline have to survive. A dark body is invisible on the right at any fill
/// alpha; the hairline is what separates it (#18 round 2).
const LIGHT_DESKTOP: (u8, u8, u8) = (238, 238, 240);
const DARK_DESKTOP: (u8, u8, u8) = (8, 8, 10);
/// The filmstrips' backdrop: neutral mid-dark, so both a bright hairline and a
/// green flash read against it.
const STRIP_BG: (u8, u8, u8) = (24, 24, 28);

/// Every button live and none of them lit — the bar at rest, and what the modes
/// that draw no bar at all are handed.
const NO_SLOTS: Slots = [Slot {
    hover: 0.0,
    enabled: true,
}; crate::pill::core::BUTTON_COUNT];

fn out_dir() -> PathBuf {
    // Under `target/` so it is git-ignored and `cargo clean` takes it.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("pill-preview");
    std::fs::create_dir_all(&dir).expect("create preview dir");
    dir
}

fn recording() -> PillMode {
    PillMode::Recording {
        origin: Origin::Hotkey,
    }
}

/// One frame at device resolution, through the same supersample-and-halve chain
/// `PillWindow` uses — so the anti-aliasing here is the anti-aliasing on screen,
/// not tiny-skia's raw output at 1x.
fn frame(geom: &Geom, bars: &[f32]) -> Pixmap {
    frame_with(geom, bars, &NO_SLOTS)
}

/// No hover and no acknowledgement — the label's usual state, and what every
/// sheet but `preview_labels` shows.
const NO_LABEL: Fade = Fade {
    from: None,
    to: None,
    t: 1.0,
};

/// The same, with per-button hover state — for the expanded bar, where which
/// button is lit is not part of the Geom.
fn frame_with(geom: &Geom, bars: &[f32], slots: &Slots) -> Pixmap {
    labelled(geom, bars, slots, &NO_LABEL)
}

/// The same again, with the label saying something — the one sheet where it is
/// the subject rather than the absence.
fn labelled(geom: &Geom, bars: &[f32], slots: &Slots, label: &Fade) -> Pixmap {
    const SS: u32 = 4;
    let mut hi = Pixmap::new(ENVELOPE_W * SS, ENVELOPE_H * SS).unwrap();
    draw(&mut hi, SS as f32, geom, bars, slots, label);
    let mut out = Pixmap::new(ENVELOPE_W, ENVELOPE_H).unwrap();
    out.draw_pixmap(
        0,
        0,
        hi.as_ref(),
        &tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        },
        tiny_skia::Transform::from_scale(1.0 / SS as f32, 1.0 / SS as f32),
        None,
    );
    out
}

/// A canvas of `cols` x `rows` cells, flooded with `bg`.
fn canvas(cols: u32, rows: u32, cell: (u32, u32), bg: (u8, u8, u8)) -> Pixmap {
    let mut pm = Pixmap::new(cols * cell.0, rows * cell.1).unwrap();
    let c = opaque(bg);
    for p in pm.pixels_mut() {
        *p = c;
    }
    pm
}

fn opaque((r, g, b): (u8, u8, u8)) -> PremultipliedColorU8 {
    PremultipliedColorU8::from_rgba(r, g, b, 255).unwrap()
}

/// Composite `pill` over `bg` and zoom it into `dst` at (`ox`, `oy`).
///
/// `pill` is premultiplied, so the source-over is `src + dst * (1 - a)` with no
/// un-premultiply step — the same arithmetic `UpdateLayeredWindow` does with
/// AC_SRC_ALPHA, which is why this preview matches the screen.
fn blit(dst: &mut Pixmap, pill: &Pixmap, ox: u32, oy: u32, bg: (u8, u8, u8)) {
    let (dw, dh) = (dst.width(), dst.height());
    for y in 0..pill.height() {
        for x in 0..pill.width() {
            let p = pill.pixel(x, y).unwrap();
            let inv = 255 - p.alpha() as u32;
            let over = |s: u8, b: u8| ((s as u32) + (b as u32) * inv / 255).min(255) as u8;
            let c = opaque((
                over(p.red(), bg.0),
                over(p.green(), bg.1),
                over(p.blue(), bg.2),
            ));
            for zy in 0..ZOOM {
                for zx in 0..ZOOM {
                    let (dx, dy) = (ox + x * ZOOM + zx, oy + y * ZOOM + zy);
                    if dx < dw && dy < dh {
                        dst.pixels_mut()[(dy * dw + dx) as usize] = c;
                    }
                }
            }
        }
    }
}

fn cell() -> (u32, u32) {
    (ENVELOPE_W * ZOOM + PAD * 2, ENVELOPE_H * ZOOM + PAD * 2)
}

/// Lay `frames` out as one row per strip, and write the PNG.
fn write(name: &str, bg: (u8, u8, u8), rows: Vec<Vec<Pixmap>>) {
    let cell = cell();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let mut out = canvas(cols, rows.len() as u32, cell, bg);
    for (r, row) in rows.iter().enumerate() {
        for (c, pill) in row.iter().enumerate() {
            let (x, y) = (c as u32 * cell.0 + PAD, r as u32 * cell.1 + PAD);
            blit(&mut out, pill, x, y, bg);
        }
    }
    let path = out_dir().join(name);
    out.save_png(&path).expect("write preview png");
    println!("wrote {}", path.display());
}

/// Every frame of a transition, evenly spaced across its duration — including
/// both endpoints, so the first and last cells are the modes themselves.
fn filmstrip(from: PillMode, to: PillMode, tween: Tween, bars: &[f32]) -> Vec<Pixmap> {
    let t0 = Instant::now();
    let motion = Motion::start(Geom::of(from), to, tween, t0);
    (0..STEPS)
        .map(|s| {
            let at = t0 + tween.dur.mul_f32(s as f32 / (STEPS - 1) as f32);
            frame(&motion.at(at), bars)
        })
        .collect()
}

/// Every mode, over a light desktop and a black one.
///
/// Rows, top to bottom: Idle (the nub), Recording, Processing, Done delivered,
/// Done failed. Columns: light desktop, black desktop.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_modes() {
    let now = Instant::now();
    let modes: [(PillMode, &[f32]); 5] = [
        (PillMode::Idle, &FLAT),
        (recording(), &LIVE),
        (PillMode::Processing { since: now }, &FLAT),
        (
            PillMode::Done {
                ok: true,
                since: now,
            },
            &FLAT,
        ),
        (
            PillMode::Done {
                ok: false,
                since: now,
            },
            &FLAT,
        ),
    ];
    let cell = cell();
    let mut out = canvas(2, modes.len() as u32, cell, LIGHT_DESKTOP);
    // The right half is the black desktop; flood it before the pills go down.
    for y in 0..out.height() {
        for x in cell.0..out.width() {
            let (w, c) = (out.width(), opaque(DARK_DESKTOP));
            out.pixels_mut()[(y * w + x) as usize] = c;
        }
    }
    for (r, (mode, bars)) in modes.iter().enumerate() {
        let pill = frame(&Geom::of(*mode), bars);
        let y = r as u32 * cell.1 + PAD;
        blit(&mut out, &pill, PAD, y, LIGHT_DESKTOP);
        blit(&mut out, &pill, cell.0 + PAD, y, DARK_DESKTOP);
    }
    let path = out_dir().join("pill-modes.png");
    out.save_png(&path).expect("write preview png");
    println!("wrote {}", path.display());
}

/// The transitions residency introduces, as filmstrips.
///
/// Rows, top to bottom: the reveal (nothing → nub), Idle → Recording, and the
/// flash resolving home (Done → Idle). Left to right is time, evenly spaced
/// across each transition's own duration — so the rows are *not* on a shared
/// clock, and a wider row is not a slower one.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_morphs() {
    let done = PillMode::Done {
        ok: true,
        since: Instant::now(),
    };
    write(
        "pill-morphs.png",
        STRIP_BG,
        vec![
            filmstrip(PillMode::Hidden, PillMode::Idle, REVEAL, &FLAT),
            filmstrip(PillMode::Idle, recording(), TO_RECORDING, &LIVE),
            filmstrip(done, PillMode::Idle, TO_IDLE, &FLAT),
        ],
    );
}

/// The exits: a resident pill concealing, and a session-only one.
///
/// The point of the pair is that the bottom row does *not* shrink. A conceal
/// fades what is on screen; aiming it at `Geom::of(Hidden)` — the nub at alpha
/// 0 — would morph a 62x28 flash into a nub on its way out, a shape that user
/// has never seen.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_conceals() {
    let done = PillMode::Done {
        ok: false,
        since: Instant::now(),
    };
    write(
        "pill-conceals.png",
        STRIP_BG,
        vec![
            filmstrip(
                PillMode::Idle,
                PillMode::Hidden,
                crate::pill::geom::CONCEAL,
                &FLAT,
            ),
            filmstrip(done, PillMode::Hidden, crate::pill::geom::CONCEAL, &FLAT),
        ],
    );
}

/// The button bar: nothing hovered, each button hovered in turn, and Copy
/// disabled — the four states the expanded pill can be settled in.
///
/// Over a light desktop and a black one, because the bare desktop *between* the
/// islands is part of the design: there is no enclosing body, so what shows
/// between them is whatever is behind the pill.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_buttons() {
    let live = Slot {
        hover: 0.0,
        enabled: true,
    };
    let hovering = |i: usize| -> Slots {
        std::array::from_fn(|s| Slot {
            hover: if s == i { 1.0 } else { 0.0 },
            ..live
        })
    };
    let rows: Vec<Slots> = vec![
        NO_SLOTS,
        hovering(0),
        hovering(1),
        hovering(2),
        // Nothing recorded yet: Copy is faint, and its slab is inert.
        std::array::from_fn(|s| Slot {
            enabled: s != 0,
            ..live
        }),
    ];
    let expanded = Geom::of(PillMode::Expanded);
    let cell = cell();
    let mut out = canvas(2, rows.len() as u32, cell, LIGHT_DESKTOP);
    for y in 0..out.height() {
        for x in cell.0..out.width() {
            let (w, c) = (out.width(), opaque(DARK_DESKTOP));
            out.pixels_mut()[(y * w + x) as usize] = c;
        }
    }
    for (r, slots) in rows.iter().enumerate() {
        let pill = frame_with(&expanded, &FLAT, slots);
        let y = r as u32 * cell.1 + PAD;
        blit(&mut out, &pill, PAD, y, LIGHT_DESKTOP);
        blit(&mut out, &pill, cell.0 + PAD, y, DARK_DESKTOP);
    }
    let path = out_dir().join("pill-buttons.png");
    out.save_png(&path).expect("write preview png");
    println!("wrote {}", path.display());
}

/// The label, over a light desktop and a black one.
///
/// Rows: each button's name in turn, and "Copied". The pair of backgrounds is
/// the point — the label floats over bare desktop with no pill under it, so its
/// chip is carrying the whole of its legibility, exactly as the pill's body
/// does for the glyphs.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_labels() {
    let now = Instant::now();
    // Through the real state machine, settled: what a hover that has finished
    // fading actually produces, rather than a `Fade` written out by hand.
    let settled = |set: &dyn Fn(&mut Label)| -> (Fade, Slots) {
        let mut l = Label::new(now);
        set(&mut l);
        (l.at(now + Duration::from_millis(200)), NO_SLOTS)
    };
    let lit = |i: usize| -> Slots {
        std::array::from_fn(|s| Slot {
            hover: if s == i { 1.0 } else { 0.0 },
            enabled: true,
        })
    };
    let rows: Vec<(Fade, Slots)> = (0..crate::pill::core::BUTTON_COUNT)
        .map(|i| {
            let (fade, _) = settled(&|l: &mut Label| {
                l.set_hover(Some(i), now);
            });
            (fade, lit(i))
        })
        .chain(std::iter::once(settled(&|l: &mut Label| {
            l.set_hover(Some(0), now);
            l.flash(COPIED, now);
        })))
        .collect();

    let expanded = Geom::of(PillMode::Expanded);
    let cell = cell();
    let mut out = canvas(2, rows.len() as u32, cell, LIGHT_DESKTOP);
    for y in 0..out.height() {
        for x in cell.0..out.width() {
            let (w, c) = (out.width(), opaque(DARK_DESKTOP));
            out.pixels_mut()[(y * w + x) as usize] = c;
        }
    }
    for (r, (fade, slots)) in rows.iter().enumerate() {
        let pill = labelled(&expanded, &FLAT, slots, fade);
        let y = r as u32 * cell.1 + PAD;
        blit(&mut out, &pill, PAD, y, LIGHT_DESKTOP);
        blit(&mut out, &pill, cell.0 + PAD, y, DARK_DESKTOP);
    }
    let path = out_dir().join("pill-labels.png");
    out.save_png(&path).expect("write preview png");
    println!("wrote {}", path.display());
}

/// One text changing into another, as a filmstrip: the chip's width lerps
/// between the two while the words cross inside it. The point of the row is
/// that it is *one* chip throughout — never two dissolving through each other.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_label_crossfade() {
    let t0 = Instant::now();
    let strip = |set: &dyn Fn(&mut Label)| -> Vec<Pixmap> {
        let mut l = Label::new(t0);
        set(&mut l);
        let expanded = Geom::of(PillMode::Expanded);
        (0..STEPS)
            .map(|s| {
                let at = t0
                    + crate::pill::label::LABEL_FADE
                        .dur
                        .mul_f32(s as f32 / (STEPS - 1) as f32);
                labelled(&expanded, &FLAT, &NO_SLOTS, &l.at(at))
            })
            .collect()
    };
    write(
        "pill-label-crossfade.png",
        STRIP_BG,
        vec![
            // Arriving from nothing: one layer, fading in at its settled width.
            strip(&|l: &mut Label| {
                l.set_hover(Some(2), t0);
            }),
            // Settings → Copy last transcript, the widest change the bar makes.
            strip(&|l: &mut Label| {
                l.set_hover(Some(2), t0 - Duration::from_millis(500));
                l.set_hover(Some(0), t0);
            }),
        ],
    );
}

/// A held "Copied" while the bar collapses out from under it.
///
/// The commonest thing a user does after clicking Copy is move the cursor away,
/// so this is what the acknowledgement mostly gets seen against. The chip's gap
/// above the pill comes off the `Geom`'s own height, so it rides the collapse
/// down as part of the same lerp rather than holding the bar's place over a
/// nub with bare desktop between them.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_label_over_a_collapsing_pill() {
    let t0 = Instant::now();
    let mut label = Label::new(t0);
    label.flash(COPIED, t0);
    let motion = Motion::start(Geom::of(PillMode::Expanded), PillMode::Idle, HOVER_OUT, t0);
    let strip: Vec<Pixmap> = (0..STEPS)
        .map(|s| {
            let at = t0 + HOVER_OUT.dur.mul_f32(s as f32 / (STEPS - 1) as f32);
            labelled(&motion.at(at), &FLAT, &NO_SLOTS, &label.at(at))
        })
        .collect();
    write("pill-label-collapse.png", STRIP_BG, vec![strip]);
}

/// The fold-out and the collapse: the flankers sliding out from behind Dictate
/// and back. The point of the pair is that neither staggers — every button's
/// offset is the same progress value.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_expansion() {
    write(
        "pill-expansion.png",
        STRIP_BG,
        vec![
            filmstrip(PillMode::Idle, PillMode::Expanded, HOVER_IN, &FLAT),
            filmstrip(PillMode::Expanded, PillMode::Idle, HOVER_OUT, &FLAT),
        ],
    );
}

/// One full cycle of the Processing breath, and the same cycle mid-handoff.
///
/// The bottom row is where the floor earns its keep: the border is still
/// crossfading from the hairline, and the breath must not take it *below* the
/// hairline on the way — the pill has to stay findable at every frame.
#[test]
#[ignore = "writes PNGs for eyeballing; run with --ignored"]
fn preview_breath() {
    const CYCLE_MS: u64 = 1250; // ~one period at 0.8 Hz
    let t0 = Instant::now();
    let processing = PillMode::Processing { since: t0 };
    let settled = Geom::of(processing);
    let handoff = Motion::start(
        Geom::of(recording()),
        processing,
        crate::pill::geom::HANDOFF,
        t0,
    );
    let at = |s: u32| Duration::from_millis(CYCLE_MS * s as u64 / (STEPS - 1) as u64);
    write(
        "pill-breath.png",
        STRIP_BG,
        vec![
            (0..STEPS)
                .map(|s| frame(&breathe(settled, at(s)), &FLAT))
                .collect(),
            (0..STEPS)
                .map(|s| frame(&breathe(handoff.at(t0 + at(s)), at(s)), &LIVE))
                .collect(),
        ],
    );
}
