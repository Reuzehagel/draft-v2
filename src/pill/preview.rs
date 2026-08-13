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
    breathe, Geom, Motion, Tween, ENVELOPE_H, ENVELOPE_W, REVEAL, TO_IDLE, TO_RECORDING,
};
use crate::pill::render::draw;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tiny_skia::{Pixmap, PremultipliedColorU8};

/// Nearest-neighbour zoom. Nearest rather than smooth on purpose: this is for
/// judging a hairline and an anti-aliased corner, and a resample would show a
/// blur the pill does not have.
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
    const SS: u32 = 4;
    let mut hi = Pixmap::new(ENVELOPE_W * SS, ENVELOPE_H * SS).unwrap();
    draw(&mut hi, SS as f32, geom, bars);
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
