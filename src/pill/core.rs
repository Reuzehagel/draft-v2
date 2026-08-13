// Pill core — the pill's whole life as a pure state machine, peer to `Session`.
//
// `Session` used to author the pill: it emitted `SetPill`/`DismissPill` and held
// the terminal flash in its own phase. That only works while the pill's life *is*
// the session's life, which the resident pill breaks. So the pill gets its own
// core and `Session` becomes one of its drivers — the residency toggle, the
// fullscreen watcher and the hover poller are the others (see ADR-0003).
//
// State is two axes, never one flat enum:
//
//   Presence — what the pill does with no session running
//   Activity — what a session is asking it to show
//
// **Activity outranks presence.** That single rule buys two decisions without
// transition bookkeeping: a session stays visible even behind a fullscreen app,
// and hover cannot expand a recording pill. It also means the state a session
// returns to is *derived* from presence, never remembered — a remembered
// return-state is already wrong if presence changed while the session ran.
//
// Pure, like `Session`: events plus `now: Instant` in, `Command`s out. No winit,
// no Win32, no `Instant::now()`. The races this exists to make assertable —
// fullscreen hide against a chord press, hover during a session, residency
// toggled mid-flash — are unreachable by tests if they live in the adapter.

use std::time::{Duration, Instant};

/// How long the pill lingers on a successful delivery, showing the green
/// border before it fades and disappears.
pub const SUCCESS_LINGER: Duration = Duration::from_millis(500);

/// Failures linger longer than successes — a red flash the user might miss in
/// 500 ms deserves an extra beat to register as "that one didn't land".
pub const ERROR_LINGER: Duration = Duration::from_millis(1200);

/// How long the Recording → Processing handoff runs. The bars ease to flat over
/// this long instead of freezing: flat is a state — "stopped listening" — where
/// a frozen height is an accident of when the key happened to be released.
pub const HANDOFF: Duration = Duration::from_millis(320);

/// The live waveform's remaining share, `elapsed` into the handoff — 1 at the
/// mode change, 0 once it is over.
///
/// Linear, not eased: energy draining at a steady rate reads as the pill losing
/// interest, where an ease-out reads as the bars being dragged down by hand.
pub fn handoff_damping(elapsed: Duration) -> f32 {
    let t = elapsed.as_secs_f32() / HANDOFF.as_secs_f32();
    (1.0 - t).clamp(0.0, 1.0)
}

/// How long the terminal flash holds before the pill leaves. Shared with the
/// pill adapter, which uses it to time the fade; the core uses it in `tick` to
/// decide when the flash retires.
pub fn linger(ok: bool) -> Duration {
    if ok {
        SUCCESS_LINGER
    } else {
        ERROR_LINGER
    }
}

/// What the pill does when no session is running.
///
/// `expanded` is a flag inside `Resident` rather than a third axis, because
/// expansion is meaningless when the pill is off or suppressed.
///
/// The residency toggle sets `Off` and `Resident`, the hover poll sets
/// `expanded` on the latter (#44), and the fullscreen watcher sets `Suppressed`
/// (#45). All three are composed into one value by the adapter, which is what
/// keeps the three drivers from overwriting each other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Presence {
    /// Residency toggled off — the pill exists only for the length of a session.
    Off,
    /// A fullscreen app has focus. Suppresses the *resident* pill only, never
    /// session feedback.
    Suppressed,
    /// On screen with nothing happening; `expanded` is set by hover.
    Resident { expanded: bool },
}

/// How a session began. Decides presentation only, never control: a hotkey
/// release finishes a click-started session exactly as its check would.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Hotkey,
    /// The pill's own Dictate button. A mouse-started session has no "release
    /// the key" gesture, so it carries its own cancel and confirm (#47).
    Click,
}

/// What clicking a button does. The adapter performs it; the core only ever
/// says which one, so the whole of "what is under the cursor and is it live"
/// stays assertable without a mouse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Put the last transcript back on the clipboard — the recovery path for a
    /// paste that landed in the wrong window, not "get the text".
    Copy,
    /// Start a click-started session, routed through the same `Session`
    /// lifecycle a chord takes — same capture, same transcription, same paste.
    Dictate,
    /// Launch the settings subprocess, exactly as the tray item does.
    Settings,
    /// Throw the click-started session's audio away: no transcription, no
    /// paste, no history entry and no flash. A flash reports an outcome, and
    /// cancelling isn't one.
    Cancel,
    /// Finish the click-started session. Identical to releasing the hotkey.
    Confirm,
}

/// Which glyph a button wears. Lucide, resolved to a path by
/// [`crate::pill::icons`] — the core names the icon and knows nothing about
/// how it is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Icon {
    Copy,
    Mic,
    Sliders,
    X,
    Check,
}

/// One button of the bar: what it does, what it wears, what it is called, and
/// how wide it is.
///
/// Width is a field rather than a constant because the three are not the same
/// size — and **the glyph box is not the button**: [`GLYPH_BOX`] is 22 in all
/// three regardless of the island they sit in, which is what makes the centre
/// button read as a wider target rather than a bigger icon.
pub struct Button {
    pub action: Action,
    pub icon: Icon,
    /// What the label says while the cursor is on it (#46). Held here rather
    /// than in a table beside the list, for the same reason everything else
    /// about the bar is: a fourth button is one edit, in one place.
    pub name: &'static str,
    /// The island's width in logical pixels. Its height is derived — see
    /// [`Button::height`].
    pub w: f32,
}

impl Button {
    /// An island is as tall as it is wide, capped at the bar's height. One
    /// expression, and both shapes fall out of it: a 32 button is a circle, a
    /// 48 button is a stadium.
    pub fn height(&self) -> f32 {
        self.w.min(BAR_H)
    }

    /// Fully rounded at its own height, at every width.
    pub fn radius(&self) -> f32 {
        self.height() / 2.0
    }
}

/// The bar's height, and the cap every island's height is taken against.
pub const BAR_H: f32 = 32.0;

/// Bare desktop between islands. There is no enclosing body to gap *within*:
/// the expanded pill is three separate shapes with the desktop showing between
/// them.
pub const BAR_GAP: f32 = 3.0;

/// The 24-unit grid every glyph is drawn in, as a box in logical pixels. The
/// same in all three buttons — see [`Button`].
pub const GLYPH_BOX: f32 = 22.0;

/// The button set, in the order it is laid out. **This list is the button bar**:
/// the bar's width, the islands, the hit slabs, the glyph lookup and the click
/// action are all derived from it, so adding a fourth button is an edit here
/// and nowhere else.
///
/// The count must stay odd — see
/// [`the_button_count_stays_odd`](tests::the_button_count_stays_odd), which is
/// the one place that invariant is stated rather than baked into the layout.
pub const BUTTON_COUNT: usize = 3;

/// The centre button's index — the one the pill's body *is* while expanded, and
/// the one the flankers fold out from behind. Derived rather than written down:
/// the odd count is what makes it exist at all.
pub const CENTRE: usize = BUTTON_COUNT / 2;

pub const BUTTONS: [Button; BUTTON_COUNT] = [
    Button {
        action: Action::Copy,
        icon: Icon::Copy,
        // The tray's wording is "Copy last transcription"; this is the same
        // recovery path said shorter, because the label is one line over a
        // 36px nub rather than a menu item with a menu's width.
        name: "Copy last transcript",
        w: 32.0,
    },
    Button {
        action: Action::Dictate,
        icon: Icon::Mic,
        name: "Dictate",
        w: 48.0,
    },
    Button {
        action: Action::Settings,
        icon: Icon::Sliders,
        name: "Settings",
        w: 32.0,
    },
];

/// The bar's total width: every island plus the desktop between them.
pub fn bar_width() -> f32 {
    BUTTONS.iter().map(|b| b.w).sum::<f32>() + BAR_GAP * (BUTTONS.len() - 1) as f32
}

/// Button `i`'s centre, as an offset from the pill's centre. Negative is left.
///
/// The bar is centred on the pill, which — with an odd count — is what puts
/// Dictate exactly under the cursor that opened it.
pub fn island_centre(i: usize) -> f32 {
    let mut x = -bar_width() / 2.0;
    for b in &BUTTONS[..i] {
        x += b.w + BAR_GAP;
    }
    x + BUTTONS[i].w / 2.0
}

/// Button `i`'s hit region: **a slab, not a circle**. Full bar height, spanning
/// the island plus half the gap on each side, so the desktop between two
/// islands belongs to one of them rather than being a dead zone the hover
/// flickers off in.
///
/// The outer ends get no half-gap: past the last island is the end padding,
/// which is inert — it holds the pill expanded with nothing lit.
pub fn slab(i: usize) -> (f32, f32) {
    let c = island_centre(i);
    let half = BUTTONS[i].w / 2.0;
    let lo = if i == 0 { 0.0 } else { BAR_GAP / 2.0 };
    let hi = if i + 1 == BUTTONS.len() {
        0.0
    } else {
        BAR_GAP / 2.0
    };
    (c - half - lo, c + half + hi)
}

/// Which button `(x, y)` — an offset from the pill's centre, in logical pixels
/// — is over, disregarding whether it is live.
///
/// The vertical half is what "full bar height" actually means, and it is
/// checked rather than assumed: until #46 the window *was* the bar's height and
/// there was nowhere else to be, but the envelope now holds a label above the
/// pill, and a cursor up there is over the window without being over a button.
fn slab_at(x: f32, y: f32) -> Option<usize> {
    if y.abs() > BAR_H / 2.0 {
        return None;
    }
    (0..BUTTONS.len()).find(|&i| {
        let (lo, hi) = slab(i);
        x >= lo && x < hi
    })
}

// --- The click-started pill ---------------------------------------------
//
// A mouse-started session has no "release the key" gesture, so it carries the
// two buttons that gesture stood for. They are **not the button bar**: the bar
// is three islands over bare desktop, and this is one body with two discs
// inside it — the pill a session is running in, not a menu.
//
// Everything below is a number the ticket states, and the layout is asserted
// against the sum it was stated as:
//
//     7 + 20 + 12 + 34 bars + 12 + 20 + 7 = 112

/// The click-started pill's body: **always one**, never the bar's three
/// islands.
pub const CLICK_W: f32 = 112.0;
pub const CLICK_H: f32 = 32.0;

/// Clear space between the body's edge and the outer edge of a button.
pub const CHECK_PAD: f32 = 7.0;

/// Cancel and confirm are 20px circles — **smaller than any hover-bar button**,
/// because these sit *inside* a body rather than being one.
pub const CHECK_BUTTON: f32 = 20.0;

/// Clear space between a button and the bar row it flanks.
pub const CHECK_GAP: f32 = 12.0;

/// What one button claims of the body's half-width: itself, its padding, and
/// the clear space between it and the waveform.
///
/// The one number the renderer needs to place both the discs and the row, so
/// it is derived here rather than reconstructed there — the layout changes in
/// one file or it drifts between two.
pub const CHECK_CLAIM: f32 = CHECK_PAD + CHECK_BUTTON + CHECK_GAP;

/// The glyph box cancel and confirm draw in — the button itself, unlike the
/// bar's, whose [`GLYPH_BOX`] is smaller than every island it sits in.
///
/// These are 20px discs inside a 32px body rather than islands *being* the
/// body, so there is no island padding to hold a mark clear of: the disc's own
/// edge is what does that, and a box smaller than the disc would leave a 20px
/// circle with a 14px mark rattling around in it.
pub const CHECK_GLYPH_BOX: f32 = CHECK_BUTTON;

/// Button `i`'s centre inside a body `body_w` wide, as an offset from the
/// pill's centre. Negative is left, which is cancel.
///
/// Takes the width rather than assuming [`CLICK_W`] because the renderer asks
/// it about a body mid-morph: measured inward from whatever edge the body has
/// this frame, the pair sit under the bar's centre island at the start of the
/// fold and at their settled places by the end. One derivation, two callers.
pub fn check_centre(body_w: f32, i: usize) -> f32 {
    let x = (body_w / 2.0 - CHECK_PAD - CHECK_BUTTON / 2.0).max(0.0);
    if i == 0 {
        -x
    } else {
        x
    }
}

/// The two buttons, in the order they are laid out. Cancel on the left,
/// confirm on the right — the destructive one furthest from the confirm the
/// user is reaching for.
///
/// A second list rather than a mode on the first: these are a different size,
/// a different shape and a different job, and folding them into [`BUTTONS`]
/// would put "which of the two lists am I in" inside every derivation the bar
/// makes off it.
pub const CHECK_BUTTONS: [Button; 2] = [
    Button {
        action: Action::Cancel,
        icon: Icon::X,
        name: "Cancel",
        w: CHECK_BUTTON,
    },
    Button {
        action: Action::Confirm,
        icon: Icon::Check,
        name: "Confirm",
        w: CHECK_BUTTON,
    },
];

/// Button `i`'s hit region, on the bar's own doctrine: full body height,
/// spanning the button plus half the gap on its *inner* side, so the desktop
/// between a button and the waveform belongs to the button rather than being a
/// dead zone. The outer ends stop at the button — past it is [`CHECK_PAD`],
/// which is inert.
///
/// Always at the settled width: a hit region is only ever asked about a pill
/// that has arrived, since the check is not clickable mid-morph.
pub fn check_slab(i: usize) -> (f32, f32) {
    let c = check_centre(CLICK_W, i);
    let half = CHECK_BUTTON / 2.0;
    if i == 0 {
        (c - half, c + half + CHECK_GAP / 2.0)
    } else {
        (c - half - CHECK_GAP / 2.0, c + half)
    }
}

/// Which of the two `(x, y)` — an offset from the pill's centre, in logical
/// pixels — is over.
fn check_slab_at(x: f32, y: f32) -> Option<usize> {
    if y.abs() > CLICK_H / 2.0 {
        return None;
    }
    (0..CHECK_BUTTONS.len()).find(|&i| {
        let (lo, hi) = check_slab(i);
        x >= lo && x < hi
    })
}

/// What a session is currently asking the pill to show. Outranks [`Presence`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Activity {
    /// No session — the pill is whatever presence says it is.
    None,
    Recording {
        origin: Origin,
    },
    Processing {
        since: Instant,
    },
    /// The terminal flash, retired by [`Pill::tick`] once its linger elapses.
    Done {
        ok: bool,
        since: Instant,
    },
}

/// The pill's logical mode: what the core derives from presence × activity and
/// hands the adapter. One mode per transition; the adapter derives every frame's
/// bars, breathing pulse and fade from it. `since` is the core's `now` at the
/// transition, so the adapter's animation clock and the core's linger clock
/// share one origin.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PillMode {
    /// Off screen. The window may still exist (residency keeps it alive).
    Hidden,
    /// Resident and idle: the nub.
    Idle,
    /// Resident and hovered.
    Expanded,
    /// Live capture: the adapter animates bars from the ring buffer.
    Recording { origin: Origin },
    /// Worker running: the bars fall flat over the handoff, then hold there
    /// under a breathing border.
    Processing { since: Instant },
    /// Terminal green/red flash.
    Done { ok: bool, since: Instant },
}

impl PillMode {
    /// Whether this mode puts anything clickable on screen — which is also the
    /// whole of the click-through rule: `WS_EX_TRANSPARENT` is off exactly
    /// while this is true.
    ///
    /// Two modes qualify, and they are the two the origin distinction exists
    /// for: the hovered bar, and a session the user started with the mouse. A
    /// **hotkey** session shows nothing to press, so a click passes straight
    /// through it to the app being dictated into — the bare recording pill is
    /// not a stop target and is not clickable at all.
    pub fn shows_buttons(self) -> bool {
        self.shows_bar() || self.shows_check()
    }

    /// Whether this mode puts the three-island [`BUTTONS`] bar on screen.
    /// Narrower than [`Self::shows_buttons`]: the click-started pill is
    /// clickable without being the bar, and everything derived from the bar's
    /// list — slabs, hover, the label's names — has to follow this one.
    pub fn shows_bar(self) -> bool {
        self == PillMode::Expanded
    }

    /// Whether this mode carries cancel and confirm. **Recording only**: they
    /// do not survive into Processing, and Processing and Done render
    /// identically whatever started the session.
    pub fn shows_check(self) -> bool {
        matches!(
            self,
            PillMode::Recording {
                origin: Origin::Click
            }
        )
    }
}

/// What a session tells the Pill core it is doing. Deliberately smaller than
/// [`Activity`]: a session knows nothing about residency, and it does not stamp
/// the flash's clock — the core does, because the flash outlives the session.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SessionActivity {
    /// No session is asking for anything (never started, dropped, or nothing
    /// usable was said).
    None,
    Recording {
        origin: Origin,
    },
    Processing {
        since: Instant,
    },
    /// The last thing a session says. What follows the flash is the core's call.
    Finished {
        ok: bool,
    },
}

/// An effect for the pill adapter to perform. The core returns these; it never
/// performs them, so a test can assert on the list instead of on private state.
///
/// The window is created once and shown/hidden, and destroyed only when the pill
/// has no reason to exist at all — create-on-demand would churn a layered window
/// on every chord press and every hover, and hover polling wants a stable rect.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Command {
    /// Create the pill window, hidden and unpainted.
    Create,
    /// Hand over a new logical mode. Always precedes the `Show` that reveals it,
    /// so the first visible frame is already the right one.
    SetMode(PillMode),
    Show,
    Hide,
    /// Tear the window down.
    Destroy,
}

/// The pill's owner: presence × activity in, commands out.
pub struct Pill {
    presence: Presence,
    activity: Activity,
    /// Whether the adapter currently holds a window (mirrors Create/Destroy).
    window: bool,
    /// Whether that window is on screen (mirrors Show/Hide).
    shown: bool,
    /// The last mode handed over, so unchanged modes emit nothing.
    mode: Option<PillMode>,
    /// Whether there is a transcript to copy. Fed by the adapter at the same
    /// events that refresh the tray's "Copy last transcription" item, because
    /// it is the same fact: with nothing recorded, Copy is disabled rather than
    /// silently no-op'ing.
    has_history: bool,
}

impl Pill {
    pub fn new() -> Self {
        Self {
            // Session-scoped until the adapter has read the config and can act
            // on the answer — which it cannot before there is an event loop to
            // create a window on. A pill that is `Off` for those first moments
            // costs nothing; one that assumed residency would have to be taken
            // back off screen for the user who turned it off.
            presence: Presence::Off,
            activity: Activity::None,
            window: false,
            shown: false,
            mode: None,
            has_history: false,
        }
    }

    /// Tell the core whether anything has ever been transcribed. Emits no
    /// commands: it changes what a button *does*, never what mode the pill is
    /// in, and the glyph that dims for it is derived per frame.
    pub fn set_has_history(&mut self, has_history: bool) {
        self.has_history = has_history;
    }

    /// Whether the pill is showing its button bar — see
    /// [`PillMode::shows_bar`], which is the predicate.
    ///
    /// The *bar*, deliberately, not everything clickable. The adapter asks
    /// this to size the region that keeps the pill expanded, and that question
    /// is only ever about the bar: a click-started session is a mouse target
    /// without being one, and asking for the bar's reach around it would hold
    /// a claim open that the session is about to hand back.
    pub fn showing_bar(&self) -> bool {
        self.derive_mode().shows_bar()
    }

    /// Whether button `i` is live. Only Copy is ever disabled, and only with an
    /// empty history.
    pub fn enabled(&self, i: usize) -> bool {
        match BUTTONS[i].action {
            Action::Copy => self.has_history,
            _ => true,
        }
    }

    /// Which *bar* button the cursor is over, `(x, y)` being its offset from
    /// the pill's centre in logical pixels.
    ///
    /// `None` for the end padding, for the label's band above the bar, for a
    /// disabled button — whose slab is inert, not merely unclickable — and for
    /// a pill that is not showing the bar at all, which is what keeps a stray
    /// `CursorMoved` from lighting anything mid-dictation.
    ///
    /// Deliberately the *bar's* index and nothing else: it is what the hover
    /// indicator and the label are indexed by, and a click-started session's
    /// two buttons are a different list. What can be *pressed* is
    /// [`Self::action_at`], which is the wider question.
    pub fn button_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.showing_bar() {
            return None;
        }
        slab_at(x, y).filter(|&i| self.enabled(i))
    }

    /// What clicking at `(x, y)` does — the one question the adapter asks, so
    /// that "which set of buttons is up" is decided here rather than there.
    ///
    /// Over the bar it is exactly [`Self::button_at`]'s answer read as an
    /// action, so nothing can light up under the cursor and then do nothing
    /// when pressed. Over a click-started session it is cancel or confirm, and
    /// **only while it is recording**: a click that arrives after the handoff
    /// has begun finds no button, which is what makes a late cancel a no-op
    /// rather than a race.
    pub fn action_at(&self, x: f32, y: f32) -> Option<Action> {
        let mode = self.derive_mode();
        if mode.shows_check() {
            return check_slab_at(x, y).map(|i| CHECK_BUTTONS[i].action);
        }
        self.button_at(x, y).map(|i| BUTTONS[i].action)
    }

    /// Apply a session's report. `Finished` is stamped here, not by the session:
    /// the session ends the moment its outcome is known, and the flash outlives it.
    pub fn on_session(&mut self, activity: SessionActivity, now: Instant) -> Vec<Command> {
        self.activity = match activity {
            SessionActivity::None => Activity::None,
            SessionActivity::Recording { origin } => Activity::Recording { origin },
            SessionActivity::Processing { since } => Activity::Processing { since },
            SessionActivity::Finished { ok } => Activity::Done { ok, since: now },
        };
        self.settle()
    }

    /// Set the presence axis. Called with the one value the adapter composes
    /// from the residency toggle, the fullscreen watcher and the hover poll —
    /// at launch, on every config reload, and on every loop that moves one of
    /// the three.
    pub fn set_presence(&mut self, presence: Presence) -> Vec<Command> {
        self.presence = presence;
        self.settle()
    }

    /// Retire the terminal flash once its linger has elapsed. Emits the
    /// transition exactly once on the crossing, then nothing — the animation is
    /// otherwise self-driven in the adapter.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        if let Activity::Done { ok, since } = self.activity {
            if now.saturating_duration_since(since) >= linger(ok) {
                self.activity = Activity::None;
                return self.settle();
            }
        }
        Vec::new()
    }

    /// The pill mode for the current axes. Activity outranks presence: whenever
    /// a session is saying anything at all, presence has no say.
    fn derive_mode(&self) -> PillMode {
        match self.activity {
            Activity::Recording { origin } => PillMode::Recording { origin },
            Activity::Processing { since } => PillMode::Processing { since },
            Activity::Done { ok, since } => PillMode::Done { ok, since },
            Activity::None => match self.presence {
                Presence::Off | Presence::Suppressed => PillMode::Hidden,
                Presence::Resident { expanded: false } => PillMode::Idle,
                Presence::Resident { expanded: true } => PillMode::Expanded,
            },
        }
    }

    /// Reconcile the window against the axes and emit only what changed.
    fn settle(&mut self) -> Vec<Command> {
        let mode = self.derive_mode();
        // A window is worth holding while the pill is resident (even suppressed,
        // where it is only hidden) or while a session is using it.
        let want_window = self.presence != Presence::Off || self.activity != Activity::None;
        let want_shown = want_window && mode != PillMode::Hidden;

        let mut cmds = Vec::new();
        if want_window && !self.window {
            cmds.push(Command::Create);
            self.window = true;
            self.mode = None;
        }
        if self.window && self.mode != Some(mode) {
            cmds.push(Command::SetMode(mode));
            self.mode = Some(mode);
        }
        if want_shown != self.shown {
            cmds.push(if want_shown {
                Command::Show
            } else {
                Command::Hide
            });
            self.shown = want_shown;
        }
        if !want_window && self.window {
            cmds.push(Command::Destroy);
            self.window = false;
            self.mode = None;
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_millis(ms)
    }

    /// The last mode the core handed over while reaching `presence` × `activity`
    /// from scratch, read off the command lists — never off private state.
    /// `None` means it never handed over a mode at all.
    fn mode_for(presence: Presence, activity: SessionActivity) -> Option<PillMode> {
        let mut p = Pill::new();
        let mut cmds = p.set_presence(presence);
        cmds.extend(p.on_session(activity, t(0)));
        cmds.iter().rev().find_map(|c| match c {
            Command::SetMode(m) => Some(*m),
            _ => None,
        })
    }

    /// Every presence × activity pair, as one table. The rows where they
    /// disagree are the point: activity wins in all of them.
    #[test]
    fn mode_is_derived_from_presence_and_activity() {
        let rec = SessionActivity::Recording {
            origin: Origin::Hotkey,
        };
        let proc = SessionActivity::Processing { since: t(0) };
        let fin = SessionActivity::Finished { ok: true };
        let cases = [
            // Presence alone, with no session running. `Off` × `None` derives
            // `Hidden` like `Suppressed` does, but from a standing start there
            // is no window to hand it to, so nothing is emitted at all —
            // reaching that same cell *with* a window is asserted in
            // `a_session_that_never_shows_anything_leaves_no_window_behind`.
            (Presence::Off, SessionActivity::None, None),
            (
                Presence::Suppressed,
                SessionActivity::None,
                Some(PillMode::Hidden),
            ),
            (
                Presence::Resident { expanded: false },
                SessionActivity::None,
                Some(PillMode::Idle),
            ),
            (
                Presence::Resident { expanded: true },
                SessionActivity::None,
                Some(PillMode::Expanded),
            ),
            // Recording, from each presence.
            (
                Presence::Off,
                rec,
                Some(PillMode::Recording {
                    origin: Origin::Hotkey,
                }),
            ),
            (
                Presence::Suppressed,
                rec,
                Some(PillMode::Recording {
                    origin: Origin::Hotkey,
                }),
            ),
            (
                Presence::Resident { expanded: false },
                rec,
                Some(PillMode::Recording {
                    origin: Origin::Hotkey,
                }),
            ),
            (
                Presence::Resident { expanded: true },
                rec,
                Some(PillMode::Recording {
                    origin: Origin::Hotkey,
                }),
            ),
            // Processing, from each presence.
            (
                Presence::Off,
                proc,
                Some(PillMode::Processing { since: t(0) }),
            ),
            (
                Presence::Suppressed,
                proc,
                Some(PillMode::Processing { since: t(0) }),
            ),
            (
                Presence::Resident { expanded: false },
                proc,
                Some(PillMode::Processing { since: t(0) }),
            ),
            (
                Presence::Resident { expanded: true },
                proc,
                Some(PillMode::Processing { since: t(0) }),
            ),
            // The terminal flash, from each presence.
            (
                Presence::Off,
                fin,
                Some(PillMode::Done {
                    ok: true,
                    since: t(0),
                }),
            ),
            (
                Presence::Suppressed,
                fin,
                Some(PillMode::Done {
                    ok: true,
                    since: t(0),
                }),
            ),
            (
                Presence::Resident { expanded: false },
                fin,
                Some(PillMode::Done {
                    ok: true,
                    since: t(0),
                }),
            ),
            (
                Presence::Resident { expanded: true },
                fin,
                Some(PillMode::Done {
                    ok: true,
                    since: t(0),
                }),
            ),
        ];
        for (presence, activity, expected) in cases {
            assert_eq!(
                mode_for(presence, activity),
                expected,
                "presence {presence:?} x activity {activity:?}"
            );
        }
    }

    // Flat is a state — "stopped listening". A frozen height is an accident of
    // when the key happened to be released, so the handoff has to actually
    // reach zero, and reach it from a full-strength waveform.
    #[test]
    fn the_handoff_drains_the_waveform_to_flat() {
        assert_eq!(handoff_damping(Duration::ZERO), 1.0);
        assert_eq!(handoff_damping(HANDOFF), 0.0);
        assert_eq!(handoff_damping(HANDOFF * 3), 0.0);
    }

    // Linear, not eased: an ease-out lingers near full and then drops, which
    // reads as the bars being dragged down rather than losing energy.
    #[test]
    fn the_handoff_drains_at_a_steady_rate() {
        for step in 0..=4 {
            let t = step as f32 / 4.0;
            let d = handoff_damping(HANDOFF.mul_f32(t));
            assert!((d - (1.0 - t)).abs() < 1e-5, "at {t}: {d}");
        }
    }

    // The handoff is longer than the flash's own fade is quick, and long enough
    // at the ~30 Hz redraw to be an animation rather than a couple of frames.
    #[test]
    fn the_handoff_is_long_enough_to_read_as_motion() {
        assert!(HANDOFF >= Duration::from_millis(250));
        assert!(HANDOFF < SUCCESS_LINGER);
    }

    #[test]
    fn a_session_is_visible_even_when_a_fullscreen_app_suppresses_the_pill() {
        let mut p = Pill::new();
        // Resident, then a fullscreen app takes focus: hidden, window kept.
        assert_eq!(
            p.set_presence(Presence::Resident { expanded: false }),
            vec![
                Command::Create,
                Command::SetMode(PillMode::Idle),
                Command::Show
            ]
        );
        assert_eq!(
            p.set_presence(Presence::Suppressed),
            vec![Command::SetMode(PillMode::Hidden), Command::Hide]
        );
        // A chord press is an explicit request — suppression never wins over it.
        assert_eq!(
            p.on_session(
                SessionActivity::Recording {
                    origin: Origin::Hotkey
                },
                t(0)
            ),
            vec![
                Command::SetMode(PillMode::Recording {
                    origin: Origin::Hotkey
                }),
                Command::Show
            ]
        );
        // The whole session shows through: bars, the breathing border, and the
        // flash the user is actually waiting on.
        assert_eq!(
            p.on_session(SessionActivity::Processing { since: t(500) }, t(500)),
            vec![Command::SetMode(PillMode::Processing { since: t(500) })]
        );
        assert_eq!(
            p.on_session(SessionActivity::Finished { ok: true }, t(900)),
            vec![Command::SetMode(PillMode::Done {
                ok: true,
                since: t(900)
            })]
        );
        // And when the flash retires the pill goes back where suppression left
        // it — off the game, window kept, because the state after a session is
        // derived from presence rather than remembered.
        assert_eq!(
            p.tick(t(900) + SUCCESS_LINGER),
            vec![Command::SetMode(PillMode::Hidden), Command::Hide]
        );
    }

    #[test]
    fn hover_cannot_expand_a_recording_pill() {
        let mut p = Pill::new();
        p.set_presence(Presence::Resident { expanded: false });
        p.on_session(
            SessionActivity::Recording {
                origin: Origin::Hotkey,
            },
            t(0),
        );
        // The cursor drifts over the pill mid-capture: no mode change at all.
        assert_eq!(
            p.set_presence(Presence::Resident { expanded: true }),
            vec![]
        );
    }

    #[test]
    fn tick_past_linger_retires_the_flash_exactly_once() {
        let mut p = Pill::new();
        p.on_session(SessionActivity::Processing { since: t(0) }, t(0));
        assert_eq!(
            p.on_session(SessionActivity::Finished { ok: true }, t(2000)),
            vec![Command::SetMode(PillMode::Done {
                ok: true,
                since: t(2000)
            })]
        );
        // Before the linger elapses: nothing.
        let early = t(2000) + SUCCESS_LINGER - Duration::from_millis(1);
        assert!(p.tick(early).is_empty());
        // Once past it, the pill goes — presence is Off, so it has nowhere to
        // return to...
        let late = t(2000) + SUCCESS_LINGER;
        assert_eq!(
            p.tick(late),
            vec![
                Command::SetMode(PillMode::Hidden),
                Command::Hide,
                Command::Destroy
            ]
        );
        // ...and never again.
        assert!(p.tick(late + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn a_failed_flash_holds_for_the_longer_linger() {
        let mut p = Pill::new();
        p.on_session(SessionActivity::Finished { ok: false }, t(0));
        // A success would already be gone by here.
        assert!(p.tick(t(0) + SUCCESS_LINGER).is_empty());
        assert_eq!(p.tick(t(0) + ERROR_LINGER).len(), 3);
    }

    #[test]
    fn the_state_after_a_session_is_derived_from_presence_not_remembered() {
        let mut p = Pill::new();
        // The session starts with the pill off...
        p.on_session(
            SessionActivity::Recording {
                origin: Origin::Hotkey,
            },
            t(0),
        );
        // ...and residency is switched on while it runs. Activity outranks
        // presence, so nothing changes on screen yet.
        assert_eq!(
            p.set_presence(Presence::Resident { expanded: false }),
            vec![]
        );
        p.on_session(SessionActivity::Finished { ok: true }, t(1000));
        // The flash retires to the *current* presence, not to where it started.
        assert_eq!(
            p.tick(t(1000) + SUCCESS_LINGER),
            vec![Command::SetMode(PillMode::Idle)]
        );
    }

    #[test]
    fn residency_switched_off_mid_flash_takes_the_window_with_it() {
        let mut p = Pill::new();
        p.set_presence(Presence::Resident { expanded: false });
        p.on_session(SessionActivity::Finished { ok: true }, t(0));
        // Toggled off during the flash: the flash still owns the pill.
        assert_eq!(p.set_presence(Presence::Off), vec![]);
        // And when it retires there is nothing left to return to.
        assert_eq!(
            p.tick(t(0) + SUCCESS_LINGER),
            vec![
                Command::SetMode(PillMode::Hidden),
                Command::Hide,
                Command::Destroy
            ]
        );
    }

    #[test]
    fn a_resident_window_is_created_once_and_survives_a_whole_session() {
        let mut p = Pill::new();
        let mut cmds = p.set_presence(Presence::Resident { expanded: false });
        cmds.extend(p.on_session(
            SessionActivity::Recording {
                origin: Origin::Hotkey,
            },
            t(0),
        ));
        cmds.extend(p.on_session(SessionActivity::Processing { since: t(500) }, t(500)));
        cmds.extend(p.on_session(SessionActivity::Finished { ok: true }, t(900)));
        cmds.extend(p.tick(t(900) + SUCCESS_LINGER));
        assert_eq!(
            cmds.iter().filter(|c| **c == Command::Create).count(),
            1,
            "{cmds:?}"
        );
        assert!(!cmds.contains(&Command::Destroy), "{cmds:?}");
    }

    #[test]
    fn a_session_that_never_shows_anything_leaves_no_window_behind() {
        let mut p = Pill::new();
        // Too short a capture, or nothing usable said: straight back to None.
        assert!(p.on_session(SessionActivity::None, t(0)).is_empty());
        assert!(p
            .on_session(
                SessionActivity::Recording {
                    origin: Origin::Hotkey
                },
                t(0)
            )
            .contains(&Command::Create));
        assert_eq!(
            p.on_session(SessionActivity::None, t(100)),
            vec![
                Command::SetMode(PillMode::Hidden),
                Command::Hide,
                Command::Destroy
            ]
        );
    }

    /// Residency turned on: the nub arrives, once, and stays. This is the whole
    /// of what the toggle does from a standing start.
    #[test]
    fn switching_residency_on_puts_the_nub_up_and_leaves_it_there() {
        let mut p = Pill::new();
        assert_eq!(
            p.set_presence(Presence::Resident { expanded: false }),
            vec![
                Command::Create,
                Command::SetMode(PillMode::Idle),
                Command::Show
            ]
        );
        // Idempotent: a config reload that didn't change residency is not a
        // reason to touch the window.
        assert_eq!(
            p.set_presence(Presence::Resident { expanded: false }),
            vec![]
        );
        // And nothing retires a nub — `tick` is the flash's clock alone.
        assert!(p.tick(t(60_000)).is_empty());
    }

    /// The idle → hidden transition: residency turned off with nothing running.
    /// The pill goes, and the window goes with it, because there is nothing
    /// left for it to do.
    #[test]
    fn switching_residency_off_while_idle_takes_the_pill_and_its_window() {
        let mut p = Pill::new();
        p.set_presence(Presence::Resident { expanded: false });
        assert_eq!(
            p.set_presence(Presence::Off),
            vec![
                Command::SetMode(PillMode::Hidden),
                Command::Hide,
                Command::Destroy
            ]
        );
        assert_eq!(p.set_presence(Presence::Off), vec![]);
    }

    /// Toggled OFF mid-session: the pill is not snatched away. It rides out
    /// Recording and Processing, holds the flash, and only then goes.
    #[test]
    fn residency_switched_off_mid_session_rides_the_session_out() {
        let mut p = Pill::new();
        p.set_presence(Presence::Resident { expanded: false });
        p.on_session(
            SessionActivity::Recording {
                origin: Origin::Hotkey,
            },
            t(0),
        );
        // The user saves settings with residency off while still speaking.
        assert_eq!(p.set_presence(Presence::Off), vec![]);
        // Recording and Processing are unaffected — activity outranks presence.
        assert_eq!(
            p.on_session(SessionActivity::Processing { since: t(500) }, t(500)),
            vec![Command::SetMode(PillMode::Processing { since: t(500) })]
        );
        assert_eq!(
            p.on_session(SessionActivity::Finished { ok: true }, t(800)),
            vec![Command::SetMode(PillMode::Done {
                ok: true,
                since: t(800)
            })]
        );
        // The flash holds its full linger...
        assert!(p
            .tick(t(800) + SUCCESS_LINGER - Duration::from_millis(1))
            .is_empty());
        // ...and only when it retires does the pill leave.
        assert_eq!(
            p.tick(t(800) + SUCCESS_LINGER),
            vec![
                Command::SetMode(PillMode::Hidden),
                Command::Hide,
                Command::Destroy
            ]
        );
    }

    /// Toggled ON mid-session: nothing appears mid-dictation, and the flash
    /// retires to the nub rather than to nothing. The other direction of the
    /// same rule — the return state is derived, never remembered.
    #[test]
    fn residency_switched_on_mid_session_shows_the_nub_once_the_flash_retires() {
        let mut p = Pill::new();
        p.on_session(
            SessionActivity::Recording {
                origin: Origin::Hotkey,
            },
            t(0),
        );
        // Turned on while recording: nothing on screen changes.
        assert_eq!(
            p.set_presence(Presence::Resident { expanded: false }),
            vec![]
        );
        p.on_session(SessionActivity::Processing { since: t(500) }, t(500));
        p.on_session(SessionActivity::Finished { ok: false }, t(900));
        // The longer linger, since this one failed.
        assert!(p.tick(t(900) + SUCCESS_LINGER).is_empty());
        // The flash resolves to the nub, and the window is kept — no Hide, no
        // Destroy, because the pill has somewhere to be.
        assert_eq!(
            p.tick(t(900) + ERROR_LINGER),
            vec![Command::SetMode(PillMode::Idle)]
        );
    }

    /// A session started with residency off, toggled on and off again before it
    /// ends, resolves to whatever presence says at the moment the flash retires
    /// — not to any state it passed through on the way.
    #[test]
    fn only_the_presence_at_retirement_decides_where_the_flash_goes() {
        let mut p = Pill::new();
        p.on_session(SessionActivity::Finished { ok: true }, t(0));
        p.set_presence(Presence::Resident { expanded: false });
        p.set_presence(Presence::Off);
        p.set_presence(Presence::Resident { expanded: false });
        assert_eq!(
            p.tick(t(0) + SUCCESS_LINGER),
            vec![Command::SetMode(PillMode::Idle)]
        );
    }

    /// The one place the odd count is stated. Symmetric growth puts the middle
    /// button under the cursor that opened the bar — which needs a middle.
    #[test]
    fn the_button_count_stays_odd() {
        assert_eq!(BUTTONS.len() % 2, 1, "the bar needs a centre button");
        assert_eq!(BUTTONS[BUTTONS.len() / 2].action, Action::Dictate);
    }

    /// Every number about the expanded bar, derived from the list rather than
    /// written down twice.
    #[test]
    fn the_bar_derives_the_settled_islands_from_its_button_list() {
        assert_eq!(bar_width(), 118.0);
        let islands: Vec<(f32, f32)> = (0..BUTTONS.len())
            .map(|i| {
                let c = island_centre(i);
                (c - BUTTONS[i].w / 2.0, c + BUTTONS[i].w / 2.0)
            })
            .collect();
        assert_eq!(islands, vec![(-59.0, -27.0), (-24.0, 24.0), (27.0, 59.0)]);
        // A circle and a stadium out of one expression, both fully rounded.
        let heights: Vec<f32> = BUTTONS.iter().map(|b| b.height()).collect();
        assert_eq!(heights, vec![32.0, 32.0, 32.0]);
        assert_eq!(BUTTONS[1].radius(), 16.0);
        // The glyph box is not the button: same box in a 32 and a 48 island.
        assert_eq!(GLYPH_BOX, 22.0);
    }

    /// Slabs, not circles: each spans its island plus half the desktop either
    /// side, and the ends stop at the island.
    #[test]
    fn the_slabs_cover_the_bar_end_to_end() {
        assert_eq!(slab(0), (-59.0, -25.5));
        assert_eq!(slab(1), (-25.5, 25.5));
        assert_eq!(slab(2), (25.5, 59.0));
        // No overlaps and no seams: every point between the ends belongs to
        // exactly one button.
        for i in 1..BUTTONS.len() {
            assert_eq!(slab(i - 1).1, slab(i).0);
        }
    }

    /// A pill with buttons up, and something to copy.
    fn expanded() -> Pill {
        let mut p = Pill::new();
        p.set_has_history(true);
        p.set_presence(Presence::Resident { expanded: true });
        p
    }

    /// The gaps are not dead zones. Sliding from Dictate to Settings crosses
    /// 3px of bare desktop, and the hover must not flicker off in it.
    #[test]
    fn a_click_in_the_gap_lands_on_the_adjacent_button() {
        let p = expanded();
        // Just left of the seam is still Dictate; just right of it is Settings.
        assert_eq!(p.action_at(25.0, 0.0), Some(Action::Dictate));
        assert_eq!(p.action_at(26.0, 0.0), Some(Action::Settings));
        assert_eq!(p.action_at(-26.0, 0.0), Some(Action::Copy));
        assert_eq!(p.action_at(-25.0, 0.0), Some(Action::Dictate));
        // And the islands themselves, at their centres.
        assert_eq!(p.action_at(-43.0, 0.0), Some(Action::Copy));
        assert_eq!(p.action_at(0.0, 0.0), Some(Action::Dictate));
        assert_eq!(p.action_at(43.0, 0.0), Some(Action::Settings));
    }

    /// Past the last island is end padding: it holds the pill expanded with
    /// nothing lit.
    #[test]
    fn a_click_in_the_end_padding_hits_nothing() {
        let p = expanded();
        for x in [-62.0, -60.0, -59.5, 59.0, 60.0, 62.0] {
            assert_eq!(p.button_at(x, 0.0), None, "at {x}");
            assert_eq!(p.action_at(x, 0.0), None, "at {x}");
        }
    }

    /// A slab is full bar height and no more. Above the bar is the label's
    /// band, which is part of the window and part of no button — the cursor is
    /// over the pill up there without being over anything to press.
    #[test]
    fn the_label_band_above_the_bar_hits_nothing() {
        let p = expanded();
        // Inside the bar, top to bottom, over each island.
        for x in [-43.0, 0.0, 43.0] {
            for y in [-15.9, 0.0, 15.9] {
                assert!(p.button_at(x, y).is_some(), "at ({x}, {y})");
            }
        }
        // And just outside it, where the label lives.
        for y in [-16.1, -24.0, -40.0, 16.1] {
            assert_eq!(p.button_at(0.0, y), None, "at y {y}");
            assert_eq!(p.action_at(0.0, y), None, "at y {y}");
        }
    }

    /// With nothing recorded, Copy is disabled rather than silently no-op'ing —
    /// and its slab is inert, so it does not light up either. Its neighbours
    /// are unaffected.
    #[test]
    fn copy_is_inert_with_an_empty_history() {
        let mut p = Pill::new();
        p.set_presence(Presence::Resident { expanded: true });
        assert!(!p.enabled(0));
        assert_eq!(p.button_at(-43.0, 0.0), None);
        assert_eq!(p.action_at(-43.0, 0.0), None);
        assert_eq!(p.action_at(0.0, 0.0), Some(Action::Dictate));
        // The first transcript of the session brings it to life.
        p.set_has_history(true);
        assert_eq!(p.action_at(-43.0, 0.0), Some(Action::Copy));
    }

    /// Hover cannot expand a recording pill — and with no bar on screen there
    /// is nothing to hit either, whatever the cursor is doing over it.
    #[test]
    fn the_buttons_are_inert_while_a_session_runs() {
        let mut p = expanded();
        assert!(p.showing_bar());
        p.on_session(
            SessionActivity::Recording {
                origin: Origin::Hotkey,
            },
            t(0),
        );
        assert!(!p.showing_bar());
        for x in [-43.0, 0.0, 43.0] {
            assert_eq!(p.button_at(x, 0.0), None, "at {x}");
            assert_eq!(p.action_at(x, 0.0), None, "at {x}");
        }
        // And they come back when the flash retires to the hovered nub.
        p.on_session(SessionActivity::Finished { ok: true }, t(100));
        p.tick(t(100) + SUCCESS_LINGER);
        assert_eq!(p.action_at(0.0, 0.0), Some(Action::Dictate));
    }

    #[test]
    fn a_click_started_session_reaches_the_adapter_as_one() {
        let mut p = Pill::new();
        assert!(p
            .on_session(
                SessionActivity::Recording {
                    origin: Origin::Click
                },
                t(0)
            )
            .contains(&Command::SetMode(PillMode::Recording {
                origin: Origin::Click
            })));
    }

    /// A pill recording something the user started with the mouse.
    fn click_recording() -> Pill {
        let mut p = expanded();
        p.on_session(
            SessionActivity::Recording {
                origin: Origin::Click,
            },
            t(0),
        );
        p
    }

    /// The layout, as the ticket writes it: `7 + 20 + 12 + 34 + 12 + 20 + 7`.
    /// Stated as that sum rather than as the numbers it produces, because the
    /// sum is the thing that has to keep adding up.
    #[test]
    fn the_click_started_pill_adds_up_to_its_body() {
        // The waveform gets whatever the two buttons have not claimed.
        let row = CLICK_W - 2.0 * CHECK_CLAIM;
        assert_eq!(row, 34.0);
        assert_eq!(
            CHECK_PAD + CHECK_BUTTON + CHECK_GAP + row + CHECK_GAP + CHECK_BUTTON + CHECK_PAD,
            CLICK_W
        );
        assert_eq!((CLICK_W, CLICK_H), (112.0, 32.0));
        // Circles, and smaller than any button the hover bar carries.
        for b in &CHECK_BUTTONS {
            assert_eq!((b.w, b.height(), b.radius()), (20.0, 20.0, 10.0));
            assert!(BUTTONS.iter().all(|bar| bar.w > b.w), "{:?}", b.action);
        }
        assert_eq!(
            (check_centre(CLICK_W, 0), check_centre(CLICK_W, 1)),
            (-39.0, 39.0)
        );
        // The claim each button makes is the pad, the button and the gap — the
        // one number the renderer places both the discs and the row from.
        assert_eq!(CHECK_CLAIM, 39.0);
    }

    /// Origin decides presentation: the same session, started the other way,
    /// shows the two buttons and is a mouse target.
    #[test]
    fn origin_decides_whether_a_recording_pill_is_clickable() {
        let hotkey = PillMode::Recording {
            origin: Origin::Hotkey,
        };
        let click = PillMode::Recording {
            origin: Origin::Click,
        };
        // A hotkey session is bare: no buttons at all, so `WS_EX_TRANSPARENT`
        // stays on and clicks pass through to the app being dictated into.
        assert!(!hotkey.shows_buttons());
        assert!(!hotkey.shows_bar() && !hotkey.shows_check());
        // A click-started one shows its check — and is still not the bar, which
        // is what keeps hover, the label and the slabs off it.
        assert!(click.shows_buttons());
        assert!(click.shows_check() && !click.shows_bar());
        // And the bar is the bar: clickable, but never a check.
        assert!(PillMode::Expanded.shows_bar() && !PillMode::Expanded.shows_check());
    }

    /// Cancel and confirm, at their centres and across the gap that separates
    /// them from the waveform — which belongs to the button, as the bar's gaps
    /// do. The pad past each button is inert.
    #[test]
    fn the_check_is_hit_across_its_button_and_its_inner_gap() {
        let p = click_recording();
        assert_eq!(p.action_at(-39.0, 0.0), Some(Action::Cancel));
        assert_eq!(p.action_at(39.0, 0.0), Some(Action::Confirm));
        // Inward, to the middle of the gap.
        assert_eq!(p.action_at(-24.0, 0.0), Some(Action::Cancel));
        assert_eq!(p.action_at(24.0, 0.0), Some(Action::Confirm));
        // The waveform between them is not a button.
        for x in [-22.0, 0.0, 22.0] {
            assert_eq!(p.action_at(x, 0.0), None, "at {x}");
        }
        // Nor is the pad outside them, nor the label's band above.
        for (x, y) in [(-52.0, 0.0), (52.0, 0.0), (-39.0, -20.0), (39.0, 24.0)] {
            assert_eq!(p.action_at(x, y), None, "at ({x}, {y})");
        }
    }

    /// The bar's own machinery must not follow the pill into a session: hover
    /// and the label are indexed by the bar's list, and a click-started pill is
    /// not the bar however clickable it is.
    #[test]
    fn a_click_started_session_lights_no_bar_button() {
        let p = click_recording();
        assert!(p.action_at(39.0, 0.0).is_some(), "it is a mouse target");
        for x in [-43.0, -39.0, 0.0, 39.0, 43.0] {
            assert_eq!(p.button_at(x, 0.0), None, "at {x}");
        }
    }

    /// Cancel is Recording-only. A click that arrives after the handoff has
    /// begun — the button was under the cursor a frame ago — finds nothing,
    /// rather than discarding a capture that is already at the worker.
    #[test]
    fn the_check_does_not_survive_the_handoff() {
        let mut p = click_recording();
        assert_eq!(p.action_at(-39.0, 0.0), Some(Action::Cancel));
        p.on_session(SessionActivity::Processing { since: t(100) }, t(100));
        assert!(
            p.action_at(39.0, 0.0).is_none(),
            "Processing is not a mouse target"
        );
        for x in [-39.0, 0.0, 39.0] {
            assert_eq!(p.action_at(x, 0.0), None, "at {x}");
        }
        // Nor does the flash that follows it.
        p.on_session(SessionActivity::Finished { ok: true }, t(200));
        assert_eq!(p.action_at(39.0, 0.0), None);
    }

    /// And once it is over, the bar the session came out of is back — with its
    /// own buttons, at their own places.
    #[test]
    fn a_cancelled_session_hands_the_bar_back() {
        let mut p = click_recording();
        // Cancel reports no session at all: no flash to sit through.
        p.on_session(SessionActivity::None, t(500));
        assert_eq!(p.action_at(0.0, 0.0), Some(Action::Dictate));
        assert_eq!(p.action_at(-39.0, 0.0), Some(Action::Copy));
    }
}
