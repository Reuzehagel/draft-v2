// PROTOTYPE — THROWAWAY. Not production code, not wired into `draft`.
//
// Answers wayfinder ticket "How the pill expands and collapses" (#18): what
// morphs into what, over how long, on what easing, and which transitions
// animate versus snap.
//
// ROUND 2. Round 1 swept motion profiles and Expanded sizes against empty
// silhouettes. Four things came back:
//   - the near-black body vanishes on a black desktop; it wants a light hairline
//   - SNAPPY's durations with SPRINGY's overshoot — a profile round 1 didn't have
//   - Recording -> Processing reads as too abrupt
//   - the green/red flash is too faint to notice
//   - Expanded has to hold mic / copy / settings buttons, which E-REC cannot
// Round 2 turns each of those into an axis, and draws stub buttons inside the
// Expanded candidates so the morph is judged against a realistic target rather
// than an empty slab.
//
// Three further asks landed while it was being built, and they change the
// shape of the answer rather than adding a dial:
//   - the buttons are circles
//   - the button set is configurable, and Expanded *auto-sizes* to it. So
//     Expanded's width is no longer a candidate to pick — it is a formula,
//     and `e` sweeps the button set that feeds it.
//   - the mic must stay under the cursor across the whole expansion, so it can
//     always be clicked. That is a constraint on the *morph*, not on the
//     layout: it says what the expansion is anchored to. `a` sweeps CENTRE
//     (today — the pill grows symmetrically) against MIC (the pill slides as it
//     grows so the mic lands on the cursor). With three buttons and the mic in
//     the middle the two are identical; the sets where the mic is not central
//     are what tell them apart.
//
// Run:  cargo run --bin pill-transitions-proto
// Then, in the terminal:
//   m        next motion profile   (SNAPBACK / INSTANT / SNAPPY / SMOOTH / SPRINGY)
//   b        next hairline         (OFF / SOFT / CLEAR / BRIGHT)
//   p        next Recording->Processing handoff  (HARD / FADE / SETTLE / SLOW-SETTLE)
//   x        next flash treatment  (HAIRLINE / THICK / GLOW / WASH)
//   e        next button set — Expanded's width is *derived* from it, not chosen
//   a        next expansion anchor (CENTRE / MIC)
//   h        toggle live hover (cursor polling) on/off
//   1        replay  Idle -> Expanded -> Idle
//   2        replay  a whole session, residency ON   (-> Recording -> Processing -> Done -> Idle)
//   3        replay  Idle -> Hidden -> Idle          (fullscreen suppress / residency toggle)
//   4        replay  Expanded -> Recording -> ... -> Idle   (chord pressed mid-hover)
//   5        replay  a whole session, residency OFF  (Hidden -> ... -> Done -> Hidden)
//   f        flip the terminal flash between ok and failed
//   q        quit
//
// Judge it over a *black* window and a *white* one, and with the cursor
// actually moving — flicking past the nub is the case scripted playback can't
// show you.
//
// It copies (rather than imports) the layered-window plumbing from
// `src/pill/window.rs`, because the crate has no lib target. Delete this file
// once the decision is recorded.

#![cfg(windows)]

use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

// The window never resizes — every state is drawn *inside* one fixed box,
// bottom-aligned and centred, so the pill's bottom edge stays put at the same
// 80px margin through the whole morph. Sized for the widest Expanded candidate
// plus headroom for the overshoot easings.
const BOX_W: u32 = 200;
const BOX_H: u32 = 56;
const BOTTOM_MARGIN: u32 = 80;
const SUPERSAMPLE: u32 = 4;
const BAR_COUNT: usize = 7;

// Settled elsewhere on this map, and held fixed here.
//   Idle      — NUB-36 from #17: 36x10, fully rounded, no bars.
//   Recording — 62x28, the size promoted out of #17 and filed as #25/#26.
const IDLE_W: f32 = 36.0;
const IDLE_H: f32 = 10.0;
const IDLE_R: f32 = 5.0;
const IDLE_FILL_A: f32 = 140.0; // provisional — #27 owns this number, not us
const REC_W: f32 = 62.0;
const REC_H: f32 = 28.0;
const REC_R: f32 = 14.0;

const BODY: (f32, f32, f32) = (13.0, 13.0, 13.0);
const BORDER_PROCESSING: (f32, f32, f32) = (190.0, 192.0, 200.0);
const BORDER_SUCCESS: (f32, f32, f32) = (74.0, 188.0, 120.0);
const BORDER_ERROR: (f32, f32, f32) = (214.0, 96.0, 96.0);

// ---------------------------------------------------------------------------
// The axes being swept
// ---------------------------------------------------------------------------

/// ROUND 2 AXIS — the hairline. Round 1 rendered the body near-black with the
/// idle nub borderless, exactly as #17 settled it, and on a black desktop it
/// disappeared. Alpha is not the dial that fixes that: a darker body on a dark
/// background is still invisible at any alpha. A light *edge* is.
///
/// This contradicts #17's "no border" for the nub and reframes #27, which is
/// currently sweeping fill alpha. Both get told.
struct Hairline {
    name: &'static str,
    rgb: (f32, f32, f32),
    a: f32,
    note: &'static str,
}

const HAIRLINES: &[Hairline] = &[
    Hairline {
        name: "OFF",
        rgb: (170.0, 172.0, 178.0),
        a: 0.0,
        note: "round 1 / #17 as settled — the one that vanishes on black",
    },
    Hairline {
        name: "SOFT",
        rgb: (210.0, 214.0, 222.0),
        a: 70.0,
        note: "just enough edge to separate from a dark desktop",
    },
    Hairline {
        name: "CLEAR",
        rgb: (220.0, 224.0, 232.0),
        a: 120.0,
        note: "unambiguously an object on any background",
    },
    Hairline {
        name: "BRIGHT",
        rgb: (235.0, 238.0, 245.0),
        a: 180.0,
        note: "reads as outlined — check it isn't now loud on a white desktop",
    },
];

/// ROUND 2 AXIS — the Recording -> Processing handoff, reported as too abrupt.
///
/// Two things change at once there, and round 1 only animated one of them. The
/// border colour swaps, and the bars *stop dead* — they hold their last live
/// heights, so all motion ends on a single frame. The frozen heights are right;
/// the instant stop is the jolt. `bars_ms` decays the live waveform toward those
/// held heights so the motion settles instead of cutting.
struct Handoff {
    name: &'static str,
    colour_ms: u32,
    bars_ms: u32,
    note: &'static str,
}

const HANDOFFS: &[Handoff] = &[
    Handoff {
        name: "HARD",
        colour_ms: 0,
        bars_ms: 0,
        note: "round 1 / today — colour snaps, bars stop on one frame",
    },
    Handoff {
        name: "FADE",
        colour_ms: 180,
        bars_ms: 0,
        note: "colour crossfades, bars still stop dead — isolates which one jolts",
    },
    Handoff {
        name: "SETTLE",
        colour_ms: 180,
        bars_ms: 180,
        note: "the waveform decays to stillness as the colour crossfades",
    },
    Handoff {
        name: "SLOW-SETTLE",
        colour_ms: 320,
        bars_ms: 320,
        note: "same, drawn out — is a longer settle calmer or just laggy?",
    },
];

/// ROUND 2 AXIS — the terminal flash, reported as too faint. It is a 1px
/// hairline today, so this sweeps *treatment* rather than duration: the flash
/// has to register peripherally, since the user is looking at where the text
/// landed, not at the pill.
struct Flash {
    name: &'static str,
    border_w: f32,
    /// How far the body is tinted toward the flash colour, 0..1.
    tint: f32,
    note: &'static str,
}

const FLASHES: &[Flash] = &[
    Flash {
        name: "HAIRLINE",
        border_w: 1.0,
        tint: 0.0,
        note: "round 1 / today — the one you said you could barely see",
    },
    Flash {
        name: "THICK",
        border_w: 2.5,
        tint: 0.0,
        note: "same idea, more of it — cheapest possible fix",
    },
    Flash {
        name: "GLOW",
        border_w: 2.0,
        tint: 0.3,
        note: "thicker edge plus a hint of colour in the body",
    },
    Flash {
        name: "WASH",
        border_w: 1.5,
        tint: 0.65,
        note: "the whole pill goes green/red — impossible to miss, maybe too loud",
    },
];

// Expanded's geometry is *derived*, not chosen: circular buttons of a fixed
// size, evenly gapped, with fixed edge padding. Change the button set and the
// width follows. This is why round 1's E-REC / E-WIDE / E-BAR candidates are
// gone — picking a width was the wrong question.
const BTN_D: f32 = 22.0;
const BTN_GAP: f32 = 8.0;
const BTN_PAD: f32 = 11.0;
const EXP_H: f32 = 32.0;
const EXP_R: f32 = 16.0;

#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    Mic,
    Copy,
    Sliders,
    Clock,
}

/// The button set Expanded carries. `mic` is the index of the mic button, which
/// the MIC anchor keeps parked under the cursor.
struct ButtonSet {
    name: &'static str,
    glyphs: &'static [Glyph],
    mic: usize,
    note: &'static str,
}

const BUTTON_SETS: &[ButtonSet] = &[
    ButtonSet {
        name: "MIC-CENTRE (3)",
        glyphs: &[Glyph::Copy, Glyph::Mic, Glyph::Sliders],
        mic: 1,
        note: "mic in the middle — the one set where both anchors are identical",
    },
    ButtonSet {
        name: "MIC-FIRST (3)",
        glyphs: &[Glyph::Mic, Glyph::Copy, Glyph::Sliders],
        mic: 0,
        note: "same three, mic on the left — this is what separates the anchors",
    },
    ButtonSet {
        name: "MIC+COPY (2)",
        glyphs: &[Glyph::Mic, Glyph::Copy],
        mic: 0,
        note: "an even count, so nothing sits on the centre line",
    },
    ButtonSet {
        name: "FOUR (4)",
        glyphs: &[Glyph::Mic, Glyph::Copy, Glyph::Sliders, Glyph::Clock],
        mic: 0,
        note: "how far does auto-sizing stretch before the morph feels like a lot?",
    },
];

impl ButtonSet {
    fn n(&self) -> f32 {
        self.glyphs.len() as f32
    }

    /// The whole point of auto-sizing: width is arithmetic, not a judgement.
    fn width(&self) -> f32 {
        2.0 * BTN_PAD + self.n() * BTN_D + (self.n() - 1.0) * BTN_GAP
    }

    /// Horizontal distance from the pill's centre to the mic's centre.
    fn mic_offset(&self) -> f32 {
        (self.mic as f32 - (self.n() - 1.0) / 2.0) * (BTN_D + BTN_GAP)
    }
}

/// ROUND 2 AXIS — what the expansion is anchored to. The nub sits at screen
/// centre; the question is what ends up there once the pill has grown.
#[derive(Clone, Copy, PartialEq)]
enum Anchor {
    /// The pill grows symmetrically about the nub, as in round 1. Whatever
    /// button happens to be central lands under the cursor — which for an even
    /// count is nothing, and for a mic-first set is the wrong button.
    Centre,
    /// The pill slides as it grows so the *mic* lands on the nub's centre line.
    /// Costs the pill its screen-centred symmetry when expanded.
    Mic,
}

const ANCHORS: &[(Anchor, &str, &str)] = &[
    (
        Anchor::Mic,
        "MIC",
        "the mic stays under the cursor for any button set — the pill slides as it grows",
    ),
    (
        Anchor::Centre,
        "CENTRE",
        "round 1 — symmetric growth; the mic is only clickable if it happens to be central",
    ),
];

#[derive(Clone, Copy, PartialEq)]
enum Ease {
    /// No animation at all: the shape is simply the new one on the next frame.
    Snap,
    Linear,
    OutCubic,
    InOutCubic,
    /// Overshoots the target and settles back.
    OutBack,
}

impl Ease {
    fn name(self) -> &'static str {
        match self {
            Ease::Snap => "snap",
            Ease::Linear => "linear",
            Ease::OutCubic => "out-cubic",
            Ease::InOutCubic => "in-out-cubic",
            Ease::OutBack => "out-back",
        }
    }

    fn apply(self, t: f32) -> f32 {
        match self {
            Ease::Snap => 1.0,
            Ease::Linear => t,
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
            Ease::InOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Ease::OutBack => {
                let c1 = 1.70158;
                let c3 = c1 + 1.0;
                1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Tween {
    ms: u32,
    ease: Ease,
}

const fn tw(ms: u32, ease: Ease) -> Tween {
    Tween { ms, ease }
}

const SNAP: Tween = tw(0, Ease::Snap);

/// A complete answer to the ticket is one of these tables. Every transition the
/// Pill core can make gets its own duration and easing, so "which snap" is an
/// axis being swept rather than an assumption baked into the harness.
///
/// Recording -> Processing is absent on purpose — the handoff axis owns it.
struct Motion {
    name: &'static str,
    note: &'static str,
    /// Hidden -> Idle (residency on, or a fullscreen app losing focus)
    reveal: Tween,
    /// Idle -> Hidden (residency off, or a fullscreen app taking focus)
    conceal: Tween,
    /// Idle -> Expanded (cursor enters)
    hover_in: Tween,
    /// Expanded -> Idle (cursor leaves)
    hover_out: Tween,
    /// anything -> Recording (the chord goes down)
    to_recording: Tween,
    /// Processing -> Done (same geometry; only the border and tint change)
    to_done: Tween,
    /// Done -> Idle (the flash resolving back to the nub)
    to_idle: Tween,
}

const MOTIONS: &[Motion] = &[
    Motion {
        name: "SNAPBACK",
        note: "ROUND 2: SNAPPY's durations with SPRINGY's overshoot — what 'springy, snappy' sounded like",
        reveal: tw(150, Ease::OutBack),
        conceal: tw(120, Ease::OutCubic),
        hover_in: tw(130, Ease::OutBack),
        hover_out: tw(95, Ease::OutCubic),
        to_recording: tw(110, Ease::OutBack),
        to_done: tw(140, Ease::Linear),
        to_idle: tw(180, Ease::OutCubic),
    },
    Motion {
        name: "INSTANT",
        note: "the control — nothing animates. Does motion earn its place at all?",
        reveal: SNAP,
        conceal: SNAP,
        hover_in: SNAP,
        hover_out: SNAP,
        to_recording: SNAP,
        to_done: SNAP,
        to_idle: SNAP,
    },
    Motion {
        name: "SNAPPY",
        note: "short and decelerating, no overshoot",
        reveal: tw(140, Ease::OutCubic),
        conceal: tw(120, Ease::OutCubic),
        hover_in: tw(110, Ease::OutCubic),
        hover_out: tw(90, Ease::OutCubic),
        to_recording: tw(90, Ease::OutCubic),
        to_done: SNAP,
        to_idle: tw(160, Ease::OutCubic),
    },
    Motion {
        name: "SMOOTH",
        note: "longer, symmetric easing throughout",
        reveal: tw(220, Ease::InOutCubic),
        conceal: tw(200, Ease::InOutCubic),
        hover_in: tw(190, Ease::InOutCubic),
        hover_out: tw(170, Ease::InOutCubic),
        to_recording: tw(150, Ease::InOutCubic),
        to_done: tw(140, Ease::Linear),
        to_idle: tw(260, Ease::InOutCubic),
    },
    Motion {
        name: "SPRINGY",
        note: "big overshoot on growth; shrinking stays clean (a bouncing exit reads as a glitch)",
        reveal: tw(240, Ease::OutBack),
        conceal: tw(130, Ease::OutCubic),
        hover_in: tw(240, Ease::OutBack),
        hover_out: tw(130, Ease::OutCubic),
        to_recording: tw(200, Ease::OutBack),
        to_done: tw(140, Ease::Linear),
        to_idle: tw(300, Ease::OutCubic),
    },
];

// ---------------------------------------------------------------------------
// Pill modes and their geometry
// ---------------------------------------------------------------------------

/// The Pill mode set settled in #16 — what the Pill core derives from presence
/// x activity and hands the adapter.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Hidden,
    Idle,
    Expanded,
    Recording,
    Processing,
    Done,
}

/// Everything that can be interpolated. A transition is a lerp from one of
/// these to another — there is no per-mode bespoke animation code, which is
/// the point: if a state cannot be expressed here, the morph model is wrong.
#[derive(Clone, Copy)]
struct Geom {
    w: f32,
    h: f32,
    /// Offset of the shape's centre from the box's centre, in logical px. Zero
    /// for every state except a MIC-anchored Expanded — interpolating it is
    /// what makes the pill *slide* as it grows rather than jumping sideways.
    x_off: f32,
    radius: f32,
    fill_rgb: (f32, f32, f32),
    fill_a: f32,
    border_rgb: (f32, f32, f32),
    border_a: f32,
    /// Stroke width in logical px — an axis now that the flash sweeps it.
    border_w: f32,
    /// Opacity of the waveform bars, 0 = absent.
    bars: f32,
    /// Opacity of the mic / copy / settings buttons, 0 = absent.
    buttons: f32,
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
            x_off: lerp(self.x_off, to.x_off, t),
            radius: lerp(self.radius, to.radius, t),
            fill_rgb: lerp3(self.fill_rgb, to.fill_rgb, t),
            fill_a: lerp(self.fill_a, to.fill_a, t),
            border_rgb: lerp3(self.border_rgb, to.border_rgb, t),
            border_a: lerp(self.border_a, to.border_a, t),
            border_w: lerp(self.border_w, to.border_w, t),
            bars: lerp(self.bars, to.bars, t),
            buttons: lerp(self.buttons, to.buttons, t),
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

enum Msg {
    NextMotion,
    NextButtonSet,
    NextAnchor,
    NextHairline,
    NextHandoff,
    NextFlash,
    ToggleHover,
    Play(u8),
    FlipOutcome,
    Quit,
}

fn main() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line).is_err() {
                let _ = tx.send(Msg::Quit);
                return;
            }
            let msg = match line.trim() {
                "q" => Msg::Quit,
                "m" => Msg::NextMotion,
                "e" => Msg::NextButtonSet,
                "a" => Msg::NextAnchor,
                "b" => Msg::NextHairline,
                "p" => Msg::NextHandoff,
                "x" => Msg::NextFlash,
                "h" => Msg::ToggleHover,
                "f" => Msg::FlipOutcome,
                "1" => Msg::Play(1),
                "2" => Msg::Play(2),
                "3" => Msg::Play(3),
                "4" => Msg::Play(4),
                "5" => Msg::Play(5),
                _ => continue,
            };
            let quit = matches!(msg, Msg::Quit);
            if tx.send(msg).is_err() || quit {
                return;
            }
        }
    });

    let el = EventLoop::new()?;
    el.set_control_flow(ControlFlow::Poll);
    let start = Instant::now();
    let mut app = App {
        win: None,
        rx,
        // Defaults are round 2's proposals, not round 1's behaviour — each
        // axis keeps its round-1 setting as its first entry for comparison.
        motion: 0,   // SNAPBACK
        hairline: 2, // CLEAR
        handoff: 2,  // SETTLE
        flash: 2,    // GLOW
        set: 1,      // MIC-FIRST (3) — the set that tells the two anchors apart
        anchor: 0,   // MIC
        hover: false,
        ok: true,
        mode: Mode::Idle,
        mode_since: start,
        anim: None,
        queue: VecDeque::new(),
        next_at: None,
        held_bars: [0.5; BAR_COUNT],
    };
    el.run_app(&mut app)?;
    Ok(())
}

struct Anim {
    from: Geom,
    to: Geom,
    start: Instant,
    dur: Duration,
    ease: Ease,
}

struct App {
    win: Option<PillWindow>,
    rx: std::sync::mpsc::Receiver<Msg>,
    motion: usize,
    hairline: usize,
    handoff: usize,
    flash: usize,
    set: usize,
    anchor: usize,
    hover: bool,
    ok: bool,
    mode: Mode,
    mode_since: Instant,
    anim: Option<Anim>,
    /// Scripted playback: the modes still to visit, with how long to dwell in
    /// each once its transition has finished.
    queue: VecDeque<(Mode, u64)>,
    /// When the next queued step is due.
    next_at: Option<Instant>,
    /// The bar heights at the instant capture stopped. The real pill holds
    /// exactly these once the mode leaves `Recording`; the settle decays the
    /// live waveform into them rather than cutting to them.
    held_bars: [f32; BAR_COUNT],
}

impl App {
    fn motion(&self) -> &'static Motion {
        &MOTIONS[self.motion]
    }
    fn hairline(&self) -> &'static Hairline {
        &HAIRLINES[self.hairline]
    }
    fn handoff(&self) -> &'static Handoff {
        &HANDOFFS[self.handoff]
    }
    fn flash(&self) -> &'static Flash {
        &FLASHES[self.flash]
    }
    fn set(&self) -> &'static ButtonSet {
        &BUTTON_SETS[self.set]
    }
    fn anchor(&self) -> Anchor {
        ANCHORS[self.anchor].0
    }

    /// How far the expanded pill's centre sits from the nub's, so that the mic
    /// lands on the cursor. Zero under the CENTRE anchor.
    fn expanded_x_off(&self) -> f32 {
        match self.anchor() {
            Anchor::Centre => 0.0,
            Anchor::Mic => -self.set().mic_offset(),
        }
    }

    fn geom_of(&self, mode: Mode) -> Geom {
        let e = self.set();
        let hl = self.hairline();
        let base = Geom {
            w: IDLE_W,
            h: IDLE_H,
            x_off: 0.0,
            radius: IDLE_R,
            fill_rgb: BODY,
            fill_a: IDLE_FILL_A,
            border_rgb: hl.rgb,
            border_a: hl.a,
            border_w: 1.0,
            bars: 0.0,
            buttons: 0.0,
        };
        match mode {
            // Hidden keeps the nub's shape and fades to nothing, so revealing is
            // one motion rather than a fade plus a resize.
            Mode::Hidden => Geom {
                fill_a: 0.0,
                border_a: 0.0,
                ..base
            },
            Mode::Idle => base,
            Mode::Expanded => Geom {
                w: e.width(),
                h: EXP_H,
                x_off: self.expanded_x_off(),
                radius: EXP_R,
                fill_a: 235.0,
                buttons: 1.0,
                ..base
            },
            Mode::Recording => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                bars: 1.0,
                ..base
            },
            Mode::Processing => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                border_rgb: BORDER_PROCESSING,
                border_a: 165.0, // mid-breath; the live pulse takes over once settled
                bars: 0.45,
                ..base
            },
            Mode::Done => {
                let f = self.flash();
                let colour = if self.ok {
                    BORDER_SUCCESS
                } else {
                    BORDER_ERROR
                };
                Geom {
                    w: REC_W,
                    h: REC_H,
                    radius: REC_R,
                    // The tint is what makes the flash readable peripherally —
                    // a 1px edge is not, which is the round-1 complaint.
                    fill_rgb: lerp3(BODY, colour, f.tint),
                    fill_a: 245.0,
                    border_rgb: colour,
                    border_a: 235.0,
                    border_w: f.border_w,
                    bars: 1.0,
                    ..base
                }
            }
        }
    }

    fn tween_for(&self, from: Mode, to: Mode) -> Tween {
        let m = self.motion();
        match (from, to) {
            (_, Mode::Hidden) => m.conceal,
            (Mode::Hidden, _) => m.reveal,
            (_, Mode::Recording) => m.to_recording,
            // Owned by the handoff axis, not the motion profile.
            (_, Mode::Processing) => {
                let h = self.handoff();
                if h.colour_ms == 0 {
                    SNAP
                } else {
                    tw(h.colour_ms, Ease::Linear)
                }
            }
            (_, Mode::Done) => m.to_done,
            (_, Mode::Expanded) => m.hover_in,
            (Mode::Expanded, Mode::Idle) => m.hover_out,
            (_, Mode::Idle) => m.to_idle,
        }
    }

    /// The geometry on screen right now: mid-tween, or the settled mode plus
    /// whatever it animates on its own (the breathing border).
    fn current_geom(&self, now: Instant) -> Geom {
        let base = match &self.anim {
            Some(a) => {
                if a.dur.is_zero() {
                    a.to
                } else {
                    let t = (now.duration_since(a.start).as_secs_f32() / a.dur.as_secs_f32())
                        .clamp(0.0, 1.0);
                    a.from.lerp(a.to, a.ease.apply(t))
                }
            }
            None => self.geom_of(self.mode),
        };
        if self.mode == Mode::Processing && self.anim_done(now) {
            // Breathing border at ~0.8 Hz, same as the shipped pill.
            let e = now.duration_since(self.mode_since).as_secs_f32();
            let pulse = 0.5 - 0.5 * (e * std::f32::consts::TAU * 0.8).cos();
            return Geom {
                border_a: 110.0 + 110.0 * pulse,
                ..base
            };
        }
        base
    }

    /// Live waveform while recording; afterwards the held heights, reached by
    /// decaying the waveform into them over the handoff's `bars_ms`.
    fn bar_amps(&self, now: Instant) -> [f32; BAR_COUNT] {
        let t = now.duration_since(self.mode_since).as_secs_f32();
        if self.mode == Mode::Recording {
            return live_waveform(t);
        }
        let settle_ms = self.handoff().bars_ms as f32;
        if self.mode == Mode::Processing && settle_ms > 0.0 {
            let k = (t * 1000.0 / settle_ms).clamp(0.0, 1.0);
            // Ease the decay so the waveform loses energy rather than being
            // linearly dragged to a stop.
            let k = Ease::OutCubic.apply(k);
            let live = live_waveform(t);
            let mut out = self.held_bars;
            for i in 0..BAR_COUNT {
                out[i] = lerp(live[i], self.held_bars[i], k);
            }
            return out;
        }
        self.held_bars
    }

    fn anim_done(&self, now: Instant) -> bool {
        match &self.anim {
            None => true,
            Some(a) => now.duration_since(a.start) >= a.dur,
        }
    }

    fn go(&mut self, mode: Mode, now: Instant) -> Duration {
        // Capture the heights the real pill would freeze at, before the mode
        // (and therefore the waveform's time origin) changes.
        if self.mode == Mode::Recording && mode != Mode::Recording {
            let t = now.duration_since(self.mode_since).as_secs_f32();
            self.held_bars = live_waveform(t);
        }
        let t = self.tween_for(self.mode, mode);
        let from = self.current_geom(now);
        let to = self.geom_of(mode);
        let dur = Duration::from_millis(t.ms as u64);
        self.anim = Some(Anim {
            from,
            to,
            start: now,
            dur,
            ease: t.ease,
        });
        self.mode = mode;
        self.mode_since = now;
        dur
    }

    fn play(&mut self, script: u8, now: Instant) {
        // Every script starts from a known mode so replays are comparable.
        let (start, steps): (Mode, &[(Mode, u64)]) = match script {
            1 => (Mode::Idle, &[(Mode::Expanded, 1400), (Mode::Idle, 0)]),
            2 => (
                Mode::Idle,
                &[
                    (Mode::Recording, 1600),
                    (Mode::Processing, 1200),
                    (Mode::Done, 900),
                    (Mode::Idle, 0),
                ],
            ),
            3 => (Mode::Idle, &[(Mode::Hidden, 700), (Mode::Idle, 0)]),
            4 => (
                Mode::Idle,
                &[
                    (Mode::Expanded, 700),
                    (Mode::Recording, 1400),
                    (Mode::Processing, 900),
                    (Mode::Done, 900),
                    (Mode::Idle, 0),
                ],
            ),
            _ => (
                Mode::Hidden,
                &[
                    (Mode::Recording, 1600),
                    (Mode::Processing, 900),
                    (Mode::Done, 900),
                    (Mode::Hidden, 0),
                ],
            ),
        };
        // Snap to the starting mode so the first transition of the script is
        // the one being judged, not a leftover from whatever was on screen.
        self.mode = start;
        self.anim = None;
        self.mode_since = now;
        self.queue = steps.iter().copied().collect();
        self.next_at = Some(now);
    }

    /// Cursor polling — the pill is never a mouse target (`WS_EX_TRANSPARENT`
    /// stays on), so hover is a hit test against the rect we last drew.
    fn poll_hover(&mut self, now: Instant) {
        if !self.hover || !self.queue.is_empty() {
            return;
        }
        if !matches!(self.mode, Mode::Idle | Mode::Expanded) {
            return;
        }
        let Some(win) = self.win.as_ref() else { return };
        let Some((cx, cy)) = cursor_pos() else { return };

        // Hit test the *settled* rect of the current mode, not the mid-morph
        // one: testing the live rect makes hover-out chase a shrinking target
        // and oscillate.
        let g = self.geom_of(self.mode);
        let inside = win.contains(&g, cx, cy);
        match (self.mode, inside) {
            (Mode::Idle, true) => {
                self.go(Mode::Expanded, now);
            }
            (Mode::Expanded, false) => {
                self.go(Mode::Idle, now);
            }
            _ => {}
        }
    }

    fn pump_queue(&mut self, now: Instant) {
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

    /// Surface the full state on every change — the numbers are the answer this
    /// ticket has to write down.
    fn report(&self) {
        let m = self.motion();
        let e = self.set();
        let hl = self.hairline();
        let hd = self.handoff();
        let fl = self.flash();

        println!("\n=== motion  {}  —  {}", m.name, m.note);
        let rows: [(&str, Tween); 7] = [
            ("Hidden    -> Idle       (reveal)", m.reveal),
            ("Idle      -> Hidden     (conceal)", m.conceal),
            ("Idle      -> Expanded   (hover in)", m.hover_in),
            ("Expanded  -> Idle       (hover out)", m.hover_out),
            ("*         -> Recording  (chord down)", m.to_recording),
            ("Processing-> Done", m.to_done),
            ("Done      -> Idle", m.to_idle),
        ];
        for (label, t) in rows {
            if t.ms == 0 {
                println!("    {label:38}  SNAP");
            } else {
                println!("    {label:38}  {:>4}ms  {}", t.ms, t.ease.name());
            }
        }
        println!(
            "    {:38}  {}",
            "Recording -> Processing (handoff)",
            if hd.colour_ms == 0 {
                "SNAP".to_string()
            } else {
                format!("{:>4}ms  linear", hd.colour_ms)
            }
        );

        println!("=== handoff {}  —  {}", hd.name, hd.note);
        println!(
            "      colour crossfade {}ms   bar settle {}ms",
            hd.colour_ms, hd.bars_ms
        );
        println!("=== hairline {}  —  {}", hl.name, hl.note);
        println!(
            "      rgb({:.0},{:.0},{:.0}) @ a={:.0}, on every state including the nub",
            hl.rgb.0, hl.rgb.1, hl.rgb.2, hl.a
        );
        println!("=== flash   {}  —  {}", fl.name, fl.note);
        println!(
            "      border {:.1}px, body tinted {:.0}% toward the flash colour",
            fl.border_w,
            fl.tint * 100.0
        );

        let (_, an, an_note) = ANCHORS[self.anchor];
        println!("=== anchor  {}  —  {}", an, an_note);
        println!("=== buttons {}  —  {}", e.name, e.note);
        println!(
            "      auto-sized: 2 x {:.0} padding + {} x {:.0} + {} x {:.0} gap = {:.0}px wide, {:.0} tall",
            BTN_PAD,
            e.glyphs.len(),
            BTN_D,
            e.glyphs.len() - 1,
            BTN_GAP,
            e.width(),
            EXP_H
        );
        println!(
            "      morph from the nub: {:.0} -> {:.0} px wide ({:.1}x), {:.0} -> {:.0} tall",
            IDLE_W,
            e.width(),
            e.width() / IDLE_W,
            IDLE_H,
            EXP_H
        );
        let off = self.expanded_x_off();
        println!(
            "      mic is button {} of {}, {:.0}px from the pill's centre; pill slides {:.0}px — mic {} on the cursor",
            e.mic + 1,
            e.glyphs.len(),
            e.mic_offset(),
            off,
            if (e.mic_offset() + off).abs() < 0.5 {
                "LANDS"
            } else {
                "MISSES — not clickable without moving the mouse"
            }
        );

        println!(
            "  live hover: {}    flash outcome: {}",
            if self.hover { "ON" } else { "off" },
            if self.ok { "ok (green)" } else { "failed (red)" }
        );
        println!("  [m] motion [b] hairline [p] handoff [x] flash [e] buttons [a] anchor [h] hover [1-5] replay [f] flip [q] quit");
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_none() {
            match PillWindow::create(el) {
                Ok(w) => {
                    w.show();
                    self.win = Some(w);
                    self.report();
                }
                Err(e) => {
                    eprintln!("window creation failed: {e}");
                    el.exit();
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            el.exit();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Quit => {
                    el.exit();
                    return;
                }
                Msg::NextMotion => {
                    self.motion = (self.motion + 1) % MOTIONS.len();
                    self.report();
                }
                Msg::NextButtonSet => {
                    self.set = (self.set + 1) % BUTTON_SETS.len();
                    self.report();
                }
                Msg::NextAnchor => {
                    self.anchor = (self.anchor + 1) % ANCHORS.len();
                    self.report();
                }
                Msg::NextHairline => {
                    self.hairline = (self.hairline + 1) % HAIRLINES.len();
                    self.report();
                }
                Msg::NextHandoff => {
                    self.handoff = (self.handoff + 1) % HANDOFFS.len();
                    self.report();
                }
                Msg::NextFlash => {
                    self.flash = (self.flash + 1) % FLASHES.len();
                    self.report();
                }
                Msg::ToggleHover => {
                    self.hover = !self.hover;
                    self.report();
                }
                Msg::FlipOutcome => {
                    self.ok = !self.ok;
                    self.report();
                }
                Msg::Play(n) => self.play(n, now),
            }
        }

        self.pump_queue(now);
        self.poll_hover(now);

        let g = self.current_geom(now);
        let amps = self.bar_amps(now);
        let set = self.set();
        if let Some(w) = self.win.as_mut() {
            if let Err(e) = w.render(&g, &amps, set) {
                eprintln!("render failed: {e}");
            }
        }

        // ~60 Hz: the whole question is motion, so a 30 Hz harness would be
        // judging the harness.
        std::thread::sleep(Duration::from_millis(16));
    }
}

/// Deterministic pseudo-waveform: replaying a script twice must look identical,
/// or two easings can't be compared.
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

fn draw(pm: &mut Pixmap, scale: f32, g: &Geom, amps: &[f32; BAR_COUNT], set: &ButtonSet) {
    pm.fill(tiny_skia::Color::TRANSPARENT);
    if g.fill_a < 0.5 && g.border_a < 0.5 {
        return;
    }

    let w = pm.width() as f32;
    let h = pm.height() as f32;
    let sw = g.w.max(1.0) * scale;
    let sh = g.h.max(1.0) * scale;

    // Half the stroke plus ~1px of transparent margin, so the anti-aliased
    // outer edge has somewhere to fade to and the curve doesn't stair-step.
    let border_w = (g.border_w * scale).max(1.0);
    let m = border_w * 0.5 + 1.0 * scale;

    let x = (w - sw) / 2.0 + g.x_off * scale + m;
    let y = h - sh + m;
    let rw = (sw - 2.0 * m).max(1.0);
    let rh = (sh - 2.0 * m).max(1.0);
    let r = (g.radius * scale).min(rh / 2.0);

    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, x, y, rw, rh, r);
    let Some(path) = pb.finish() else { return };

    let mut fill = Paint::default();
    fill.set_color_rgba8(
        g.fill_rgb.0.clamp(0.0, 255.0) as u8,
        g.fill_rgb.1.clamp(0.0, 255.0) as u8,
        g.fill_rgb.2.clamp(0.0, 255.0) as u8,
        g.fill_a.clamp(0.0, 255.0) as u8,
    );
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    if g.border_a >= 0.5 {
        let mut border = Paint::default();
        border.set_color_rgba8(
            g.border_rgb.0.clamp(0.0, 255.0) as u8,
            g.border_rgb.1.clamp(0.0, 255.0) as u8,
            g.border_rgb.2.clamp(0.0, 255.0) as u8,
            g.border_a.clamp(0.0, 255.0) as u8,
        );
        border.anti_alias = true;
        let stroke = Stroke {
            width: border_w,
            ..Default::default()
        };
        pm.stroke_path(&path, &border, &stroke, Transform::identity(), None);
    }

    let cx = x + rw / 2.0;
    let cy = y + rh / 2.0;
    if g.bars > 0.01 {
        draw_bars(pm, scale, g.bars, cx, cy, rh, amps);
    }
    if g.buttons > 0.01 {
        draw_buttons(pm, scale, g, cx, cy, set);
    }
}

/// Bars are sized from the *current* (interpolated) shape, so they grow out of
/// the morph rather than popping in at full size when it lands.
fn draw_bars(
    pm: &mut Pixmap,
    scale: f32,
    alpha: f32,
    cx: f32,
    cy: f32,
    rh: f32,
    amps: &[f32; BAR_COUNT],
) {
    let bar_w = (rh * 0.09).max(1.0 * scale);
    let gap = bar_w;
    let min_h = bar_w * 2.5;
    let max_h = (rh - 8.0 * scale).max(min_h);
    let n = BAR_COUNT as f32;
    let total = n * bar_w + (n - 1.0) * gap;
    let start_x = cx - total / 2.0;

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, (235.0 * alpha).clamp(0.0, 255.0) as u8);
    paint.anti_alias = true;

    let mut pb = PathBuilder::new();
    for (i, &amp) in amps.iter().enumerate() {
        let bh = min_h + amp.clamp(0.0, 1.0) * (max_h - min_h);
        let x = start_x + i as f32 * (bar_w + gap);
        rounded_rect(&mut pb, x, cy - bh / 2.0, bar_w, bh, bar_w / 2.0);
    }
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Circular stub buttons. The glyphs are stand-ins — what is being judged is
/// whether targets of a usable size sit comfortably in an auto-sized pill, and
/// whether the mic lands where the cursor already is. The mic's circle is drawn
/// brighter so you can see at a glance whether the anchor put it under you.
///
/// The circles are drawn at their true size but *faded* with `g.buttons`, and
/// the pill's width is interpolated around them — so mid-morph they are already
/// in their final positions, sliding into place with the edges.
fn draw_buttons(pm: &mut Pixmap, scale: f32, g: &Geom, cx: f32, cy: f32, set: &ButtonSet) {
    let d = BTN_D * scale;
    let gap = BTN_GAP * scale;
    let n = set.glyphs.len();
    let total = n as f32 * d + (n as f32 - 1.0) * gap;
    let start_x = cx - total / 2.0;
    let a = g.buttons.clamp(0.0, 1.0);

    let mut ring = Paint::default();
    ring.set_color_rgba8(255, 255, 255, (26.0 * a) as u8);
    ring.anti_alias = true;
    let mut mic_ring = Paint::default();
    mic_ring.set_color_rgba8(255, 255, 255, (58.0 * a) as u8);
    mic_ring.anti_alias = true;
    let mut ink = Paint::default();
    ink.set_color_rgba8(255, 255, 255, (215.0 * a) as u8);
    ink.anti_alias = true;

    for (i, glyph) in set.glyphs.iter().enumerate() {
        let bx = start_x + i as f32 * (d + gap);
        let gcx = bx + d / 2.0;

        // The hit target as a filled circle — these are the per-button regions
        // the map deferred and this ticket has just un-deferred.
        let mut pb = PathBuilder::new();
        rounded_rect(&mut pb, bx, cy - d / 2.0, d, d, d / 2.0);
        if let Some(p) = pb.finish() {
            let paint = if i == set.mic { &mic_ring } else { &ring };
            pm.fill_path(&p, paint, FillRule::Winding, Transform::identity(), None);
        }

        let mut pb = PathBuilder::new();
        match glyph {
            // Mic: a capsule on a stem.
            Glyph::Mic => {
                let cw = d * 0.26;
                let ch = d * 0.42;
                rounded_rect(&mut pb, gcx - cw / 2.0, cy - ch * 0.75, cw, ch, cw / 2.0);
                rounded_rect(
                    &mut pb,
                    gcx - cw * 0.08,
                    cy + ch * 0.25,
                    cw * 0.16,
                    d * 0.16,
                    cw * 0.08,
                );
            }
            // Copy: two offset squares.
            Glyph::Copy => {
                let s = d * 0.34;
                let o = d * 0.09;
                rounded_rect(&mut pb, gcx - s * 0.9, cy - s * 0.9, s, s, s * 0.22);
                rounded_rect(
                    &mut pb,
                    gcx - s * 0.9 + o * 2.0,
                    cy - s * 0.9 + o * 2.0,
                    s,
                    s,
                    s * 0.22,
                );
            }
            // Settings: three stacked sliders.
            Glyph::Sliders => {
                let lw = d * 0.46;
                let lh = (d * 0.08).max(1.0);
                for k in 0..3 {
                    let ly = cy - lh * 4.0 + k as f32 * lh * 4.0;
                    rounded_rect(&mut pb, gcx - lw / 2.0, ly, lw, lh, lh / 2.0);
                }
            }
            // History: a ring with two hands.
            Glyph::Clock => {
                let r = d * 0.22;
                let t = (d * 0.07).max(1.0);
                rounded_rect(&mut pb, gcx - r, cy - t / 2.0, r, t, t / 2.0);
                rounded_rect(&mut pb, gcx - t / 2.0, cy - r, t, r, t / 2.0);
            }
        }
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, &ink, FillRule::Winding, Transform::identity(), None);
        }
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
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
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
    window: Window,
    scale: f32,
    /// Physical screen position of the (fixed) window box.
    win_x: i32,
    win_y: i32,
    pixmap: Pixmap,
    hires: Pixmap,
    mid: Pixmap,
    layered: LayeredSurface,
}

impl PillWindow {
    fn create(el: &ActiveEventLoop) -> Result<Self> {
        let primary = el
            .primary_monitor()
            .or_else(|| el.available_monitors().next())
            .ok_or_else(|| anyhow!("no monitor available"))?;
        let scale = primary.scale_factor() as f32;
        let monitor_pos = primary.position();
        let monitor_size = primary.size();

        let phys_w = (BOX_W as f32 * scale) as i32;
        let phys_h = (BOX_H as f32 * scale) as i32;
        let margin = (BOTTOM_MARGIN as f32 * scale) as i32;
        let x = monitor_pos.x + (monitor_size.width as i32 - phys_w) / 2;
        let y = monitor_pos.y + monitor_size.height as i32 - phys_h - margin;

        let attrs = WindowAttributes::default()
            .with_title("Draft Pill Transitions Prototype")
            .with_inner_size(LogicalSize::new(BOX_W, BOX_H))
            .with_position(LogicalPosition::new(
                x as f64 / scale as f64,
                y as f64 / scale as f64,
            ))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false);

        let window = el.create_window(attrs)?;
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let pixmap = Pixmap::new(w, h).ok_or_else(|| anyhow!("pixmap"))?;
        let hires =
            Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE).ok_or_else(|| anyhow!("hires"))?;
        let mid = Pixmap::new(w * 2, h * 2).ok_or_else(|| anyhow!("mid"))?;
        let layered = LayeredSurface::new(&window, w, h)?;

        Ok(Self {
            window,
            scale,
            win_x: x,
            win_y: y,
            pixmap,
            hires,
            mid,
            layered,
        })
    }

    fn show(&self) {
        self.window.set_visible(true);
    }

    /// Hit test a physical cursor position against the shape `g` would occupy —
    /// bottom-aligned and centred inside the fixed box, plus a couple of
    /// logical px of slop so the 10px-tall nub is not a pixel-hunt.
    fn contains(&self, g: &Geom, cx: i32, cy: i32) -> bool {
        let s = self.scale;
        let slop = 3.0 * s;
        let sw = g.w * s;
        let sh = g.h * s;
        let box_w = BOX_W as f32 * s;
        let box_h = BOX_H as f32 * s;
        let left = self.win_x as f32 + (box_w - sw) / 2.0 + g.x_off * s - slop;
        let right = left + sw + 2.0 * slop;
        let bottom = self.win_y as f32 + box_h;
        let top = bottom - sh - slop;
        let (cx, cy) = (cx as f32, cy as f32);
        cx >= left && cx <= right && cy >= top && cy <= bottom
    }

    fn render(&mut self, g: &Geom, amps: &[f32; BAR_COUNT], set: &ButtonSet) -> Result<()> {
        draw(&mut self.hires, self.scale * SUPERSAMPLE as f32, g, amps, set);

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

struct LayeredSurface {
    hwnd: windows::Win32::Foundation::HWND,
    mem_dc: windows::Win32::Graphics::Gdi::HDC,
    dib: windows::Win32::Graphics::Gdi::HBITMAP,
    bits: *mut u8,
    w: u32,
    h: u32,
}

impl LayeredSurface {
    fn new(window: &Window, w: u32, h: u32) -> Result<Self> {
        let hwnd = hwnd_from_window(window)?;
        apply_layered_styles(hwnd);
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

fn hwnd_from_window(window: &Window) -> Result<windows::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window
        .window_handle()
        .map_err(|e| anyhow!("window handle: {e}"))?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return Err(anyhow!("not a Win32 window"));
    };
    Ok(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _))
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

fn apply_layered_styles(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = (cur as u32)
            | WS_EX_LAYERED.0
            | WS_EX_TRANSPARENT.0
            | WS_EX_NOACTIVATE.0
            | WS_EX_TOOLWINDOW.0
            | WS_EX_TOPMOST.0;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style as isize);
    }
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
