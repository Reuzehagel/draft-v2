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
//   Wait      — nothing to look for: the pill is off screen, the display is
//               off, or the session is locked. No timer at all.
//   Animating — a frame is due; 33 ms is the pill's 30 Hz.
//   Near      — the cursor is close enough to arrive on the pill within a
//               poll or two; 50 ms, which is what hover used to cost always.
//   Far       — the cursor is somewhere else entirely; 250 ms.
//   Watching  — the pill is off screen but Draft is still watching for the
//               fullscreen app that took it there to go away; 1 s.
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

use crate::pill::monitor::Rect;

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
    Watching,
}

impl Rung {
    /// How long the loop may sleep on this rung — `None` for no timer at all.
    pub fn period(self) -> Option<Duration> {
        match self {
            Rung::Wait => None,
            Rung::Animating => Some(Duration::from_millis(33)),
            Rung::Near => Some(Duration::from_millis(50)),
            Rung::Far => Some(Duration::from_millis(250)),
            Rung::Watching => Some(crate::pill::fullscreen::POLL_INTERVAL),
        }
    }
}

/// What the ladder is decided from. Four booleans, gathered by the adapter
/// because only it can know any of them.
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
    pub watching: bool,
}

/// The rung these signals put the loop on.
///
/// `awake` outranks everything, animation included: the instruction for a
/// display that has gone off is to stop rendering, and a locked session has
/// nobody in front of it. Coming back is an event — the pill hook surfaces both
/// — so nothing has to be polling to notice.
pub fn rung(s: Signals) -> Rung {
    if !s.awake {
        return Rung::Wait;
    }
    if s.animating {
        return Rung::Animating;
    }
    if !s.reachable {
        return if s.watching {
            Rung::Watching
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

/// Whether `cursor` is inside `rect` grown by `band` on every side. Physical
/// virtual-screen pixels throughout — `GetCursorPos` and the window's placement
/// are both in that space.
pub fn near(rect: Rect, cursor: (i32, i32), band: i32) -> bool {
    cursor.0 >= rect.left - band
        && cursor.0 < rect.right + band
        && cursor.1 >= rect.top - band
        && cursor.1 < rect.bottom + band
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
                watching: false,
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
            watching: false,
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
            watching: true,
            ..Default::default()
        };
        assert_eq!(rung(s), Rung::Watching);
        assert_eq!(
            rung(s).period(),
            Some(crate::pill::fullscreen::POLL_INTERVAL)
        );
        assert_eq!(
            rung(Signals {
                watching: false,
                ..s
            }),
            Rung::Wait
        );
    }

    /// Nobody is looking: stop rendering, stop polling, wait for the event that
    /// says the display is back. A game on a dark screen is not worth watching
    /// either — the unlock and the display coming back are both events.
    #[test]
    fn a_dark_or_locked_session_outranks_everything() {
        for (animating, reachable, near, watching) in [
            (false, false, false, false),
            (true, true, true, true),
            (false, true, true, false),
            (false, false, false, true),
        ] {
            let s = Signals {
                awake: false,
                animating,
                reachable,
                near,
                watching,
            };
            assert_eq!(
                rung(s),
                Rung::Wait,
                "{animating} {reachable} {near} {watching}"
            );
        }
    }

    /// Every period is one of Microsoft's recommended values, and none is
    /// shorter than the frame rate.
    #[test]
    fn the_ladder_only_ever_slows_down() {
        let periods =
            [Rung::Animating, Rung::Near, Rung::Far, Rung::Watching].map(|r| r.period().unwrap());
        assert!(periods.windows(2).all(|w| w[0] < w[1]), "{periods:?}");
    }

    #[test]
    fn the_near_band_grows_the_pills_rect_on_every_side() {
        let rect = Rect {
            left: 100,
            top: 100,
            right: 200,
            bottom: 200,
        };
        // Inside the rect itself.
        assert!(near(rect, (150, 150), 10));
        // Inside the band, on each side.
        assert!(near(rect, (92, 150), 10));
        assert!(near(rect, (208, 150), 10));
        assert!(near(rect, (150, 92), 10));
        assert!(near(rect, (150, 208), 10));
        // Outside it, on each side.
        assert!(!near(rect, (89, 150), 10));
        assert!(!near(rect, (210, 150), 10));
        assert!(!near(rect, (150, 89), 10));
        assert!(!near(rect, (150, 210), 10));
        // A cursor on another monitor entirely.
        assert!(!near(rect, (-1900, 400), 180));
    }
}
