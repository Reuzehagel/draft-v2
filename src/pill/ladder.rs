// The proximity ladder: the one thing in Draft that arms a timer.
//
// The event loop's resting state is `ControlFlow::Wait` — no timer, no wakeup,
// nothing until a producer posts to it. Everything that used to ride the old
// unconditional 50 ms tick now wakes the loop for itself (#49), with one
// exception that cannot: **hover**. A click-through window is not a mouse
// target, so the cursor arriving over the pill is not an event anyone can send;
// the only way to find out is to look. That poll is the whole reason a timer
// still exists, so the ladder is stated in terms of the cursor.
//
// Five rungs, and the order between them is the whole design:
//
//   Wait       — nothing to look for: the pill is off screen, the display is
//                off, or the session is locked. No timer at all.
//   Animating  — a frame is due; 33 ms is the pill's 30 Hz.
//   Near       — the cursor is close enough to arrive on the pill within a
//                poll or two; 50 ms, which is what hover used to cost always.
//   Far        — the cursor is somewhere else entirely; 250 ms.
//   Suppressed — the pill is off screen behind a fullscreen app, and only the
//                fullscreen watcher's own backstop can see it come back; 1 s.
//   Retrying   — a window failed to build and the core is waiting to ask
//                again; 1 s, which is the core's own shortest wait (#54).
//
// The periods are Microsoft's own list — "you should use timer periods of 50,
// 100, 250, 500, and 1,000 ms" (Windows Timer Coalescing whitepaper) — with
// 33 ms as the one deliberate exception, because a frame rate is not a poll.
//
// A far cursor waits up to 250 ms before the pill notices it, and that is not
// the latency it looks like: the cursor has to cross the near band first, which
// drops the period to 50 ms well before it reaches the nub.
//
// **Why suppression is a rung rather than `Wait`.** Nothing fires when an app
// leaves fullscreen *in place* — a browser dropping out of F11 does not change
// the foreground window, and `SHQueryUserNotificationState` is documented as
// notifying nobody either way. The fullscreen watcher's 1 s backstop is the
// only thing that ever sees it, so a suppressed pill that armed no timer would
// be a pill that never came back. It is the slowest rung for the same reason it
// exists: the pill is behind a game, which is where battery actually matters.

use std::time::Duration;

/// How far outside the pill's window a cursor still counts as **near**, in
/// logical pixels.
///
/// Generous on purpose. It is not a hover reach — nothing happens at this
/// boundary except the poll speeding up — and the cost of being too small is a
/// hover that starts late, while the cost of being too large is a 50 ms poll
/// while the cursor is merely in the same corner of the screen. At a normal
/// pointer speed this band takes several 250 ms polls to cross.
pub const NEAR_BAND: f32 = 180.0;

/// Which rung the loop is on, i.e. what it is waiting for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rung {
    /// Nothing to poll for. No timer is armed; only a producer can wake us.
    Wait,
    /// The pill has a frame to draw.
    Animating,
    /// The cursor is within [`NEAR_BAND`] of the pill.
    Near,
    /// The cursor is elsewhere.
    Far,
    /// The pill is off screen behind a fullscreen app, and the only thing that
    /// will ever notice it leaving is the watcher's own backstop.
    Suppressed,
    /// A `Create` failed and the Pill core is waiting to try again. Like
    /// `Suppressed`, a backstop for a state nothing else will report — and
    /// deliberately not `Animating`: the core's waits are seconds long, so
    /// asking at the frame rate would burn thirty wakeups a second to reach
    /// one create.
    ///
    /// One period rather than the core's current wait, which decays to 30 s:
    /// a rung is a *maximum* sleep and the ladder's are Microsoft's coalescing
    /// values, so the alternative is a rung carrying a deadline — a second
    /// clock, in the module whose whole job is that there is one. A wakeup a
    /// second is what a fullscreen app already costs, and this one only runs
    /// while the pill is broken.
    Retrying,
}

impl Rung {
    /// How long the loop may sleep on this rung — `None` for no timer at all.
    pub fn period(self) -> Option<Duration> {
        match self {
            Rung::Wait => None,
            Rung::Animating => Some(Duration::from_millis(33)),
            Rung::Near => Some(Duration::from_millis(50)),
            Rung::Far => Some(Duration::from_millis(250)),
            Rung::Suppressed => Some(crate::pill::fullscreen::POLL_INTERVAL),
            Rung::Retrying => Some(crate::pill::core::RETRY_FIRST),
        }
    }
}

/// What the ladder is decided from. Five facts, gathered by the adapter because
/// only it can know any of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Signals {
    /// Whether anyone is looking: the display is on and the session unlocked.
    pub awake: bool,
    /// Whether the pill needs the frame cadence: a transition running, or a
    /// terminal flash whose linger has to expire on time.
    pub animating: bool,
    /// Whether the pill is on screen at all — the thing a cursor could reach.
    /// Deliberately not "a window exists": a resident pill's window outlives
    /// every hide, and a hidden pill is not worth polling for.
    pub reachable: bool,
    /// Whether the cursor is within [`NEAR_BAND`] of the pill.
    pub near: bool,
    /// Whether the fullscreen watcher still has something to look for: the pill
    /// is suppressed, and no event will report the app leaving fullscreen.
    pub suppressed: bool,
    /// Whether the Pill core is holding a `Create` that failed. Nothing will
    /// report the reason for that failure clearing either, so the loop has to
    /// come back and let the core ask again.
    pub retrying: bool,
}

/// The rung these signals put the loop on.
///
/// `awake` outranks everything, animation included: the instruction for a
/// display that has gone off is to stop rendering, and a locked session has
/// nobody in front of it. Coming back is an event — the pill hook surfaces both
/// — so nothing has to be polling to notice.
///
/// It follows that a terminal flash cannot expire while the screen is off, and
/// that is the right answer rather than a cost: the flash is a *report*, and
/// one that ran out to nobody would be a report nobody received. It retires on
/// the same pass that hears the display is back.
pub fn rung(s: Signals) -> Rung {
    if !s.awake {
        return Rung::Wait;
    }
    if s.animating {
        return Rung::Animating;
    }
    // Above reachability rather than inside it: a pill with no window is not
    // reachable by definition, so this would otherwise sit in the same branch
    // saying the same thing twice.
    if s.retrying {
        return Rung::Retrying;
    }
    if !s.reachable {
        return if s.suppressed {
            Rung::Suppressed
        } else {
            Rung::Wait
        };
    }
    if s.near {
        Rung::Near
    } else {
        Rung::Far
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the change: doing nothing costs no timer.
    #[test]
    fn a_pill_that_is_not_on_screen_arms_nothing() {
        let s = Signals {
            awake: true,
            reachable: false,
            ..Default::default()
        };
        assert_eq!(rung(s), Rung::Wait);
        assert_eq!(rung(s).period(), None);
    }

    /// A resident nub with the cursor nowhere near it — the state the app
    /// spends nearly all its life in.
    #[test]
    fn a_settled_nub_polls_slowly_and_speeds_up_as_the_cursor_arrives() {
        let far = Signals {
            awake: true,
            reachable: true,
            ..Default::default()
        };
        assert_eq!(rung(far), Rung::Far);
        assert_eq!(rung(far).period(), Some(Duration::from_millis(250)));
        let near = Signals { near: true, ..far };
        assert_eq!(rung(near), Rung::Near);
        assert_eq!(rung(near).period(), Some(Duration::from_millis(50)));
    }

    /// 30 Hz while the pill is moving, wherever the cursor is.
    #[test]
    fn an_animating_pill_asks_for_frames_rather_than_polls() {
        for near in [false, true] {
            let s = Signals {
                awake: true,
                animating: true,
                reachable: true,
                near,
                suppressed: false,
                retrying: false,
            };
            assert_eq!(rung(s), Rung::Animating, "near {near}");
            assert_eq!(rung(s).period(), Some(Duration::from_millis(33)));
        }
    }

    /// A pill can animate its way off screen — the conceal is a transition, and
    /// `reachable` goes false at its first frame. The frames still have to be
    /// drawn, so animation outranks reachability.
    #[test]
    fn a_pill_animating_off_screen_still_gets_its_frames() {
        let s = Signals {
            awake: true,
            animating: true,
            reachable: false,
            near: false,
            suppressed: false,
            retrying: false,
        };
        assert_eq!(rung(s), Rung::Animating);
    }

    /// A pill behind a game keeps the slowest poll there is, because leaving
    /// fullscreen in place is a transition nothing reports. Off screen with
    /// nothing to watch for is the case that reaches `Wait`.
    #[test]
    fn a_suppressed_pill_keeps_the_watchers_backstop() {
        let s = Signals {
            awake: true,
            reachable: false,
            suppressed: true,
            ..Default::default()
        };
        assert_eq!(rung(s), Rung::Suppressed);
        assert_eq!(
            rung(s).period(),
            Some(crate::pill::fullscreen::POLL_INTERVAL)
        );
        assert_eq!(
            rung(Signals {
                suppressed: false,
                ..s
            }),
            Rung::Wait
        );
    }

    /// A window that failed to build is off screen and unreachable, which on
    /// its own is `Wait` — and a pill waiting on `Wait` is a pill that never
    /// retries. It gets the same second the fullscreen backstop does, and for
    /// the same reason: nothing else will ever report the state changing.
    #[test]
    fn a_pill_waiting_to_retry_a_window_keeps_being_asked() {
        let s = Signals {
            awake: true,
            reachable: false,
            retrying: true,
            ..Default::default()
        };
        assert_eq!(rung(s), Rung::Retrying);
        assert_eq!(rung(s).period(), Some(crate::pill::core::RETRY_FIRST));
        // Seconds, not frames — the core's shortest wait is a second, so the
        // frame rate would be thirty wakeups to reach one create.
        assert!(rung(s).period() > Rung::Animating.period());
        assert_eq!(
            rung(Signals {
                retrying: false,
                ..s
            }),
            Rung::Wait
        );
    }

    /// Nobody is looking: stop rendering, stop polling, wait for the event that
    /// says the display is back. A game on a dark screen is not worth watching
    /// either — the unlock and the display coming back are both events.
    ///
    /// A retry waits too, on the flash's own reasoning: building a pill onto a
    /// dark screen is doing the work for nobody, and the pass that hears the
    /// display is back settles the core with a deadline long since due.
    #[test]
    fn a_dark_or_locked_session_outranks_everything() {
        for (animating, reachable, near, suppressed, retrying) in [
            (false, false, false, false, false),
            (true, true, true, true, true),
            (false, true, true, false, false),
            (false, false, false, true, false),
            (false, false, false, false, true),
        ] {
            let s = Signals {
                awake: false,
                animating,
                reachable,
                near,
                suppressed,
                retrying,
            };
            assert_eq!(
                rung(s),
                Rung::Wait,
                "{animating} {reachable} {near} {suppressed} {retrying}"
            );
        }
    }

    /// Every period is one of Microsoft's recommended values, and none is
    /// shorter than the frame rate.
    #[test]
    fn the_ladder_only_ever_slows_down() {
        let periods =
            [Rung::Animating, Rung::Near, Rung::Far, Rung::Suppressed].map(|r| r.period().unwrap());
        assert!(periods.windows(2).all(|w| w[0] < w[1]), "{periods:?}");
        // The two backstops sit together at the bottom: neither is polling for
        // the cursor, and both are waiting on something no event reports.
        assert_eq!(Rung::Retrying.period(), Rung::Suppressed.period());
    }
}
