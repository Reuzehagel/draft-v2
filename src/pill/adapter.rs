//! The pill adapter: performs the Pill core's commands against the window.
//!
//! The adapter holds no lifecycle rules — those are the core's. What it owns
//! is everything *per frame*: the motion, the bars, the hover and the label,
//! and the three rules that have bitten before and are tested here:
//!
//! - **`wants_frame`**: a settled pill asks for no frames.
//! - **Click-through**: `WS_EX_TRANSPARENT` flips with the mode, off exactly
//!   while the pill shows something to press.
//! - **Teardown supersede**: a deferred `Hide`/`Destroy` is dropped by a later
//!   `Create` or `Show`.
//!
//! The window sits behind [`PillPort`] so those rules can be tested without
//! one; [`PillWindow`] is the only implementation that reaches the screen.

use std::time::Instant;

use anyhow::Result;
use draft::audio;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use crate::pill;
use crate::pill::core::{BodyStyle, PillMode};
use crate::pill::geom::{Geom, Hover, Motion, Slots};
use crate::pill::hook::HookEvent;
use crate::pill::label::{Fade, Label};
use crate::pill::ladder;
use crate::pill::monitor::{HomeMonitor, Rect};
use crate::pill::window::PillWindow;

/// How far outside the nub a cursor counts as having reached it, in logical
/// pixels. The nub is 36x10 and deliberately small; asking the cursor to land
/// on it exactly would make the bar hard to open, and asking only that the
/// cursor be on the *window* would open it from across the envelope.
const HOVER_REACH: f32 = 8.0;

/// What the adapter needs of a pill window — its port, so as not to be one
/// more "surface" beside the layered one. The seam exists for the tests:
/// [`PillWindow`] is the one implementation that reaches the screen, and each
/// method is its namesake there.
pub trait PillPort {
    fn set_home(&mut self, home: HomeMonitor) -> Result<()>;
    fn show(&self);
    fn hide(&self);
    fn render(
        &mut self,
        geom: &Geom,
        bar_heights: &[f32],
        slots: &Slots,
        label: &Fade,
        style: BodyStyle,
    ) -> Result<()>;
    fn set_click_through(&mut self, on: bool);
    fn rect(&self) -> Rect;
    fn scale(&self) -> f32;
    fn has_frame(&self) -> bool;
    fn repush(&mut self) -> Result<()>;
}

impl PillPort for PillWindow {
    fn set_home(&mut self, home: HomeMonitor) -> Result<()> {
        PillWindow::set_home(self, home)
    }
    fn show(&self) {
        PillWindow::show(self)
    }
    fn hide(&self) {
        PillWindow::hide(self)
    }
    fn render(
        &mut self,
        geom: &Geom,
        bar_heights: &[f32],
        slots: &Slots,
        label: &Fade,
        style: BodyStyle,
    ) -> Result<()> {
        PillWindow::render(self, geom, bar_heights, slots, label, style)
    }
    fn set_click_through(&mut self, on: bool) {
        PillWindow::set_click_through(self, on)
    }
    fn rect(&self) -> Rect {
        PillWindow::rect(self)
    }
    fn scale(&self) -> f32 {
        PillWindow::scale(self)
    }
    fn has_frame(&self) -> bool {
        PillWindow::has_frame(self)
    }
    fn repush(&mut self) -> Result<()> {
        PillWindow::repush(self)
    }
}

/// The pill window and its animation, driven by the [`PillMode`] the Pill core
/// derives. The core decides *what* mode, *when* to transition, and whether a
/// window exists at all; the adapter derives every frame's geometry from the
/// motion model, and its bars from the ring buffer. It holds no lifecycle rules.
///
/// The whole of its animation state is one [`Motion`] — where the pill was,
/// where it is going, and when it set off. There is no per-mode animation code
/// left here: a mode change starts a tween, and every frame is `motion.at(now)`.
pub struct PillAdapter<W: PillPort = PillWindow> {
    window: Option<W>,
    /// Handed to each window it creates, so the wndproc hook can post to the
    /// app loop.
    hook_tx: crossbeam_channel::Sender<HookEvent>,
    /// Where the pill lives. Held even with no window, so the next `Create`
    /// lands on the right monitor without having to re-derive first. `None`
    /// only before the first derivation — and on a machine with no monitors at
    /// all, where there is nowhere to put a window anyway.
    home: Option<HomeMonitor>,
    bands: audio::level::BandMeter,
    /// The current logical mode; `None` when there is no window.
    mode: Option<PillMode>,
    /// The transition in flight — or a settled Geom, once it has finished.
    motion: Motion,
    /// A frame is owed that the animation state alone would not ask for: the
    /// one that lands a finished transition. Cleared by [`Self::redraw`].
    ///
    /// This is what makes an idle nub cost nothing: with no motion running and
    /// no frame owed, the adapter asks for none at all and the system keeps the
    /// layered surface alive by itself.
    frame_owed: bool,
    /// A `Hide` or `Destroy` the core has issued that the pill is still
    /// animating its way to. Both arrive in the same command list as the mode
    /// change that concealing *is*, so performing them on arrival would cut
    /// that conceal off at its first frame.
    ///
    /// This defers *when* a teardown happens; it never decides *whether* one
    /// does. See [`Self::supersede_teardown`] for the one case where a deferred
    /// teardown is dropped — which is also the core's call, not the adapter's.
    pending: Option<Teardown>,
    /// When the handoff started, i.e. when the mode last became `Processing`.
    ///
    /// Kept on the adapter rather than read off the mode because the handoff
    /// can outlive `Processing`: a worker that resolves inside 320 ms puts the
    /// pill in `Done` mid-fall, and `Done`'s own `since` is the flash's clock,
    /// not the handoff's. Without this the bars would snap flat on that frame —
    /// exactly the seam the handoff exists to remove.
    handoff_since: Option<Instant>,
    /// Read-only clone of the active capture's ring buffer, for live bars.
    ring: Option<audio::ring::Buffer>,
    /// Which button the cursor is on, and the fade between it and the last.
    /// Deliberately outside the [`Motion`]: per-button hover is per-button
    /// state that one `Geom` cannot carry (#29).
    hover: Hover,
    /// Whether each button is live, as the Pill core last derived it. Cached
    /// here because it is a per-frame drawing input and the core is not asked
    /// per frame; `App::refresh_status` is what keeps the two in step.
    enabled: [bool; pill::core::BUTTON_COUNT],
    /// What the label is saying, and the crossfade between it and the last
    /// thing it said. A third piece of per-frame state beside the [`Motion`]
    /// and the [`Hover`], for the same reason as the second: it is a surface
    /// with its own clock, and a flash outlives the hover under it (#46).
    label: Label,
    /// The cursor's last known offset from the pill's centre, in logical
    /// pixels. `MouseInput` carries no position, so this is what a click is
    /// tested against.
    cursor: Option<(f32, f32)>,
    /// Which body the expanded pill wears. A drawing input, like `enabled`:
    /// the Geom cannot carry a style, and a frame mid-morph belongs to no mode
    /// to read one off. Fed from config beside the core's own copy.
    body_style: BodyStyle,
}

/// What to do with the window once the motion taking it off screen has run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Teardown {
    /// Off screen, window kept — the resident case, where the next reveal must
    /// not have to rebuild a layered window.
    Hide,
    /// Off screen and gone. Only reached when the pill has no reason to exist
    /// at all: residency off, and no session running.
    Destroy,
}

impl<W: PillPort> PillAdapter<W> {
    pub fn new(hook_tx: crossbeam_channel::Sender<HookEvent>) -> Self {
        Self {
            window: None,
            hook_tx,
            home: None,
            bands: audio::level::BandMeter::new(pill::BAR_COUNT),
            mode: None,
            motion: Motion::settled(PillMode::Hidden, Instant::now()),
            frame_owed: false,
            pending: None,
            handoff_since: None,
            ring: None,
            hover: Hover::new(Instant::now()),
            enabled: [true; pill::core::BUTTON_COUNT],
            label: Label::new(Instant::now()),
            cursor: None,
            body_style: BodyStyle::default(),
        }
    }

    /// Adopt the configured body style. Owes a frame: the bar on screen right
    /// now is the wrong shape from here on.
    pub fn set_body_style(&mut self, style: BodyStyle) {
        let changed = self.body_style != style;
        self.body_style = style;
        self.owe(changed);
    }

    /// Note that something the renderer reads has moved, so the loop draws
    /// once more.
    ///
    /// Every mutator below reports whether it changed anything, and every one
    /// of them means the same thing by it — a frame is owed. One place to say
    /// so, rather than the same `if changed` at each.
    fn owe(&mut self, changed: bool) {
        self.frame_owed |= changed;
    }

    /// Adopt the core's view of which buttons are live.
    pub fn set_enabled(&mut self, enabled: [bool; pill::core::BUTTON_COUNT]) {
        let changed = self.enabled != enabled;
        self.enabled = enabled;
        self.owe(changed);
    }

    /// Where the cursor is, as a logical offset from the pill's centre, and
    /// which button that lights and names. A cursor wandering inside one slab
    /// moves nothing, so it costs no frame.
    pub fn set_cursor(&mut self, offset: Option<(f32, f32)>, hovered: Option<usize>, now: Instant) {
        self.cursor = offset;
        // Both, always: they are two surfaces answering the same move, and
        // short-circuiting would leave the label naming the button the
        // indicator has just left.
        let lit = self.hover.set(hovered, now);
        let named = self.label.set_hover(hovered, now);
        self.owe(lit || named);
    }

    /// Put the hover indicator and the label out, leaving the cursor's own
    /// position alone. What the bar collapsing means — for a click-started
    /// session it collapses into a pill that is still a mouse target, so the
    /// position a click is tested against has to survive it.
    fn clear_hover(&mut self, now: Instant) {
        let lit = self.hover.set(None, now);
        let named = self.label.set_hover(None, now);
        self.owe(lit || named);
    }

    /// Say something over the pill for a beat — a landed copy, which the pill
    /// otherwise has no way to distinguish from a click that did nothing.
    pub fn flash_label(&mut self, text: &'static str, now: Instant) {
        let flashed = self.label.flash(text, now);
        self.owe(flashed);
    }

    /// Retire an expired flash. The one thing about the pill that moves with no
    /// event behind it, so the app loop calls it every pass.
    pub fn tick_label(&mut self, now: Instant) {
        let retired = self.label.tick(now);
        self.owe(retired);
    }

    /// The cursor's last known offset from the pill's centre — what a click is
    /// tested against.
    pub fn cursor(&self) -> Option<(f32, f32)> {
        self.cursor
    }

    /// Where the pill is, in physical virtual-screen pixels — `None` with no
    /// window.
    pub fn rect(&self) -> Option<Rect> {
        self.window.as_ref().map(|pw| pw.rect())
    }

    /// Whether the cursor is inside the region that keeps the pill expanded.
    ///
    /// A direct comparison of physical virtual-screen pixels: `GetCursorPos`
    /// and the window's placement are both in that space, and scaling either by
    /// [`PillWindow::scale`] would put the test in a space neither of them is
    /// in.
    ///
    /// **The region is not the same coming and going.** Opening asks the cursor
    /// to be near the *nub* — the window is the envelope, and a 36x10 nub that
    /// sprang open from 60px away would be a pill that expands at anything
    /// passing along the bottom of the screen. Staying open asks that it be on
    /// the *bar*, which is where the buttons are. The overlap between the two
    /// is the hysteresis: nothing can sit on a boundary and flicker.
    ///
    /// Neither region is the window rect, and since #46 that matters: the
    /// envelope grew to 260x80 to hold the label, so "on the window" would now
    /// hold the bar open from 70px to either side of it and from the label's
    /// band, which is not a button and does not keep one open.
    ///
    /// What this does not decide is clicks: a layered window hit-tests on
    /// per-pixel alpha, so the corners stay click-through however it answers.
    pub fn cursor_over(&self, cursor: (i32, i32), expanded: bool) -> bool {
        let Some(r) = self.rect() else {
            return false;
        };
        let reach = if expanded {
            self.reach(r, self.body_style.bar_width(), pill::geom::BAR_H)
        } else {
            self.reach(r, pill::geom::NUB_W, pill::geom::NUB_H)
        };
        reach.contains(cursor)
    }

    /// Whether the cursor is close enough that the pill should be watching for
    /// it properly — the proximity ladder's middle rung.
    ///
    /// Deliberately the *window's* rect grown by a wide band, rather than the
    /// nub's reach grown by one: this is not a hover test and nothing happens
    /// at its boundary. It only decides whether the next look is 50 ms away or
    /// 250 ms away, so it wants to be cheap and generous, and it costs nothing
    /// to be wrong about by a hundred pixels.
    pub fn cursor_near(&self, cursor: (i32, i32)) -> bool {
        let Some(r) = self.rect() else {
            return false;
        };
        let scale = self.window.as_ref().map_or(1.0, |pw| pw.scale());
        r.grown((ladder::NEAR_BAND * scale).round() as i32)
            .contains(cursor)
    }

    /// A `w` x `h` logical shape centred on the pill, grown by [`HOVER_REACH`]
    /// on every side. In physical pixels, off the window's own rect, so it
    /// lands on the shape at any DPI.
    ///
    /// Centred on the *pill's band* rather than on the window: the pill sits at
    /// the bottom of the surface, so the window's middle is up in the label's
    /// band where nothing is drawn.
    fn reach(&self, r: Rect, w: f32, h: f32) -> Rect {
        let scale = self.window.as_ref().map_or(1.0, |pw| pw.scale());
        let half_w = ((w / 2.0 + HOVER_REACH) * scale).round() as i32;
        let half_h = ((h / 2.0 + HOVER_REACH) * scale).round() as i32;
        let cx = (r.left + r.right) / 2;
        let cy = r.top + pill::geom::pill_centre_y(r.height() as f32, scale).round() as i32;
        Rect {
            left: cx - half_w,
            top: cy - half_h,
            right: cx + half_w,
            bottom: cy + half_h,
        }
    }

    /// A `CursorMoved` position — physical pixels from the window's top-left —
    /// as the logical offset from the pill's centre the button slabs are stated
    /// in. This is the *one* place the scale divides: the slabs are logical,
    /// every pixel Windows reports is not.
    ///
    /// Both axes, since #46: the surface is taller than the bar, and a cursor
    /// in the label's band is over the window without being over a button.
    pub fn offset_in_window(&self, physical: (f32, f32)) -> Option<(f32, f32)> {
        let (r, pw) = (self.rect()?, self.window.as_ref()?);
        let scale = pw.scale();
        let cy = pill::geom::pill_centre_y(r.height() as f32, scale);
        Some((
            (physical.0 - r.width() as f32 / 2.0) / scale,
            (physical.1 - cy) / scale,
        ))
    }

    pub fn set_ring(&mut self, ring: audio::ring::Buffer) {
        self.ring = Some(ring);
    }

    /// Adopt a home monitor the core just derived, moving the window there if
    /// there is one. With no window this only records it — the next `Create`
    /// reads it.
    pub fn set_home(&mut self, home: HomeMonitor) {
        self.home = Some(home);
        if let Some(pw) = self.window.as_mut() {
            if let Err(e) = pw.set_home(home) {
                tracing::error!(error = %e, "could not move the pill to its home monitor");
            }
        }
    }

    pub fn has_window(&self) -> bool {
        self.window.is_some()
    }

    /// Whether the pill is on screen — something a cursor could arrive over.
    ///
    /// **Not `window.is_some()`.** Residency makes the window permanent, so a
    /// window exists through every fullscreen suppression, every lock, and the
    /// whole of a session-less life; a ladder reading that would pin the loop
    /// to a hover poll forever for a pill that is not there (#49). What decides
    /// it is the mode: `Hidden` means off screen, and `None` means no window at
    /// all.
    ///
    /// A pill still *animating* its way off screen answers false, and the
    /// ladder is written so that costs it nothing — animation outranks
    /// reachability, so the conceal keeps its frames.
    pub fn is_active(&self) -> bool {
        self.window.is_some() && !matches!(self.mode, None | Some(PillMode::Hidden))
    }

    /// Whether the pill is doing nothing — the only stretch in which it may
    /// move. Latched at session start and while expanded, so it cannot skate to
    /// another monitor mid-sentence, nor slide out from under the hand about to
    /// click it.
    ///
    /// A transition still running counts as busy even when the mode it is
    /// heading for is `Idle`. The mode flips at the *start* of the morph, and
    /// the move is a hard cut — one without the other would tear the conceal or
    /// the reveal in half.
    pub fn is_idle(&self, now: Instant) -> bool {
        matches!(
            self.mode,
            None | Some(PillMode::Hidden) | Some(PillMode::Idle)
        ) && !self.motion.is_running(now)
    }

    /// Whether the pill has a frame to draw right now. False for a settled nub,
    /// which is the point: residency costs one `UpdateLayeredWindow` and then
    /// nothing until something happens.
    pub fn wants_frame(&self, now: Instant) -> bool {
        if self.window.is_none() {
            return false;
        }
        self.frame_owed
            || self.motion.is_running(now)
            || self.hover.is_running(now)
            || self.label.is_running(now)
            || self.mode_self_animates()
    }

    /// The modes that produce new pixels without a transition running: live
    /// bars, the working breath, and the tail of a handoff still falling.
    fn mode_self_animates(&self) -> bool {
        match self.mode {
            Some(PillMode::Recording { .. }) | Some(PillMode::Processing { .. }) => true,
            // The flash itself is a still image. The only thing moving under it
            // is a handoff that outlived Processing.
            Some(PillMode::Done { .. }) => handoff_damping(self.handoff_since) > 0.0,
            _ => false,
        }
    }

    /// Drop a deferred teardown, because the core has since said it wants the
    /// pill again.
    ///
    /// Not a lifecycle decision of the adapter's own: the core issues its
    /// commands in order, and a later `Create` or `Show` supersedes an earlier
    /// `Hide`/`Destroy` that has not been performed yet. All the adapter is
    /// doing is refusing to perform a command the core has already overruled —
    /// which is exactly what deferring it made possible.
    fn supersede_teardown(&mut self) {
        self.pending = None;
    }

    /// Build the window, off screen. Answers whether there is one afterwards —
    /// the core is told, and corrects itself, rather than going on issuing
    /// commands to a window that was never built (#53).
    ///
    /// `build` makes the window itself, given the hook's sender and the home
    /// monitor; everything around it — the reuse, the missing home, the failure
    /// — is the adapter's.
    fn create_with(
        &mut self,
        build: impl FnOnce(crossbeam_channel::Sender<HookEvent>, HomeMonitor) -> Result<W>,
    ) -> bool {
        // A window still here means a `Destroy` is deferred behind a conceal.
        // The core has now asked for a window and there is one — reuse it
        // rather than tearing a layered window down to build the same thing
        // back a frame later.
        if self.window.is_some() {
            self.supersede_teardown();
            return true;
        }
        let Some(home) = self.home else {
            tracing::error!("no home monitor to put the pill on");
            return false;
        };
        match build(self.hook_tx.clone(), home) {
            Ok(pw) => {
                self.window = Some(pw);
                true
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to create pill window");
                self.window = None;
                false
            }
        }
    }

    /// Apply a mode the core derived: start the transition into it from
    /// whatever the pill currently *looks* like.
    ///
    /// Starting from the drawn geometry rather than from the previous mode's is
    /// what makes an interrupted transition continue rather than jump — a chord
    /// pressed halfway through a reveal grows from the half-revealed nub.
    ///
    /// Note what is *not* reset on the way out of `Recording`: the meter keeps
    /// its clock, so the waveform running under the handoff is the same one
    /// that was running a frame earlier, with no sideways jump at the change.
    pub fn set_mode(&mut self, mode: PillMode, now: Instant) {
        let entering_recording = matches!(mode, PillMode::Recording { .. })
            && !matches!(self.mode, Some(PillMode::Recording { .. }));
        // Stamp the handoff on the way *into* Processing only, so a `Done` that
        // follows keeps counting from the mode change rather than restarting.
        if let PillMode::Processing { since } = mode {
            if !matches!(self.mode, Some(PillMode::Processing { .. })) {
                self.handoff_since = Some(since);
            }
        }
        if entering_recording {
            self.handoff_since = None;
            self.bands.reset();
        }
        // Click-through is off exactly while the pill has something to press —
        // the bar, or a click-started session's cancel and confirm — and the
        // flip happens with the mode rather than on a timer: the window becomes
        // a mouse target at the instant it has something to click. A hotkey
        // session shows neither, so a click passes straight through it.
        //
        // Note it goes *back on* at the start of a collapse, not the end. The
        // pill is on its way out from under the cursor either way, and a window
        // that swallowed clicks through the fade would be swallowing them for
        // the app underneath.
        let buttons = mode.shows_buttons();
        if buttons != self.mode.is_some_and(PillMode::shows_buttons) {
            if let Some(pw) = self.window.as_mut() {
                pw.set_click_through(!buttons);
            }
        }
        // The hover indicator and the label follow the *bar* rather than the
        // click-through flag, and since #47 those are two different questions:
        // a click-started session is a mouse target without being the bar, and
        // leaving "Dictate" lit and named over a pill that is now recording
        // would be the bar's chrome outliving the bar.
        if !mode.shows_bar() && self.mode.is_some_and(PillMode::shows_bar) {
            // Nothing to hover once the bar is gone, and a hover left standing
            // would light a button on the next reveal. The cursor's *position*
            // is left alone: it is what a click is tested against, and the
            // buttons a click-started session carries are still under it.
            self.clear_hover(now);
            // The *name* goes with the button, but the acknowledgement does
            // not: a copy is acknowledged for a second whether or not the
            // cursor stays, and moving away the instant you click is the
            // commonest thing to do. What does take it is a session — a
            // "Copied" over a recording pill would be saying nothing about
            // what the pill is now doing.
            if mode != PillMode::Idle {
                let dismissed = self.label.dismiss(now);
                self.owe(dismissed);
            }
        }
        let tween = pill::geom::transition(self.mode.unwrap_or(PillMode::Hidden), mode);
        self.motion = Motion::start(self.motion.at(now), mode, tween, now);
        self.mode = Some(mode);
        self.frame_owed = true;
        // Paint the first frame before the `Show` that follows reveals it, so
        // what appears is already the pill and never a blank rectangle.
        self.redraw(now);
    }

    pub fn show(&mut self, now: Instant) {
        self.supersede_teardown();
        // Paint before revealing, so what appears is already the pill and never
        // a blank rectangle. Normally a no-op: the `SetMode` that precedes
        // every `Show` has drawn that frame already, and re-pushing an
        // identical surface is the per-frame cost residency exists to avoid.
        if !self.window.as_ref().is_some_and(|pw| pw.has_frame()) {
            self.redraw(now);
        }
        if let Some(pw) = self.window.as_ref() {
            pw.show();
        }
    }

    /// Take the pill off screen — once the motion doing so has finished. The
    /// core issues `Hide` with the mode change that *is* the conceal, so hiding
    /// the window here and now would cut that animation off at its first frame.
    pub fn hide(&mut self, now: Instant) {
        self.pending = Some(Teardown::Hide);
        self.flush_pending(now);
    }

    /// Tear the window down, on the same terms as [`Self::hide`]: the conceal
    /// runs first, then the window goes.
    pub fn destroy(&mut self, now: Instant) {
        self.pending = Some(Teardown::Destroy);
        self.flush_pending(now);
    }

    /// Perform a deferred hide/destroy if the motion that had to run first is
    /// over. Called after every frame, so the teardown lands on the frame after
    /// the last one the conceal drew.
    fn flush_pending(&mut self, now: Instant) {
        let Some(teardown) = self.pending else {
            return;
        };
        if self.motion.is_running(now) {
            return;
        }
        self.pending = None;
        if let Some(pw) = self.window.as_ref() {
            pw.hide();
        }
        if teardown == Teardown::Destroy {
            // The ring goes with the window — it belongs to a capture that is
            // long over by the time the pill has no reason to exist.
            self.mode = None;
            self.ring = None;
            self.handoff_since = None;
            self.frame_owed = false;
            // Wiped rather than faded: there is nothing left to fade out *on*,
            // and the next window must not open with the tail of a crossfade
            // that belonged to one that is gone.
            self.label.reset(now);
            drop(self.window.take());
        }
    }

    /// Re-push the surface the pill is already showing. The system maintains a
    /// layered window's pixels on its own, so this is only for the events that
    /// can invalidate them out from under us — see [`PillWindow::repush`].
    pub fn repush(&mut self) {
        let Some(pw) = self.window.as_mut() else {
            return;
        };
        if let Err(e) = pw.repush() {
            tracing::error!(error = %e, "pill surface re-push failed");
        }
    }

    /// Draw one frame: the motion's geometry at `now`, with this frame's bars.
    pub fn redraw(&mut self, now: Instant) {
        let Some(pill) = self.window.as_mut() else {
            return;
        };
        let mut geom = self.motion.at(now);
        // The breath rides on top of the morph rather than being part of it:
        // it is a sustained oscillation with no end state, so it cannot be a
        // lerp between two Geoms.
        if let Some(PillMode::Processing { since }) = self.mode {
            geom = pill::geom::breathe(geom, now.saturating_duration_since(since));
        }
        // The bars' *heights* are not part of the Geom — the Geom carries the
        // row's opacity, and the waveform is live data. The handoff drains it
        // over the same 320 ms the border is crossfading across.
        let ring = matches!(self.mode, Some(PillMode::Recording { .. }))
            .then_some(self.ring.as_ref())
            .flatten();
        let bars = if geom.bars > 0.0 {
            bars_for_frame(&mut self.bands, ring, handoff_damping(self.handoff_since))
        } else {
            Vec::new()
        };
        // Per-button hover, likewise: the Geom carries the bar's *growth*, and
        // which button is lit is state beside it.
        let slots = self.hover.slots(now, |i| self.enabled[i]);
        // And the label, likewise: what it says is a surface of its own with
        // its own clock, not something a Geom could carry.
        let label = self.label.at(now);
        if let Err(e) = pill.render(&geom, &bars, &slots, &label, self.body_style) {
            tracing::error!(error = %e, "pill render failed");
        }
        // One more frame is owed while a transition is still running, so the
        // frame that lands it is drawn even if the loop wakes up past its end.
        // The hover and label fades are two smaller ones with the same need.
        self.frame_owed =
            self.motion.is_running(now) || self.hover.is_running(now) || self.label.is_running(now);
        self.flush_pending(now);
    }
}

impl PillAdapter<PillWindow> {
    pub fn window_id(&self) -> Option<WindowId> {
        self.window.as_ref().map(|pw| pw.id())
    }

    /// Build the real window, off screen. See [`Self::create_with`].
    pub fn create(&mut self, el: &ActiveEventLoop) -> bool {
        self.create_with(|hook_tx, home| PillWindow::create(el, hook_tx, home))
    }
}

/// A flat row — every bar at its resting height. What "stopped listening" looks
/// like, and all Processing and Done ever show once the handoff has run.
fn flat_bars() -> Vec<f32> {
    vec![0.0; pill::BAR_COUNT]
}

/// How much of the waveform is left, given when the handoff started. `None` —
/// no handoff yet — is the recording case's full strength.
fn handoff_damping(since: Option<Instant>) -> f32 {
    since.map_or(1.0, |t| pill::core::handoff_damping(t.elapsed()))
}

/// This frame's bars, scaled by `damping`.
///
/// `ring` is `Some` only while capture is live. Past that the meter *holds*
/// instead: the ring was drained into the worker the moment recording stopped,
/// so ticking it would ease the bars toward the silence of an empty buffer and
/// the handoff's own fall would have nothing left to take down. Either way the
/// meter's clock keeps advancing, which is what makes the waveform continuous
/// across the mode change rather than jumping sideways into the fall.
///
/// A free function over the fields it needs rather than a method: `redraw`
/// holds a mutable borrow of the window across the whole match, and `&mut self`
/// here would collide with it.
fn bars_for_frame(
    bands: &mut audio::level::BandMeter,
    ring: Option<&audio::ring::Buffer>,
    damping: f32,
) -> Vec<f32> {
    // Past the fall there is nothing left to shape — and nothing to gain from
    // advancing a meter whose output is about to be multiplied by zero.
    if damping <= 0.0 {
        return flat_bars();
    }
    let raw = match ring {
        Some(ring) => bands.tick(ring).to_vec(),
        None => bands.hold().to_vec(),
    };
    audio::level::shape_bars(&raw)
        .into_iter()
        .map(|v| v * damping)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pill::core::Origin;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    /// Everything the adapter did to its window.
    #[derive(Default)]
    struct Log {
        click_through: Vec<bool>,
        renders: usize,
        hidden: usize,
        dropped: bool,
    }

    /// A window that draws nothing and remembers what it was asked. The log is
    /// shared so it outlives the surface — a `Destroy` drops it.
    struct Fake(Rc<RefCell<Log>>);

    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.borrow_mut().dropped = true;
        }
    }

    impl PillPort for Fake {
        fn set_home(&mut self, _: HomeMonitor) -> Result<()> {
            Ok(())
        }
        fn show(&self) {}
        fn hide(&self) {
            self.0.borrow_mut().hidden += 1;
        }
        fn render(&mut self, _: &Geom, _: &[f32], _: &Slots, _: &Fade, _: BodyStyle) -> Result<()> {
            self.0.borrow_mut().renders += 1;
            Ok(())
        }
        fn set_click_through(&mut self, on: bool) {
            self.0.borrow_mut().click_through.push(on);
        }
        fn rect(&self) -> Rect {
            HOME.placement()
        }
        fn scale(&self) -> f32 {
            1.0
        }
        fn has_frame(&self) -> bool {
            self.0.borrow().renders > 0
        }
        fn repush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    const HOME: HomeMonitor = HomeMonitor {
        id: 1,
        work: Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        },
        dpi: 96,
        autohide_reserve: 0,
    };

    /// Long enough for any transition, hover fade or label fade to be over.
    const SETTLE: Duration = Duration::from_secs(5);

    const EXPANDED: PillMode = PillMode::Expanded {
        style: BodyStyle::Islands,
    };

    fn recording(origin: Origin) -> PillMode {
        PillMode::Recording { origin }
    }

    fn adapter() -> PillAdapter<Fake> {
        let (hook_tx, _) = crossbeam_channel::unbounded();
        let mut pill = PillAdapter::new(hook_tx);
        pill.set_home(HOME);
        pill
    }

    /// An adapter with a window, and the log of what it does to it.
    fn with_window() -> (PillAdapter<Fake>, Rc<RefCell<Log>>) {
        let mut pill = adapter();
        let log = Rc::new(RefCell::new(Log::default()));
        let shared = log.clone();
        assert!(pill.create_with(move |_, _| Ok(Fake(shared))));
        (pill, log)
    }

    /// Draw the frame the adapter asks for at `now`, if it asks — the app
    /// loop's one gate.
    fn pump(pill: &mut PillAdapter<Fake>, now: Instant) {
        if pill.wants_frame(now) {
            pill.redraw(now);
        }
    }

    /// The resident nub, revealed and settled.
    fn settled_nub() -> (PillAdapter<Fake>, Rc<RefCell<Log>>, Instant) {
        let (mut pill, log) = with_window();
        let t0 = Instant::now();
        pill.set_mode(PillMode::Idle, t0);
        pill.show(t0);
        let settled = t0 + SETTLE;
        pump(&mut pill, settled);
        (pill, log, settled)
    }

    // --- wants_frame ---------------------------------------------------------

    #[test]
    fn no_window_asks_for_no_frames() {
        let mut pill = adapter();
        pill.set_body_style(BodyStyle::Unified);
        assert!(!pill.wants_frame(Instant::now()));
    }

    #[test]
    fn a_settled_nub_asks_for_no_frames() {
        let (pill, log, settled) = settled_nub();
        let drawn = log.borrow().renders;
        assert!(!pill.wants_frame(settled));
        assert!(!pill.wants_frame(settled + SETTLE));
        assert_eq!(log.borrow().renders, drawn);
    }

    #[test]
    fn a_transition_asks_for_frames_until_the_one_that_lands_it() {
        let (mut pill, _, settled) = settled_nub();
        pill.set_mode(EXPANDED, settled);
        assert!(pill.wants_frame(settled + Duration::from_millis(1)));
        // Woken past the end, the landing frame is still owed — once.
        let late = settled + SETTLE;
        assert!(pill.wants_frame(late));
        pill.redraw(late);
        assert!(!pill.wants_frame(late));
    }

    #[test]
    fn recording_asks_for_frames_however_long_it_runs() {
        let (mut pill, _, settled) = settled_nub();
        pill.set_mode(recording(Origin::Hotkey), settled);
        let later = settled + SETTLE;
        pump(&mut pill, later);
        assert!(pill.wants_frame(later));
    }

    #[test]
    fn a_change_the_renderer_reads_owes_exactly_one_frame() {
        let (mut pill, _, settled) = settled_nub();
        pill.set_enabled([false; pill::core::BUTTON_COUNT]);
        assert!(pill.wants_frame(settled));
        pill.redraw(settled);
        assert!(!pill.wants_frame(settled));
        // The same value again moves nothing.
        pill.set_enabled([false; pill::core::BUTTON_COUNT]);
        assert!(!pill.wants_frame(settled));
    }

    // --- click-through -------------------------------------------------------

    #[test]
    fn the_nub_never_touches_click_through() {
        let (_, log, _) = settled_nub();
        assert!(log.borrow().click_through.is_empty());
    }

    #[test]
    fn the_bar_is_a_mouse_target_and_a_hotkey_session_is_not() {
        let (mut pill, log, t) = settled_nub();
        pill.set_mode(EXPANDED, t);
        pill.set_mode(recording(Origin::Hotkey), t);
        assert_eq!(log.borrow().click_through, [false, true]);
    }

    #[test]
    fn a_click_started_session_keeps_its_buttons_clickable() {
        let (mut pill, log, t) = settled_nub();
        pill.set_mode(EXPANDED, t);
        // Bar to cancel/confirm: buttons throughout, so no flip at all.
        pill.set_mode(recording(Origin::Click), t);
        assert_eq!(log.borrow().click_through, [false]);
        // Processing carries no buttons, whatever started it.
        pill.set_mode(PillMode::Processing { since: t }, t);
        assert_eq!(log.borrow().click_through, [false, true]);
    }

    #[test]
    fn click_through_goes_back_on_at_the_start_of_a_collapse() {
        let (mut pill, log, t) = settled_nub();
        pill.set_mode(EXPANDED, t);
        pump(&mut pill, t + SETTLE);
        let collapsing = t + SETTLE;
        pill.set_mode(PillMode::Idle, collapsing);
        assert!(pill.wants_frame(collapsing + Duration::from_millis(1)));
        assert_eq!(log.borrow().click_through, [false, true]);
    }

    // --- teardown ------------------------------------------------------------

    #[test]
    fn a_hide_waits_for_the_conceal_to_finish() {
        let (mut pill, log, t) = settled_nub();
        pill.set_mode(PillMode::Hidden, t);
        pill.hide(t);
        assert_eq!(log.borrow().hidden, 0, "the conceal was cut off");
        pump(&mut pill, t + SETTLE);
        assert_eq!(log.borrow().hidden, 1);
        assert!(pill.has_window());
    }

    #[test]
    fn a_destroy_waits_for_the_conceal_then_drops_the_window() {
        let (mut pill, log, t) = settled_nub();
        pill.set_mode(PillMode::Hidden, t);
        pill.destroy(t);
        assert!(pill.has_window());
        pump(&mut pill, t + SETTLE);
        assert!(!pill.has_window());
        assert!(log.borrow().dropped);
        assert!(!pill.wants_frame(t + SETTLE));
    }

    #[test]
    fn a_show_supersedes_a_deferred_hide() {
        let (mut pill, log, t) = settled_nub();
        pill.set_mode(PillMode::Hidden, t);
        pill.hide(t);
        pill.set_mode(PillMode::Idle, t);
        pill.show(t);
        pump(&mut pill, t + SETTLE);
        assert_eq!(log.borrow().hidden, 0);
        assert!(pill.is_active());
    }

    #[test]
    fn a_create_supersedes_a_deferred_destroy_and_reuses_the_window() {
        let (mut pill, log, t) = settled_nub();
        pill.set_mode(PillMode::Hidden, t);
        pill.destroy(t);
        assert!(pill.create_with(|_, _| panic!("rebuilt a window that was still there")));
        pump(&mut pill, t + SETTLE);
        assert!(pill.has_window());
        assert!(!log.borrow().dropped);
        assert_eq!(log.borrow().hidden, 0);
    }

    #[test]
    fn create_without_a_home_builds_nothing() {
        let (hook_tx, _) = crossbeam_channel::unbounded();
        let mut pill = PillAdapter::<Fake>::new(hook_tx);
        assert!(!pill.create_with(|_, _| panic!("built with no home")));
        assert!(!pill.has_window());
    }

    #[test]
    fn a_failed_create_leaves_no_window() {
        let mut pill = adapter();
        assert!(!pill.create_with(|_, _| Err(anyhow::anyhow!("no display"))));
        assert!(!pill.has_window());
    }
}
