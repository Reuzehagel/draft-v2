// What one animated frame costs (#106).
//
// A settled pill asks for no frames at all, so its cost is nothing; this is
// about the other case — every frame of every recording, and every frame of a
// morph. It times one frame end to end, as far as pixels go: the draw at the
// supersample, the resolve to device pixels, and the conversion into the
// layered surface's BGRA. What it does not time is `UpdateLayeredWindow`
// itself, which needs a window and is the system's cost rather than ours.
//
// `#[ignore]`d, like `pill::preview`: it asserts nothing and its numbers only
// mean something in an optimised build. Run it deliberately:
//
//     cargo test --release -- --ignored pill::bench --nocapture
//
// The allocations it counts are almost all tiny-skia's: its rasterizer builds
// an edge list and coverage runs per anti-aliased fill (3 to 5 allocations
// each), and a stroke builds its outline as a path. The rest is `render`
// building each shape's path, two allocations apiece — a microsecond of a
// frame measured in hundreds, and not worth threading a pooled builder through
// every draw call for. What feeds the renderer allocates nothing: the meter,
// the row and the frame the window keeps — `a_recording_frame_allocates_nothing`
// in `pill::adapter` holds that line. The count is here so a regression in any
// of it shows up as a number.

#![cfg(test)]

use crate::alloc_count;
use crate::pill::bodies::ISLANDS;
use crate::pill::core::{BodyStyle, Origin, PillMode, BUTTON_COUNT};
use crate::pill::geom::{Geom, Slot, Slots, ENVELOPE_H, ENVELOPE_W};
use crate::pill::label::Fade;
use crate::pill::render::pixmap_to_premul_bgra;
use crate::pill::surface::Surface;
use std::time::{Duration, Instant};

/// Frames timed per case, after [`WARMUP`] untimed ones fill the caches.
const FRAMES: usize = 600;
const WARMUP: usize = 30;

/// The chain as `PillWindow` runs it, frame for frame: the surface, and the
/// layered window's buffer the damage is copied into.
struct Chain {
    surface: Surface,
    dib: Vec<u8>,
}

impl Chain {
    fn new(scale: f32) -> Self {
        let (w, h) = (
            (ENVELOPE_W as f32 * scale).round() as u32,
            (ENVELOPE_H as f32 * scale).round() as u32,
        );
        Self {
            surface: Surface::new(w, h).unwrap(),
            dib: vec![0; (w * h * 4) as usize],
        }
    }

    fn frame(&mut self, scale: f32, f: &FrameIn) {
        let damage = self
            .surface
            .draw(scale, &f.geom, &f.bars, &f.slots, &f.label, f.style);
        if let Some(r) = damage {
            pixmap_to_premul_bgra(self.surface.pixels(), &mut self.dib, r);
        }
    }
}

struct FrameIn {
    geom: Geom,
    bars: Vec<f32>,
    slots: Slots,
    label: Fade,
    style: BodyStyle,
}

/// A recording frame, with the waveform moving: what the pill draws ~30 times a
/// second for as long as anyone is talking.
fn recording(i: usize) -> FrameIn {
    let bars = (0..crate::pill::BAR_COUNT)
        .map(|b| (((i + b * 3) % 17) as f32 / 16.0).clamp(0.0, 1.0))
        .collect();
    FrameIn {
        geom: Geom::of(PillMode::Recording {
            origin: Origin::Hotkey,
        }),
        bars,
        slots: [Slot {
            hover: 0.0,
            enabled: true,
        }; BUTTON_COUNT],
        label: Fade {
            from: None,
            to: None,
            t: 1.0,
        },
        style: BodyStyle::Islands,
    }
}

/// The busiest frame the pill draws: the button bar, a button lit, and the
/// label crossfading above it.
fn bar_with_label(i: usize) -> FrameIn {
    let mut slots = [Slot {
        hover: 0.0,
        enabled: true,
    }; BUTTON_COUNT];
    slots[0].hover = (i % 10) as f32 / 10.0;
    FrameIn {
        geom: Geom::of(ISLANDS),
        bars: Vec::new(),
        slots,
        label: Fade {
            from: Some("Copy last transcript"),
            to: Some("Settings"),
            t: (i % 10) as f32 / 10.0,
        },
        style: BodyStyle::Islands,
    }
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored pill::bench --nocapture"]
fn bench_frame() {
    type Case = (&'static str, fn(usize) -> FrameIn);
    let cases: [Case; 2] = [("recording", recording), ("bar+label", bar_with_label)];
    println!();
    println!("case        scale  surface    median    mean      allocs/frame");
    for (name, make) in cases {
        for scale in [1.0f32, 1.5] {
            let mut chain = Chain::new(scale);
            let inputs: Vec<FrameIn> = (0..FRAMES + WARMUP).map(make).collect();
            for f in &inputs[..WARMUP] {
                chain.frame(scale, f);
            }
            let mut times = Vec::with_capacity(FRAMES);
            let mut allocs = 0;
            let start = Instant::now();
            for f in &inputs[WARMUP..] {
                let t = Instant::now();
                let ((), n) = alloc_count::during(|| chain.frame(scale, f));
                times.push(t.elapsed());
                allocs += n;
            }
            let mean = start.elapsed() / FRAMES as u32;
            println!(
                "{name:<11} {scale:<5}  {:>4}x{:<4}  {:>7.3}ms {:>7.3}ms {:>6.1}",
                chain.surface.width(),
                chain.surface.height(),
                median(times).as_secs_f64() * 1e3,
                mean.as_secs_f64() * 1e3,
                allocs as f64 / FRAMES as f64,
            );
        }
    }
}
