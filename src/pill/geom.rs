// The pill's motion model: every mode is one interpolable [`Geom`], and every
// transition is a lerp between two of them off a start time and an [`Ease`].
//
// This is the whole animation system. There is no per-mode bespoke tween, no
// "fade the flash out over the last 30% of its linger", no resize-then-fade —
// one shape morphs into another, and the adapter derives each frame by asking a
// running [`Motion`] what the geometry is *now*. Settled in #18, where a
// prototype swept four motion profiles over real `UpdateLayeredWindow` frames
// and SNAPPY won.
//
// Two consequences worth stating, because they are what makes the resident pill
// possible at all:
//
// - **`Hidden` is the nub's shape at alpha 0.** Revealing is therefore one
//   motion — the alphas come up — rather than a fade plus a resize. Every mode
//   the pill can be in is the same silhouette at a different size, so there is
//   never a frame where the shape is ambiguous.
// - **The pill only ever animates its pixels.** The window sits at a fixed
//   [`ENVELOPE_W`]x[`ENVELOPE_H`], big enough for the largest mode, and a Geom
//   is drawn centred inside it. Nothing is resized, moved, or reallocated to
//   run an animation.
//
// Pure: no winit, no Win32, no `Instant::now()`. `Motion::at` takes the `now`
// it is asked about, exactly like the cores do.

use crate::pill::core::PillMode;
use std::time::{Duration, Instant};

/// The pill's resting silhouette — what `Idle` renders and what `Hidden` keeps
/// at alpha 0. Settled in #17/#27.
pub const NUB_W: f32 = 36.0;
pub const NUB_H: f32 = 10.0;
/// Fully rounded at this height, and stated rather than derived: the nub is the
/// one size whose radius was judged by eye rather than off the shape ratio.
pub const NUB_RADIUS: f32 = 5.0;

/// The window's fixed size, in logical pixels: the largest mode's envelope,
/// with the smaller ones drawn centred inside it. Every mode animates within
/// this rect, so no window move or buffer reallocation is ever part of a
/// transition.
///
/// The largest mode *is* the envelope, and if a future mode grows past it, this
/// is the one place that has to change. Since #44 that mode is `Expanded`: the
/// button bar is 118x32, and the envelope holds it with a margin all round —
/// the end padding the hit test treats as inert, and the room the outermost
/// island's anti-aliased edge fades into.
///
/// Growing it does not move the pill: every Geom is drawn centred, and
/// [`crate::pill::PILL_BOTTOM_MARGIN`] was retuned by the same growth, so the
/// nub and the session pill sit exactly where they did.
pub const ENVELOPE_W: u32 = 124;
pub const ENVELOPE_H: u32 = 36;

/// The session pill's own size, settled in #41 — the mid-size silhouette read
/// better as the *recording* state than anything did as idle, so recording took
/// it and the nub went smaller. It was the envelope until the button bar grew
/// past it.
pub const SESSION_W: f32 = 62.0;
pub const SESSION_H: f32 = 28.0;

/// The pill's near-black body. Dark enough to read as an overlay rather than a
/// widget on every desktop; the light hairline is what makes it findable on a
/// black one (fill alpha is not that dial — see #18 round 2).
pub const BODY: Rgb = Rgb(13, 13, 13);

/// The edge the pill is *found* by, drawn on every mode. Modes with something
/// to say stroke their accent over it rather than instead of it.
pub const HAIRLINE: Rgb = Rgb(220, 224, 232);

/// The nub's hairline alpha. Present enough to separate from a black desktop,
/// faint enough not to compete with what the user is doing.
const NUB_BORDER_A: f32 = 120.0;
/// The nub's fill alpha — considerably below the session pill's 245, because
/// the nub is a marker rather than a surface.
const NUB_FILL_A: f32 = 140.0;

/// The session pill's fill alpha, held over from #41.
const PILL_FILL_A: f32 = 245.0;

/// The hairline's alpha, and the floor no mode's border ever drops below. The
/// body is near-black, so on a black desktop nothing but a light edge separates
/// it — and fill alpha is not the dial that fixes that. Whatever a mode's accent
/// is doing, the pill stays findable.
pub const HAIRLINE_A: f32 = 120.0;

/// A calm, muted success green — distinct from the settings lime, not loud.
pub const SUCCESS: Rgb = Rgb(74, 188, 120);
/// A muted red for the failure flash: "that one didn't land", not an alarm.
pub const ERROR: Rgb = Rgb(214, 96, 96);
/// Neutral cool-grey for the "working" breath while the worker runs.
pub const PROCESSING: Rgb = Rgb(190, 192, 200);

/// The terminal flash's accent alpha — loud, because it is the one thing the
/// user reads peripherally while looking at where the text landed.
const FLASH_BORDER_A: f32 = 235.0;

/// The bar row's opacity once the handoff is over and the row is only being
/// held. The fall from 1 to here *is* the handoff's dimming — it is a lerp
/// between two Geoms, not a ramp applied on top of one.
const PROCESSING_BARS_A: f32 = 0.45;

/// The "working" border at the top of its breath. The bottom is
/// [`HAIRLINE_A`], which [`breathe`] floors it at.
const PROCESSING_BORDER_A: f32 = 220.0;

/// An 8-bit RGB triple. Interpolated channelwise, which is not perceptually
/// correct — but every pair the pill lerps between is either the same colour or
/// a swap under a simultaneous alpha change, where the difference is invisible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    fn lerp(self, to: Rgb, t: f32) -> Rgb {
        let c = |a: u8, b: u8| {
            (a as f32 + (b as f32 - a as f32) * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Rgb(c(self.0, to.0), c(self.1, to.1), c(self.2, to.2))
    }
}

/// Everything about how the pill looks, as numbers that can be interpolated.
///
/// One struct for every mode: if a state cannot be expressed here, the morph
/// model is wrong (#18 said so explicitly, and nothing has needed an escape
/// hatch since). Sizes are logical pixels; alphas are 0..255 for the colours
/// the renderer writes directly and 0..1 for the two opacity multipliers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Geom {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub fill: Rgb,
    pub fill_a: f32,
    pub border: Rgb,
    pub border_a: f32,
    pub border_w: f32,
    /// The bar row's opacity. 0 means no row at all — the nub has none.
    pub bars: f32,
    /// The button bar's opacity — and, since #44, its *growth progress* too:
    /// the flankers' offset from the centre island is this number times their
    /// settled offset. One field for both because the fold-out has to be one
    /// lerp with everything else rather than a second animation bolted beside
    /// it, and a stagger would need a clock the Geom cannot carry.
    pub buttons: f32,
}

impl Geom {
    /// The geometry `mode` rests at. Every transition is a lerp between two of
    /// these, so this table is the entire visual specification of the pill.
    pub fn of(mode: PillMode) -> Self {
        match mode {
            // The nub at alpha 0 — same shape, nothing drawn. That equality is
            // what makes a reveal one motion instead of two.
            PillMode::Hidden => Geom {
                fill_a: 0.0,
                border_a: 0.0,
                ..Geom::of(PillMode::Idle)
            },
            PillMode::Idle => Geom {
                w: NUB_W,
                h: NUB_H,
                radius: NUB_RADIUS,
                fill: BODY,
                fill_a: NUB_FILL_A,
                border: HAIRLINE,
                border_a: NUB_BORDER_A,
                border_w: 1.0,
                bars: 0.0,
                buttons: 0.0,
            },
            // The expanded pill is the *centre* island — the bar's other two
            // are drawn beside it by the renderer, sliding out from behind it
            // as `buttons` comes up. So the Geom that morphs is Dictate's, and
            // the nub grows into the button under the cursor rather than into
            // a body that is about to be three shapes.
            PillMode::Expanded => {
                let dictate = &crate::pill::core::BUTTONS[crate::pill::core::BUTTONS.len() / 2];
                Geom {
                    w: dictate.w,
                    h: dictate.height(),
                    radius: dictate.radius(),
                    fill_a: PILL_FILL_A,
                    border_a: HAIRLINE_A,
                    buttons: 1.0,
                    ..Geom::of(PillMode::Idle)
                }
            }
            // The bars are what say "live", so recording wears the bare
            // hairline and no accent.
            PillMode::Recording { .. } => Geom::session(),
            // The handoff's whole visual content: the neutral border arrives
            // and the row dims, over the 320 ms the bars are also falling flat.
            // The breath is layered on top of this by the adapter — it is a
            // sustained oscillation, not a transition between two states.
            PillMode::Processing { .. } => Geom {
                border: PROCESSING,
                border_a: PROCESSING_BORDER_A,
                bars: PROCESSING_BARS_A,
                ..Geom::session()
            },
            PillMode::Done { ok, .. } => Geom {
                border: if ok { SUCCESS } else { ERROR },
                border_a: FLASH_BORDER_A,
                bars: PROCESSING_BARS_A,
                ..Geom::session()
            },
        }
    }

    /// The shape every in-session mode shares: the full-size pill with a bar
    /// row, wearing the bare hairline. Recording is exactly this; Processing
    /// and Done are this with an accent and a dimmed row.
    ///
    /// A named constructor rather than `Geom::of(Recording { .. })`, which
    /// would need an [`Origin`](crate::pill::core::Origin) invented purely to
    /// reach a base shape that has nothing to do with how the session started.
    fn session() -> Self {
        Geom {
            w: SESSION_W,
            h: SESSION_H,
            radius: shape_radius(SESSION_W, SESSION_H),
            fill: BODY,
            fill_a: PILL_FILL_A,
            border: HAIRLINE,
            border_a: HAIRLINE_A,
            border_w: 1.0,
            bars: 1.0,
            buttons: 0.0,
        }
    }

    /// This shape with the lights off: same silhouette, *nothing* drawn.
    ///
    /// What a conceal aims at. Every opacity goes, not just the body's — the
    /// bar row is drawn at its own opacity and would otherwise outlive the pill
    /// it sits in, leaving a row of white marks on the desktop after the body
    /// has faded out from under them.
    ///
    /// Note `Geom::of(Hidden).blanked()` is itself — the nub is already blank —
    /// so concealing a resident pill is unchanged.
    fn blanked(self) -> Self {
        Geom {
            fill_a: 0.0,
            border_a: 0.0,
            bars: 0.0,
            buttons: 0.0,
            ..self
        }
    }

    /// `self` at `t`, on the way to `to`. `t` is already eased.
    pub fn lerp(self, to: Geom, t: f32) -> Geom {
        let f = |a: f32, b: f32| a + (b - a) * t;
        Geom {
            w: f(self.w, to.w),
            h: f(self.h, to.h),
            radius: f(self.radius, to.radius),
            fill: self.fill.lerp(to.fill, t),
            fill_a: f(self.fill_a, to.fill_a),
            border: self.border.lerp(to.border, t),
            border_a: f(self.border_a, to.border_a),
            border_w: f(self.border_w, to.border_w),
            bars: f(self.bars, to.bars),
            buttons: f(self.buttons, to.buttons),
        }
    }
}

/// The session pill's corner radius, as the shape ratio #25 settled derives it:
/// a fixed fraction of the shorter side, clamped to a full round.
fn shape_radius(w: f32, h: f32) -> f32 {
    const RADIUS_RATIO: f32 = 18.0 / 42.0;
    (w.min(h) * RADIUS_RATIO).min(h / 2.0)
}

/// How a transition's progress is shaped over its duration.
///
/// There is deliberately no overshoot here. #18 swept SPRINGY and SNAPBACK
/// against SNAPPY over live frames; overshoot on a 36x10 nub reads as a wobble
/// rather than as energy, and on the reveal it puts the pill briefly *larger*
/// than the mode it is arriving at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ease {
    Linear,
    OutCubic,
}

impl Ease {
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Linear => t,
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
        }
    }
}

/// One transition: how long it runs and how it is shaped. A zero duration is a
/// snap, which is a real answer for some pairs rather than a missing entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tween {
    pub dur: Duration,
    pub ease: Ease,
}

impl Tween {
    const SNAP: Tween = Tween {
        dur: Duration::ZERO,
        ease: Ease::Linear,
    };

    const fn out_cubic(ms: u64) -> Tween {
        Tween {
            dur: Duration::from_millis(ms),
            ease: Ease::OutCubic,
        }
    }
}

/// The reveal: nothing on screen to the nub.
pub const REVEAL: Tween = Tween::out_cubic(140);
/// The conceal: back to nothing. Quicker than the reveal — arriving wants to be
/// noticed, leaving does not.
pub const CONCEAL: Tween = Tween::out_cubic(120);
/// Into live capture. The fastest transition the pill makes: it is answering a
/// key the user is holding down right now.
pub const TO_RECORDING: Tween = Tween::out_cubic(90);
/// The handoff. Linear and long, matching [`crate::pill::core::HANDOFF`], which
/// drains the waveform over exactly the same window — the border's arrival and
/// the bars' fall are one gesture, so they share a clock and an easing.
pub const HANDOFF: Tween = Tween {
    dur: crate::pill::core::HANDOFF,
    ease: Ease::Linear,
};
/// Back to the nub after the flash retires.
pub const TO_IDLE: Tween = Tween::out_cubic(160);
/// Hover in and out: the nub growing into the bar, and collapsing back.
pub const HOVER_IN: Tween = Tween::out_cubic(110);
pub const HOVER_OUT: Tween = Tween::out_cubic(90);

/// The hover indicator moving between buttons, matching [`HOVER_OUT`] — the
/// same gesture at a smaller scale.
pub const HOVER_FADE: Tween = Tween::out_cubic(90);

/// How lit each button is this frame, and whether it can be lit at all.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Slot {
    /// The indicator's opacity behind this button, 0..1.
    pub hover: f32,
    /// A disabled button's glyph is drawn faint and its slab is inert — see
    /// [`crate::pill::core::Pill::enabled`].
    pub enabled: bool,
}

/// One [`Slot`] per button, in the button list's own order — the renderer's
/// second input beside the [`Geom`]. An array rather than a slice, so a
/// mismatched length is a compile error rather than a button quietly drawn at
/// its defaults.
pub type Slots = [Slot; crate::pill::core::BUTTON_COUNT];

/// Live, unlit — what a button is when nobody has said otherwise. Deliberately
/// not `derive`d: a defaulted `enabled: false` would draw every glyph faint.
impl Default for Slot {
    fn default() -> Self {
        Self {
            hover: 0.0,
            enabled: true,
        }
    }
}

/// Which button the cursor is on, and the fade between it and the last one.
///
/// **Deliberately outside [`Geom`].** #18's finding is that every frame derives
/// from two whole-pill Geoms, a start time and an easing; per-button hover is
/// per-button state that one Geom cannot carry. Contorting `Geom` into holding
/// it would break the very model that makes the morph one lerp — so this is a
/// second, smaller tween beside it rather than a hole in the first.
#[derive(Clone, Copy, Debug)]
pub struct Hover {
    current: Option<usize>,
    /// What the fade is coming *from*, so a slide from one button to the next
    /// dims the old one over the same 90 ms the new one lights up.
    previous: Option<usize>,
    started: Instant,
}

impl Hover {
    pub fn new(now: Instant) -> Self {
        Self {
            current: None,
            previous: None,
            started: now,
        }
    }

    /// Point the hover at `index`, and say whether that moved it.
    ///
    /// A no-op when it hasn't — restarting the fade on every `CursorMoved`
    /// would leave the indicator permanently mid-fade while the cursor wanders
    /// inside one slab. The caller reads the answer to decide whether a frame
    /// is owed, rather than testing the same thing again.
    pub fn set(&mut self, index: Option<usize>, now: Instant) -> bool {
        if index == self.current {
            return false;
        }
        self.previous = self.current;
        self.current = index;
        self.started = now;
        true
    }

    fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        HOVER_FADE
            .ease
            .apply(elapsed / HOVER_FADE.dur.as_secs_f32())
    }

    /// This frame's per-button state: the arriving button coming up, the
    /// leaving one going down, everything else dark.
    pub fn slots(&self, now: Instant, enabled: impl Fn(usize) -> bool) -> Slots {
        let t = self.progress(now);
        std::array::from_fn(|i| Slot {
            hover: if self.current == Some(i) {
                t
            } else if self.previous == Some(i) {
                1.0 - t
            } else {
                0.0
            },
            enabled: enabled(i),
        })
    }

    /// Whether the fade still has frames to draw.
    pub fn is_running(&self, now: Instant) -> bool {
        self.current != self.previous && self.progress(now) < 1.0
    }
}

/// The transition from `from` to `to`.
///
/// Read as a table, not as a cascade of special cases: what varies is which
/// *kind* of change is happening, and the pill only makes seven kinds.
pub fn transition(from: PillMode, to: PillMode) -> Tween {
    use PillMode::*;
    match (from, to) {
        // Arriving from nothing. A chord press is exempt: with residency off
        // that is the pill's entire existence beginning, and it appears at once
        // — the same instant appearance the session pill has always had.
        (Hidden, Recording { .. }) => Tween::SNAP,
        (Hidden, _) => REVEAL,
        // Leaving for nothing. The flash's exit is this, rather than a fade
        // baked into `Done` — one shape, one motion, one place it lives.
        (_, Hidden) => CONCEAL,
        (_, Recording { .. }) => TO_RECORDING,
        (Recording { .. }, Processing { .. }) => HANDOFF,
        // The outcome is news. Delaying it behind a tween would be showing the
        // user a state the app is no longer in.
        (Processing { .. }, Done { .. }) => Tween::SNAP,
        // Hover before the general return-to-idle: collapsing the button bar
        // is the same gesture as opening it, not the flash's slower resolve.
        (Idle, Expanded) => HOVER_IN,
        (Expanded, Idle) => HOVER_OUT,
        (_, Idle) => TO_IDLE,
        // Everything else is a mode the pill was not in a moment ago arriving
        // without a settled treatment — a worker resolving before Recording's
        // own tween finished, say. Snap rather than invent a motion.
        _ => Tween::SNAP,
    }
}

/// How fast the "working" border breathes, in Hz. Slow enough to read as
/// breathing rather than blinking.
const BREATH_HZ: f32 = 0.8;

/// The Processing breath, applied to an already-tweened Geom.
///
/// Not a transition — it is a sustained oscillation with no end state, so it
/// cannot be a lerp between two Geoms and does not live in the table. It rides
/// on top of whatever the morph produced, which is what lets the breath be
/// running *while* the handoff is still crossfading the border in.
///
/// It never takes the border below [`HAIRLINE_A`]: the breath is what a mode
/// says, the hairline is how the pill is found at all, and an edge that has
/// breathed down must not take the pill's silhouette with it.
pub fn breathe(geom: Geom, elapsed: Duration) -> Geom {
    let phase = elapsed.as_secs_f32() * std::f32::consts::TAU * BREATH_HZ;
    let pulse = 0.5 - 0.5 * phase.cos();
    Geom {
        border_a: (geom.border_a * (0.5 + 0.5 * pulse)).max(HAIRLINE_A),
        ..geom
    }
}

/// A transition in flight: where it started, where it is going, and when it
/// began. The adapter holds one of these and asks it for each frame's geometry.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    from: Geom,
    to: Geom,
    tween: Tween,
    started: Instant,
}

impl Motion {
    /// Start a transition from wherever the pill currently *looks*, not from
    /// the mode it was resting at. Interrupting a reveal halfway and going
    /// somewhere else has to continue from the half-revealed shape, or the
    /// pill jumps on the frame the second transition begins.
    pub fn start(from: Geom, to: PillMode, tween: Tween, now: Instant) -> Self {
        // A conceal fades what is on screen; it does not also reshape it.
        //
        // `Geom::of(Hidden)` is the *nub* at alpha 0, because that is what a
        // reveal has to grow from — but read as a conceal's target it says
        // something quite different: a session-only pill leaving would shrink
        // 62x28 down to nub size on its way out, a morph into a shape that
        // user has never seen and residency is switched off precisely to avoid.
        // For a resident pill the two readings coincide, since what is leaving
        // is already the nub.
        let to = match to {
            PillMode::Hidden => from.blanked(),
            visible => Geom::of(visible),
        };
        Self {
            from,
            to,
            tween,
            started: now,
        }
    }

    /// A pill resting at `mode`, with nothing running.
    pub fn settled(mode: PillMode, now: Instant) -> Self {
        Self {
            from: Geom::of(mode),
            to: Geom::of(mode),
            tween: Tween::SNAP,
            started: now,
        }
    }

    /// How far through the transition `now` is, 0..1. A snap is 1 immediately.
    fn progress(&self, now: Instant) -> f32 {
        if self.tween.dur.is_zero() {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        (elapsed / self.tween.dur.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// This frame's geometry.
    pub fn at(&self, now: Instant) -> Geom {
        self.from
            .lerp(self.to, self.tween.ease.apply(self.progress(now)))
    }

    /// Whether the transition still has frames left to draw. Once this is
    /// false the pill is a still image, and the adapter stops pushing pixels.
    pub fn is_running(&self, now: Instant) -> bool {
        self.progress(now) < 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pill::core::Origin;

    const REC: PillMode = PillMode::Recording {
        origin: Origin::Hotkey,
    };

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    fn proc() -> PillMode {
        PillMode::Processing { since: t(0) }
    }

    fn done(ok: bool) -> PillMode {
        PillMode::Done { ok, since: t(0) }
    }

    /// Whether a geometry puts anything at all on screen — *anything*,
    /// including the bar row, which is drawn at its own opacity and so can
    /// outlive the body it sits in.
    fn is_blank(g: &Geom) -> bool {
        g.fill_a < 0.5 && g.border_a < 0.5 && g.bars <= 0.0 && g.buttons <= 0.0
    }

    /// The nub's numbers, as settled. Pinned here rather than only on screen,
    /// so a change to one shows up as a failing number.
    #[test]
    fn the_nub_is_the_settled_shape() {
        let g = Geom::of(PillMode::Idle);
        assert_eq!((g.w, g.h), (36.0, 10.0));
        assert_eq!(g.radius, 5.0);
        assert_eq!((g.fill, g.fill_a), (Rgb(13, 13, 13), 140.0));
        assert_eq!((g.border, g.border_a), (Rgb(220, 224, 232), 120.0));
        // No bars. The nub says one thing — Draft is on — and a bar row would
        // make it look like it was listening.
        assert_eq!(g.bars, 0.0);
        assert_eq!(g.buttons, 0.0);
    }

    /// The single fact the whole reveal rests on: `Hidden` is not "no pill", it
    /// is the nub with the lights off. Revealing is therefore one motion.
    #[test]
    fn hidden_is_the_nub_at_zero_alpha() {
        let hidden = Geom::of(PillMode::Hidden);
        let idle = Geom::of(PillMode::Idle);
        assert_eq!(
            (hidden.w, hidden.h, hidden.radius),
            (idle.w, idle.h, idle.radius)
        );
        assert_eq!(hidden.fill_a, 0.0);
        assert_eq!(hidden.border_a, 0.0);
        assert!(is_blank(&hidden));
        assert!(!is_blank(&idle));
    }

    /// Every mode has to fit the window the pill is created at, because the
    /// window is never resized to run an animation.
    #[test]
    fn no_mode_exceeds_the_envelope() {
        for mode in [
            PillMode::Hidden,
            PillMode::Idle,
            PillMode::Expanded,
            REC,
            proc(),
            done(true),
        ] {
            let g = Geom::of(mode);
            assert!(
                g.w <= ENVELOPE_W as f32,
                "{mode:?} is wider than the window"
            );
            assert!(
                g.h <= ENVELOPE_H as f32,
                "{mode:?} is taller than the window"
            );
        }
    }

    /// The bar is the largest thing the pill draws, so the envelope has to hold
    /// it — with room over for the inert end padding and the outermost island's
    /// anti-aliased edge.
    #[test]
    fn the_envelope_holds_the_whole_button_bar() {
        use crate::pill::core::{bar_width, BAR_H};
        assert!(bar_width() < ENVELOPE_W as f32, "{}", bar_width());
        assert!(BAR_H < ENVELOPE_H as f32);
        // And the session pill, which stopped being the envelope when the bar
        // grew past it.
        assert!(SESSION_W < ENVELOPE_W as f32 && SESSION_H < ENVELOPE_H as f32);
    }

    /// The nub grows into the button under the cursor, not into a body that is
    /// about to be three shapes: `Expanded`'s Geom is the centre island's.
    #[test]
    fn the_expanded_geom_is_the_centre_island() {
        use crate::pill::core::BUTTONS;
        let g = Geom::of(PillMode::Expanded);
        let dictate = &BUTTONS[BUTTONS.len() / 2];
        assert_eq!((g.w, g.h, g.radius), (48.0, 32.0, 16.0));
        assert_eq!((g.w, g.h), (dictate.w, dictate.height()));
        assert_eq!(g.buttons, 1.0);
        // And it carries the session pill's surface, not the nub's marker
        // alphas — it is a thing to click, not a thing to notice.
        assert!(g.fill_a > Geom::of(PillMode::Idle).fill_a);
    }

    /// The flankers' offset is a pure function of the growth progress, and
    /// `buttons` is that progress — which is what keeps the fold-out inside the
    /// derived-frame model instead of being a second animation beside it.
    #[test]
    fn the_fold_out_rides_the_same_lerp_as_the_growth() {
        let m = Motion::start(Geom::of(PillMode::Idle), PillMode::Expanded, HOVER_IN, t(0));
        assert_eq!(m.at(t(0)).buttons, 0.0);
        assert_eq!(m.at(t(110)).buttons, 1.0);
        // Monotone all the way, so nothing slides back on the way out.
        let mut prev = 0.0;
        for ms in 0..=110 {
            let b = m.at(t(ms)).buttons;
            assert!(b >= prev, "buttons went backwards at {ms}ms");
            prev = b;
        }
    }

    /// Sliding from one button to the next is one crossfade: the arriving
    /// button comes up over exactly the 90 ms the leaving one goes down.
    #[test]
    fn the_hover_indicator_crossfades_between_buttons() {
        let all_live = |_| true;
        let mut h = Hover::new(t(0));
        assert!(h.slots(t(0), all_live).iter().all(|s| s.hover == 0.0));

        h.set(Some(1), t(0));
        assert!(h.is_running(t(0)));
        assert_eq!(h.slots(t(0), all_live)[1].hover, 0.0);
        assert_eq!(h.slots(t(90), all_live)[1].hover, 1.0);
        assert!(!h.is_running(t(90)));

        // On to the next button: the two move together.
        h.set(Some(2), t(90));
        let mid = h.slots(t(135), all_live);
        assert!((mid[1].hover + mid[2].hover - 1.0).abs() < 1e-5, "{mid:?}");
        assert!(mid[1].hover > 0.0 && mid[2].hover > 0.0);
        let done = h.slots(t(180), all_live);
        assert_eq!((done[1].hover, done[2].hover), (0.0, 1.0));

        // And off the bar entirely: everything goes dark.
        h.set(None, t(180));
        assert!(h.slots(t(270), all_live).iter().all(|s| s.hover == 0.0));
    }

    /// A cursor wandering inside one slab is not a change. Restarting the fade
    /// on every `CursorMoved` would leave the indicator permanently mid-fade.
    #[test]
    fn re_hovering_the_same_button_does_not_restart_the_fade() {
        let mut h = Hover::new(t(0));
        h.set(Some(0), t(0));
        h.set(Some(0), t(45));
        assert_eq!(h.slots(t(90), |_| true)[0].hover, 1.0);
        assert!(!h.is_running(t(90)));
    }

    /// The durations, as the ticket states them. A table test rather than six
    /// assertions, because the point is the whole table.
    #[test]
    fn the_transition_table_is_the_settled_one() {
        let cases = [
            (PillMode::Hidden, PillMode::Idle, REVEAL),
            (PillMode::Idle, PillMode::Hidden, CONCEAL),
            (done(true), PillMode::Hidden, CONCEAL),
            (PillMode::Idle, REC, TO_RECORDING),
            (PillMode::Expanded, REC, TO_RECORDING),
            (REC, proc(), HANDOFF),
            (proc(), done(true), Tween::SNAP),
            (done(true), PillMode::Idle, TO_IDLE),
            (PillMode::Idle, PillMode::Expanded, HOVER_IN),
            (PillMode::Expanded, PillMode::Idle, HOVER_OUT),
        ];
        for (from, to, expected) in cases {
            assert_eq!(transition(from, to), expected, "{from:?} -> {to:?}");
        }
        assert_eq!(REVEAL.dur, Duration::from_millis(140));
        assert_eq!(CONCEAL.dur, Duration::from_millis(120));
        assert_eq!(TO_RECORDING.dur, Duration::from_millis(90));
        assert_eq!(TO_IDLE.dur, Duration::from_millis(160));
        assert_eq!(HANDOFF.dur, Duration::from_millis(320));
        assert_eq!(HANDOFF.ease, Ease::Linear);
    }

    /// The handoff's border crossfade and the core's waveform drain are one
    /// gesture. If these two clocks ever diverged, the bars would reach flat
    /// while the border was still arriving.
    #[test]
    fn the_handoff_tween_and_the_cores_drain_share_a_clock() {
        assert_eq!(HANDOFF.dur, crate::pill::core::HANDOFF);
    }

    /// With residency off, a chord press is the pill's whole existence
    /// beginning — it appears at once, exactly as the session pill always has.
    #[test]
    fn a_chord_press_from_nothing_does_not_animate_in() {
        assert_eq!(transition(PillMode::Hidden, REC), Tween::SNAP);
    }

    /// The one motion rule with no exceptions: nothing ever goes past where it
    /// is heading. An overshoot on a 36x10 nub reads as a wobble.
    #[test]
    fn no_easing_overshoots() {
        for ease in [Ease::Linear, Ease::OutCubic] {
            for step in 0..=100 {
                let e = ease.apply(step as f32 / 100.0);
                assert!((0.0..=1.0).contains(&e), "{ease:?} at {step}%: {e}");
            }
            // And it is monotone: no easing doubles back either.
            let mut prev = 0.0;
            for step in 0..=100 {
                let e = ease.apply(step as f32 / 100.0);
                assert!(e >= prev, "{ease:?} went backwards at {step}%");
                prev = e;
            }
            assert_eq!(ease.apply(0.0), 0.0);
            assert_eq!(ease.apply(1.0), 1.0);
            // Past the end, and before the start, both clamp.
            assert_eq!(ease.apply(2.0), 1.0);
            assert_eq!(ease.apply(-1.0), 0.0);
        }
    }

    /// Out-cubic, not linear: most of the distance is covered early, so a
    /// 140 ms reveal reads as arriving rather than as sliding.
    #[test]
    fn out_cubic_front_loads_the_motion() {
        assert!(Ease::OutCubic.apply(0.5) > 0.8);
        assert!(Ease::OutCubic.apply(0.25) > Ease::Linear.apply(0.25));
    }

    /// A geometry that never overshoots also never leaves the two Geoms it is
    /// between — the pill cannot be narrower than the nub or wider than the
    /// mode it is growing into at any point in a transition.
    #[test]
    fn a_transition_stays_between_its_endpoints() {
        let m = Motion::start(Geom::of(PillMode::Idle), REC, REVEAL, t(0));
        let (from, to) = (Geom::of(PillMode::Idle), Geom::of(REC));
        for ms in 0..=200 {
            let g = m.at(t(ms));
            assert!(g.w >= from.w && g.w <= to.w, "at {ms}ms: w {}", g.w);
            assert!(g.h >= from.h && g.h <= to.h, "at {ms}ms: h {}", g.h);
            assert!(g.bars >= 0.0 && g.bars <= 1.0, "at {ms}ms: bars {}", g.bars);
        }
    }

    #[test]
    fn a_motion_runs_for_its_duration_and_then_holds() {
        let m = Motion::start(Geom::of(PillMode::Hidden), PillMode::Idle, REVEAL, t(0));
        assert_eq!(m.at(t(0)), Geom::of(PillMode::Hidden));
        assert!(m.is_running(t(0)));
        assert!(m.is_running(t(139)));
        assert!(!m.is_running(t(140)));
        assert_eq!(m.at(t(140)), Geom::of(PillMode::Idle));
        // And it stays there rather than continuing past the target.
        assert_eq!(m.at(t(5000)), Geom::of(PillMode::Idle));
    }

    /// A snap has no frames to draw at all — not even the first one.
    #[test]
    fn a_snap_is_already_over_when_it_starts() {
        let m = Motion::start(Geom::of(proc()), done(true), Tween::SNAP, t(0));
        assert!(!m.is_running(t(0)));
        assert_eq!(m.at(t(0)), Geom::of(done(true)));
    }

    /// A pill that is not transitioning is a still image, so the adapter can
    /// stop pushing pixels entirely — the whole idle-cost argument.
    #[test]
    fn a_settled_pill_has_nothing_left_to_draw() {
        let m = Motion::settled(PillMode::Idle, t(0));
        assert!(!m.is_running(t(0)));
        assert_eq!(m.at(t(10_000)), Geom::of(PillMode::Idle));
    }

    /// Interrupting a transition continues from what is on screen, not from
    /// the mode it set out from — otherwise the pill jumps on the frame the
    /// second transition begins.
    #[test]
    fn an_interrupted_transition_continues_from_where_it_looks() {
        let reveal = Motion::start(Geom::of(PillMode::Hidden), PillMode::Idle, REVEAL, t(0));
        let mid = reveal.at(t(70));
        assert!(mid.fill_a > 0.0 && mid.fill_a < NUB_FILL_A, "{mid:?}");
        let interrupted = Motion::start(mid, REC, TO_RECORDING, t(70));
        assert_eq!(interrupted.at(t(70)), mid);
        assert_eq!(interrupted.at(t(160)), Geom::of(REC));
    }

    /// The flash resolves *to* something rather than fading in place: with
    /// residency on that is the nub, and both endpoints are real geometry.
    #[test]
    fn the_flash_returns_to_the_nub_rather_than_to_nothing() {
        let m = Motion::start(Geom::of(done(true)), PillMode::Idle, TO_IDLE, t(0));
        let end = m.at(t(160));
        assert_eq!(end, Geom::of(PillMode::Idle));
        assert!(
            !is_blank(&end),
            "the pill is still on screen after a session"
        );
        // Halfway, it is genuinely between the two — a morph, not a cut.
        let mid = m.at(t(80));
        assert!(mid.w > Geom::of(PillMode::Idle).w && mid.w < Geom::of(done(true)).w);
    }

    /// And with residency off it resolves to nothing, over the conceal — the
    /// exit is the same one motion, aimed at a blank Geom.
    #[test]
    fn the_flash_conceals_to_blank_when_there_is_no_nub_to_return_to() {
        let m = Motion::start(Geom::of(done(false)), PillMode::Hidden, CONCEAL, t(0));
        assert!(!is_blank(&m.at(t(0))));
        assert!(is_blank(&m.at(t(120))));
    }

    /// A conceal fades in place. With residency off the pill leaving has never
    /// been a nub, and shrinking it into one on the way out would be a morph
    /// into a shape that user has never seen — the exit residency is switched
    /// off to keep unchanged.
    #[test]
    fn a_conceal_fades_without_reshaping_what_is_leaving() {
        let full = Geom::of(done(true));
        let m = Motion::start(full, PillMode::Hidden, CONCEAL, t(0));
        for ms in 0..=120 {
            let g = m.at(t(ms));
            assert_eq!((g.w, g.h), (full.w, full.h), "the pill resized at {ms}ms");
        }
    }

    /// The bar row fades *with* the pill, not after it. It is drawn at its own
    /// opacity, so a conceal that only took the body's alphas down left a row
    /// of white marks sitting on the desktop with nothing around them.
    #[test]
    fn a_conceal_takes_the_bar_row_with_it() {
        let m = Motion::start(Geom::of(done(false)), PillMode::Hidden, CONCEAL, t(0));
        // The row is on the way out the whole time, and gone at the end.
        let start = m.at(t(0)).bars;
        assert!(start > 0.0, "the flash draws a bar row to begin with");
        for ms in 0..=120 {
            let g = m.at(t(ms));
            assert!(
                g.bars <= start,
                "at {ms}ms the row was brighter than the flash's own: {}",
                g.bars
            );
            // It never outlives the body it sits in.
            assert!(
                g.bars <= 0.0 || g.fill_a > 0.0 || g.border_a > 0.0,
                "at {ms}ms the bars outlived the pill"
            );
        }
        assert_eq!(m.at(t(120)).bars, 0.0);
    }

    /// The same rule read the other way: a resident pill's conceal *is* the nub
    /// fading, because that is what was on screen. Both cases are one rule.
    #[test]
    fn concealing_a_resident_pill_is_still_the_nub() {
        let m = Motion::start(Geom::of(PillMode::Idle), PillMode::Hidden, CONCEAL, t(0));
        assert_eq!(m.at(t(120)), Geom::of(PillMode::Hidden));
    }

    /// Green and red have to be unmistakable at a glance — they are read
    /// peripherally, while the user is looking at where the text landed.
    #[test]
    fn the_two_flashes_stay_far_apart() {
        let ok = Geom::of(done(true)).border;
        let bad = Geom::of(done(false)).border;
        assert!(ok.1 > ok.0 + 40, "success reads green: {ok:?}");
        assert!(bad.0 > bad.1 + 40, "error reads red: {bad:?}");
    }

    /// The handoff is 320 ms of animation, not a swap with animation either
    /// side of it: at the mode change, Processing has to *be* Recording.
    #[test]
    fn the_handoff_starts_looking_exactly_like_recording() {
        let m = Motion::start(Geom::of(REC), proc(), HANDOFF, t(0));
        assert_eq!(m.at(t(0)), Geom::of(REC));
        assert_eq!(m.at(t(320)), Geom::of(proc()));
    }

    /// The breath is what a mode says; the hairline is how the pill is found.
    /// At the dimmest point of the breath the edge is still the hairline —
    /// including mid-handoff, where the tweened border is at its faintest.
    #[test]
    fn the_breath_never_dims_the_pill_below_its_hairline() {
        let handoff = Motion::start(Geom::of(REC), proc(), HANDOFF, t(0));
        for ms in 0..=1200 {
            let base = if ms <= 320 {
                handoff.at(t(ms))
            } else {
                Geom::of(proc())
            };
            let g = breathe(base, Duration::from_millis(ms));
            assert!(
                g.border_a >= HAIRLINE_A,
                "at {ms}ms the edge breathed down to {}",
                g.border_a
            );
        }
    }

    /// And it actually breathes: a full cycle reaches both ends rather than
    /// hovering near one of them.
    #[test]
    fn the_breath_swings_over_its_cycle() {
        let settled = Geom::of(proc());
        let alphas: Vec<f32> = (0..=1250)
            .map(|ms| breathe(settled, Duration::from_millis(ms)).border_a)
            .collect();
        let lo = alphas.iter().cloned().fold(f32::INFINITY, f32::min);
        let hi = alphas.iter().cloned().fold(0.0_f32, f32::max);
        assert_eq!(lo, HAIRLINE_A);
        assert!(hi > 200.0, "the breath never reaches full: {hi}");
    }

    /// The breath is the one thing the Geom table does not own, so it must not
    /// touch anything else — a modulation of the edge, not of the shape.
    #[test]
    fn the_breath_changes_nothing_but_the_edge() {
        let g = Geom::of(proc());
        let b = breathe(g, Duration::from_millis(400));
        assert_eq!(
            Geom {
                border_a: g.border_a,
                ..b
            },
            g
        );
    }
}
