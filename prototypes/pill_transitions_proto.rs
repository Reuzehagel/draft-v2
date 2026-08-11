// PROTOTYPE — THROWAWAY. Not production code, not wired into `draft`.
//
// Answers wayfinder ticket "How the pill expands and collapses" (#18): what
// morphs into what, over how long, on what easing, and which transitions
// animate versus snap.
//
// Run:  cargo run --bin pill-transitions-proto
// Then, in the terminal:
//   m        next motion profile (INSTANT / SNAPPY / SMOOTH / SPRINGY)
//   e        next Expanded size candidate
//   h        toggle live hover (cursor polling) on/off
//   1        replay  Idle -> Expanded -> Idle
//   2        replay  a whole session, residency ON   (-> Recording -> Processing -> Done -> Idle)
//   3        replay  Idle -> Hidden -> Idle          (fullscreen suppress / residency toggle)
//   4        replay  Expanded -> Recording -> ... -> Idle   (chord pressed mid-hover)
//   5        replay  a whole session, residency OFF  (Hidden -> ... -> Done -> Hidden)
//   f        flip the terminal flash between ok and failed
//   q        quit
//
// Judge it over a *white* window as well as a dark one, and with the cursor
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
// plus headroom for SPRINGY's overshoot.
const BOX_W: u32 = 128;
const BOX_H: u32 = 48;
const BOTTOM_MARGIN: u32 = 80;
const SUPERSAMPLE: u32 = 4;
const BAR_COUNT: usize = 7;

// Settled elsewhere on this map, and held fixed here.
//   Idle      — NUB-36 from #17: 36x10, fully rounded, no border, no bars.
//   Recording — 62x28, the size promoted out of #17 and filed as #25/#26.
const IDLE_W: f32 = 36.0;
const IDLE_H: f32 = 10.0;
const IDLE_R: f32 = 5.0;
const IDLE_FILL_A: f32 = 140.0; // provisional — #27 owns this number, not us
const REC_W: f32 = 62.0;
const REC_H: f32 = 28.0;
const REC_R: f32 = 14.0;

const BORDER_IDLE_REC: (f32, f32, f32) = (170.0, 172.0, 178.0);
const BORDER_PROCESSING: (f32, f32, f32) = (190.0, 192.0, 200.0);
const BORDER_SUCCESS: (f32, f32, f32) = (74.0, 188.0, 120.0);
const BORDER_ERROR: (f32, f32, f32) = (214.0, 96.0, 96.0);

// ---------------------------------------------------------------------------
// The axes being swept
// ---------------------------------------------------------------------------

/// Expanded has never been sized by any ticket — the map only says the expanded
/// pill is clickable as one hit region. It is swept here rather than assumed,
/// because how far the nub has to travel and how long that should take are the
/// same judgement: a 36->62 morph and a 36->104 morph do not want one duration.
///
/// All three are drawn empty. What Expanded *contains* (a hotkey hint, a mic
/// glyph, a button bar) is still fog on the map; this is silhouette only.
struct Expanded {
    name: &'static str,
    w: f32,
    h: f32,
    radius: f32,
    note: &'static str,
}

const EXPANDED: &[Expanded] = &[
    Expanded {
        name: "E-REC 62x28",
        w: 62.0,
        h: 28.0,
        radius: 14.0,
        note: "hover previews the recording silhouette exactly — one object, two sizes",
    },
    Expanded {
        name: "E-WIDE 84x28",
        w: 84.0,
        h: 28.0,
        radius: 14.0,
        note: "wider than recording: room for a hotkey hint, and hover reads as its own state",
    },
    Expanded {
        name: "E-BAR 104x32",
        w: 104.0,
        h: 32.0,
        radius: 16.0,
        note: "wide enough for the deferred button bar — the biggest morph on the table",
    },
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
    /// Recording -> Processing (same geometry; only the border changes)
    to_processing: Tween,
    /// Processing -> Done (same geometry; only the border changes)
    to_done: Tween,
    /// Done -> Idle (the flash resolving back to the nub)
    to_idle: Tween,
}

const MOTIONS: &[Motion] = &[
    Motion {
        name: "INSTANT",
        note: "the control — nothing animates. Does motion earn its place at all?",
        reveal: SNAP,
        conceal: SNAP,
        hover_in: SNAP,
        hover_out: SNAP,
        to_recording: SNAP,
        to_processing: SNAP,
        to_done: SNAP,
        to_idle: SNAP,
    },
    Motion {
        name: "SNAPPY",
        note: "short and decelerating; the chord response is the fastest thing on screen",
        reveal: tw(140, Ease::OutCubic),
        conceal: tw(120, Ease::OutCubic),
        hover_in: tw(110, Ease::OutCubic),
        hover_out: tw(90, Ease::OutCubic),
        to_recording: tw(90, Ease::OutCubic),
        to_processing: SNAP,
        to_done: SNAP,
        to_idle: tw(160, Ease::OutCubic),
    },
    Motion {
        name: "SMOOTH",
        note: "longer, symmetric easing; colour changes crossfade instead of snapping",
        reveal: tw(220, Ease::InOutCubic),
        conceal: tw(200, Ease::InOutCubic),
        hover_in: tw(190, Ease::InOutCubic),
        hover_out: tw(170, Ease::InOutCubic),
        to_recording: tw(150, Ease::InOutCubic),
        to_processing: tw(180, Ease::Linear),
        to_done: tw(140, Ease::Linear),
        to_idle: tw(260, Ease::InOutCubic),
    },
    Motion {
        name: "SPRINGY",
        note: "growth overshoots and settles; shrinking stays clean (a bouncing exit reads as a glitch)",
        reveal: tw(240, Ease::OutBack),
        conceal: tw(130, Ease::OutCubic),
        hover_in: tw(240, Ease::OutBack),
        hover_out: tw(130, Ease::OutCubic),
        to_recording: tw(200, Ease::OutBack),
        to_processing: tw(180, Ease::Linear),
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
    radius: f32,
    fill_a: f32,
    border_a: f32,
    border_rgb: (f32, f32, f32),
    /// Opacity of the waveform bars, 0 = absent.
    bars: f32,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

impl Geom {
    fn lerp(self, to: Geom, t: f32) -> Geom {
        Geom {
            w: lerp(self.w, to.w, t),
            h: lerp(self.h, to.h, t),
            radius: lerp(self.radius, to.radius, t),
            fill_a: lerp(self.fill_a, to.fill_a, t),
            border_a: lerp(self.border_a, to.border_a, t),
            border_rgb: (
                lerp(self.border_rgb.0, to.border_rgb.0, t),
                lerp(self.border_rgb.1, to.border_rgb.1, t),
                lerp(self.border_rgb.2, to.border_rgb.2, t),
            ),
            bars: lerp(self.bars, to.bars, t),
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

enum Msg {
    NextMotion,
    NextExpanded,
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
                "e" => Msg::NextExpanded,
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
        motion: 1, // SNAPPY — INSTANT is the control, not the default to stare at
        expanded: 0,
        hover: false,
        ok: true,
        mode: Mode::Idle,
        mode_since: start,
        anim: None,
        queue: VecDeque::new(),
        next_at: None,
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
    expanded: usize,
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
}

impl App {
    fn motion(&self) -> &'static Motion {
        &MOTIONS[self.motion]
    }

    fn expanded(&self) -> &'static Expanded {
        &EXPANDED[self.expanded]
    }

    fn geom_of(&self, mode: Mode) -> Geom {
        let e = self.expanded();
        match mode {
            // Hidden keeps the nub's shape and fades to nothing, so revealing is
            // one motion rather than a fade plus a resize.
            Mode::Hidden => Geom {
                w: IDLE_W,
                h: IDLE_H,
                radius: IDLE_R,
                fill_a: 0.0,
                border_a: 0.0,
                border_rgb: BORDER_IDLE_REC,
                bars: 0.0,
            },
            Mode::Idle => Geom {
                w: IDLE_W,
                h: IDLE_H,
                radius: IDLE_R,
                fill_a: IDLE_FILL_A,
                border_a: 0.0,
                border_rgb: BORDER_IDLE_REC,
                bars: 0.0,
            },
            Mode::Expanded => Geom {
                w: e.w,
                h: e.h,
                radius: e.radius,
                fill_a: 235.0,
                border_a: 48.0,
                border_rgb: BORDER_IDLE_REC,
                bars: 0.0,
            },
            Mode::Recording => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                border_a: 48.0,
                border_rgb: BORDER_IDLE_REC,
                bars: 1.0,
            },
            Mode::Processing => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                border_a: 165.0, // mid-breath; the live pulse takes over once settled
                border_rgb: BORDER_PROCESSING,
                bars: 0.45,
            },
            Mode::Done => Geom {
                w: REC_W,
                h: REC_H,
                radius: REC_R,
                fill_a: 245.0,
                border_a: 235.0,
                border_rgb: if self.ok {
                    BORDER_SUCCESS
                } else {
                    BORDER_ERROR
                },
                bars: 1.0,
            },
        }
    }

    fn tween_for(&self, from: Mode, to: Mode) -> Tween {
        let m = self.motion();
        match (from, to) {
            (_, Mode::Hidden) => m.conceal,
            (Mode::Hidden, _) => m.reveal,
            (_, Mode::Recording) => m.to_recording,
            (_, Mode::Processing) => m.to_processing,
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
                    let t = (now.duration_since(a.start).as_secs_f32()
                        / a.dur.as_secs_f32())
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

    fn anim_done(&self, now: Instant) -> bool {
        match &self.anim {
            None => true,
            Some(a) => now.duration_since(a.start) >= a.dur,
        }
    }

    fn go(&mut self, mode: Mode, now: Instant) -> Duration {
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
            1 => (Mode::Idle, &[(Mode::Expanded, 900), (Mode::Idle, 0)]),
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
                    (Mode::Expanded, 500),
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
        let e = self.expanded();
        println!("\n=== {}  —  {}", m.name, m.note);
        println!("  Expanded: {} ({:.0}x{:.0} r{:.0})  —  {}", e.name, e.w, e.h, e.radius, e.note);
        let rows: [(&str, Tween); 8] = [
            ("Hidden   -> Idle      (reveal)", m.reveal),
            ("Idle     -> Hidden    (conceal)", m.conceal),
            ("Idle     -> Expanded  (hover in)", m.hover_in),
            ("Expanded -> Idle      (hover out)", m.hover_out),
            ("*        -> Recording (chord down)", m.to_recording),
            ("Recording-> Processing", m.to_processing),
            ("Processing-> Done", m.to_done),
            ("Done     -> Idle", m.to_idle),
        ];
        for (label, t) in rows {
            if t.ms == 0 {
                println!("    {label:36}  SNAP");
            } else {
                println!("    {label:36}  {:>4}ms  {}", t.ms, t.ease.name());
            }
        }
        println!(
            "  live hover: {}    flash: {}",
            if self.hover { "ON" } else { "off" },
            if self.ok { "ok (green)" } else { "failed (red)" }
        );
        println!("  [m] motion  [e] expanded  [h] hover  [1-5] replay  [f] flip flash  [q] quit");
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
                Msg::NextExpanded => {
                    self.expanded = (self.expanded + 1) % EXPANDED.len();
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
        let bar_t = now.duration_since(self.mode_since).as_secs_f32();
        let recording = self.mode == Mode::Recording;
        if let Some(w) = self.win.as_mut() {
            if let Err(e) = w.render(&g, bar_t, recording) {
                eprintln!("render failed: {e}");
            }
        }

        // ~60 Hz: the whole question is motion, so a 30 Hz harness would be
        // judging the harness.
        std::thread::sleep(Duration::from_millis(16));
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(pm: &mut Pixmap, scale: f32, g: &Geom, bar_t: f32, recording: bool) {
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
    let border_w = (1.0 * scale).max(1.0);
    let m = border_w * 0.5 + 1.0 * scale;

    let x = (w - sw) / 2.0 + m;
    let y = h - sh + m;
    let rw = (sw - 2.0 * m).max(1.0);
    let rh = (sh - 2.0 * m).max(1.0);
    let r = (g.radius * scale).min(rh / 2.0);

    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, x, y, rw, rh, r);
    let Some(path) = pb.finish() else { return };

    let mut fill = Paint::default();
    fill.set_color_rgba8(13, 13, 13, g.fill_a.clamp(0.0, 255.0) as u8);
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

    if g.bars > 0.01 {
        draw_bars(pm, scale, g, x + rw / 2.0, y + rh / 2.0, rh, bar_t, recording);
    }
}

/// Bars are sized from the *current* (interpolated) shape, so they grow out of
/// the morph rather than popping in at full size when it lands.
#[allow(clippy::too_many_arguments)]
fn draw_bars(
    pm: &mut Pixmap,
    scale: f32,
    g: &Geom,
    cx: f32,
    cy: f32,
    rh: f32,
    t: f32,
    recording: bool,
) {
    let bar_w = (rh * 0.09).max(1.0 * scale);
    let gap = bar_w;
    let min_h = bar_w * 2.5;
    let max_h = (rh - 8.0 * scale).max(min_h);
    let n = BAR_COUNT as f32;
    let total = n * bar_w + (n - 1.0) * gap;
    let start_x = cx - total / 2.0;

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, (235.0 * g.bars).clamp(0.0, 255.0) as u8);
    paint.anti_alias = true;

    let mut pb = PathBuilder::new();
    for i in 0..BAR_COUNT {
        // Deterministic pseudo-waveform: replaying a script twice must look
        // identical, or two easings can't be compared.
        let amp = if recording {
            let p = i as f32 * 0.9;
            (0.5 + 0.5 * ((t * 6.0 + p).sin() * 0.6 + (t * 11.0 + p * 1.7).sin() * 0.4))
                .clamp(0.0, 1.0)
        } else {
            // Frozen mid-height once capture stops, standing in for the real
            // frozen `last_bars`.
            let p = i as f32 * 0.9;
            (0.5 + 0.5 * (p.sin() * 0.6)).clamp(0.0, 1.0)
        };
        let bh = min_h + amp * (max_h - min_h);
        let x = start_x + i as f32 * (bar_w + gap);
        rounded_rect(&mut pb, x, cy - bh / 2.0, bar_w, bh, bar_w / 2.0);
    }
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
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
    #[allow(dead_code)]
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
        let left = self.win_x as f32 + (box_w - sw) / 2.0 - slop;
        let right = left + sw + 2.0 * slop;
        let bottom = self.win_y as f32 + box_h;
        let top = bottom - sh - slop;
        let (cx, cy) = (cx as f32, cy as f32);
        cx >= left && cx <= right && cy >= top && cy <= bottom
    }

    fn render(&mut self, g: &Geom, bar_t: f32, recording: bool) -> Result<()> {
        draw(
            &mut self.hires,
            self.scale * SUPERSAMPLE as f32,
            g,
            bar_t,
            recording,
        );

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
