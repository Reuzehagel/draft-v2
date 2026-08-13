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
/// `Off` and `Resident { expanded: false }` are both reachable — the residency
/// toggle sets them. The other two are their drivers': `Suppressed` is the
/// fullscreen watcher's (#45), and `expanded` the hover poller's (#19). The
/// rules are here, and tested, ahead of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
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
    /// The pill's own Dictate button. Nothing can produce one until the button
    /// bar lands (#29); the mode it derives is asserted in the tests below.
    #[allow(dead_code)]
    Click,
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
        }
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

    /// Set the presence axis. Called by the residency toggle at launch and on
    /// every config reload; the fullscreen watcher (#45) and the hover poller
    /// (#19) become its other callers in turn.
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
}
