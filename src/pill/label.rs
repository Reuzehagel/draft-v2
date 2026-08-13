// The label: the text above the pill that names the button under the cursor,
// and says "Copied" when a copy lands.
//
// It does two jobs and **no others**. It is the only thing that makes an
// icon-only bar learnable, and it is what stops Copy looking inert: Draft
// pastes via clipboard + Ctrl+V, so the last transcript is usually already on
// the clipboard, and a copy that worked perfectly looks exactly like a click
// that did nothing.
//
// It reports no status of any kind — not the provider, not the input device,
// not the last error. One idea per surface: the pill is what you *do* things
// with, and the tray and settings are where you look things up (#46).
//
// Pure, like `pill::core` and `pill::geom`: text and `now` in, a [`Fade`] out.
// It does not know how wide a string is, because that needs a face and this is
// not the module that has one — the renderer measures, which is what lets the
// state machine be asserted without a font installed.

use crate::pill::core::BUTTONS;
use crate::pill::geom::{Ease, Tween};
use std::time::{Duration, Instant};

/// What a landed copy says. Not "Copied to clipboard": the label is one line
/// over a 36px nub, and the extra words say nothing the first one doesn't.
pub const COPIED: &str = "Copied";

/// How long "Copied" holds before the label goes back to naming whatever is
/// under the cursor. Long enough to be read by someone who was looking at the
/// window they meant to paste into rather than at the pill.
pub const COPIED_LINGER: Duration = Duration::from_millis(1000);

/// The crossfade from one text to the next.
///
/// **Linear, unlike every other tween the pill runs.** The pair of texts
/// overlap in the same place, so what matters is that their opacities sum to
/// one throughout; an out-cubic pair would put both strings near full opacity
/// through the middle of the fade, which is two legible words on top of each
/// other rather than one changing into another.
pub const LABEL_FADE: Tween = Tween {
    dur: Duration::from_millis(110),
    ease: Ease::Linear,
};

/// The label's whole appearance this frame: what it is fading from, what to,
/// and how far along.
///
/// Either side may be `None` — that is the label arriving from nothing and
/// leaving for it, which is the same crossfade with one layer missing rather
/// than a separate appear/disappear animation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Fade {
    pub from: Option<&'static str>,
    pub to: Option<&'static str>,
    /// 0..1, already eased.
    pub t: f32,
}

impl Fade {
    /// Whether this frame draws anything at all — which is what the label is
    /// whenever no button is hovered and no copy is being acknowledged, i.e.
    /// almost always.
    pub fn is_blank(&self) -> bool {
        self.opacities() == (0.0, 0.0)
    }

    /// The outgoing and incoming texts' opacities. A missing side is 0 rather
    /// than a text at 0 — the renderer has nothing to measure for it.
    pub fn opacities(&self) -> (f32, f32) {
        let t = self.t.clamp(0.0, 1.0);
        (
            if self.from.is_some() { 1.0 - t } else { 0.0 },
            if self.to.is_some() { t } else { 0.0 },
        )
    }
}

/// The label's state: what it is showing, what it was showing, and when it
/// started changing between them.
///
/// The two sources are held separately rather than merged on arrival, because
/// they outlive each other in both directions: a flash has to survive the
/// cursor moving to another button, and the hover underneath it has to still be
/// there when the flash expires.
#[derive(Clone, Copy, Debug)]
pub struct Label {
    /// The name of the button under the cursor.
    hovered: Option<&'static str>,
    /// An acknowledgement and the moment it stops being shown.
    flash: Option<(&'static str, Instant)>,
    /// What the crossfade is heading for — the resolution of the two above.
    shown: Option<&'static str>,
    /// What it is coming from, so a change from one text to another dims the
    /// old one over exactly the 110 ms the new one comes up.
    previous: Option<&'static str>,
    started: Instant,
}

impl Label {
    pub fn new(now: Instant) -> Self {
        Self {
            hovered: None,
            flash: None,
            shown: None,
            previous: None,
            started: now,
        }
    }

    /// Name the button at `index`, or nothing at all.
    ///
    /// `None` covers both halves of the "nothing is hovered" rule: off the bar
    /// entirely, and over the inert end padding, which the Pill core already
    /// reports as no button.
    ///
    /// Says whether that moved the label, so the caller can decide a frame is
    /// owed rather than testing the same thing again.
    pub fn set_hover(&mut self, index: Option<usize>, now: Instant) -> bool {
        self.hovered = index.map(|i| BUTTONS[i].name);
        self.resolve(now)
    }

    /// Acknowledge something that just happened, for [`COPIED_LINGER`].
    ///
    /// It outranks the hover for as long as it holds — the cursor is on Copy
    /// when this fires, and naming the button the user just pressed is exactly
    /// the reading that makes the press look like it did nothing.
    pub fn flash(&mut self, text: &'static str, now: Instant) -> bool {
        self.flash = Some((text, now + COPIED_LINGER));
        self.resolve(now)
    }

    /// Retire an expired flash. Costs nothing while none is running, and is the
    /// only thing that moves the label without an event behind it — so the app
    /// loop calls it every pass, and draws a frame when it says so.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.resolve(now)
    }

    /// Take the label away: no hover, no flash, fading out from whatever it was
    /// showing. What the bar collapsing means — a "Copied" that outlived its
    /// button would sit over a recording pill saying nothing about it.
    pub fn dismiss(&mut self, now: Instant) -> bool {
        self.hovered = None;
        self.flash = None;
        self.resolve(now)
    }

    /// Wipe it, mid-crossfade and all. For a window being destroyed, where
    /// there is nothing left to fade out *on* — the next one must not open with
    /// the tail of a fade that belonged to a window that is gone.
    pub fn reset(&mut self, now: Instant) {
        *self = Self::new(now);
    }

    /// Point the label at whatever the two sources now add up to, and say
    /// whether that changed it.
    fn resolve(&mut self, now: Instant) -> bool {
        let want = self
            .flash
            .filter(|(_, until)| now < *until)
            .map(|(text, _)| text)
            .or(self.hovered);
        if want == self.shown {
            return false;
        }
        self.previous = self.shown;
        self.shown = want;
        self.started = now;
        true
    }

    fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        LABEL_FADE
            .ease
            .apply(elapsed / LABEL_FADE.dur.as_secs_f32())
    }

    /// This frame's crossfade.
    pub fn at(&self, now: Instant) -> Fade {
        Fade {
            from: self.previous,
            to: self.shown,
            t: self.progress(now),
        }
    }

    /// Whether the crossfade still has frames to draw.
    ///
    /// Deliberately not true for the length of a *flash*: a held "Copied" is a
    /// still image, and 1000 ms of redrawing it would be a second of pushing
    /// identical pixels. What ends the flash is [`Self::tick`].
    pub fn is_running(&self, now: Instant) -> bool {
        self.previous != self.shown && self.progress(now) < 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pill::core::Action;

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    /// The index of the button with `action`, so the tests name buttons the way
    /// the bar does rather than by a position that could be re-ordered.
    fn button(action: Action) -> usize {
        BUTTONS.iter().position(|b| b.action == action).unwrap()
    }

    /// Nothing hovered, nothing acknowledged: the label is not on screen. The
    /// end padding reaches here too — the Pill core reports it as no button.
    #[test]
    fn a_label_with_nothing_to_say_draws_nothing() {
        let mut l = Label::new(t(0));
        assert!(l.at(t(0)).is_blank());
        assert!(!l.set_hover(None, t(0)), "no change to make");
        assert!(l.at(t(1000)).is_blank());
        assert!(!l.is_running(t(0)));
    }

    /// Hovering a button names it, over the settled 110 ms.
    #[test]
    fn hovering_a_button_fades_its_name_in() {
        let mut l = Label::new(t(0));
        assert!(l.set_hover(Some(button(Action::Copy)), t(0)));
        let f = l.at(t(0));
        assert_eq!(f.to, Some("Copy last transcript"));
        assert_eq!(f.from, None);
        assert_eq!(f.opacities(), (0.0, 0.0));
        assert!(l.is_running(t(0)));
        assert_eq!(l.at(t(110)).opacities(), (0.0, 1.0));
        assert!(!l.is_running(t(110)));
    }

    /// Sliding from one button to the next is one crossfade: the arriving name
    /// comes up over exactly the 110 ms the leaving one goes down, and the two
    /// opacities sum to one throughout.
    #[test]
    fn the_name_crossfades_between_buttons() {
        let mut l = Label::new(t(0));
        l.set_hover(Some(button(Action::Copy)), t(0));
        assert!(l.set_hover(Some(button(Action::Settings)), t(110)));
        for ms in 110..=220 {
            let f = l.at(t(ms));
            assert_eq!(
                (f.from, f.to),
                (Some("Copy last transcript"), Some("Settings"))
            );
            let (a, b) = f.opacities();
            assert!((a + b - 1.0).abs() < 1e-5, "at {ms}ms: {a} + {b}");
        }
        assert_eq!(l.at(t(220)).opacities(), (0.0, 1.0));
    }

    /// A cursor wandering inside one slab is not a change: restarting the fade
    /// on every `CursorMoved` would leave the name permanently half-drawn.
    #[test]
    fn re_hovering_the_same_button_does_not_restart_the_fade() {
        let mut l = Label::new(t(0));
        l.set_hover(Some(button(Action::Settings)), t(0));
        assert!(!l.set_hover(Some(button(Action::Settings)), t(55)));
        assert_eq!(l.at(t(110)).opacities(), (0.0, 1.0));
        assert!(!l.is_running(t(110)));
    }

    /// Leaving the bar takes the label with it, over the same crossfade — out
    /// to nothing rather than by some second disappear animation.
    #[test]
    fn leaving_the_bar_fades_the_name_out() {
        let mut l = Label::new(t(0));
        l.set_hover(Some(button(Action::Dictate)), t(0));
        assert!(l.set_hover(None, t(110)));
        let f = l.at(t(165));
        assert_eq!((f.from, f.to), (Some("Dictate"), None));
        let (a, b) = f.opacities();
        assert!(a > 0.0 && b == 0.0, "{a} {b}");
        assert!(l.at(t(220)).is_blank());
    }

    /// The copy acknowledgement: exactly 1000 ms, then back to whatever the
    /// cursor is on — which, since the user just clicked it, is Copy.
    #[test]
    fn a_copy_flashes_for_its_linger_and_then_names_the_button_again() {
        let mut l = Label::new(t(0));
        l.set_hover(Some(button(Action::Copy)), t(0));
        assert!(l.flash(COPIED, t(200)));
        assert_eq!(l.at(t(310)).to, Some(COPIED));
        // Still up a millisecond short of the linger...
        assert!(!l.tick(t(1199)));
        assert_eq!(l.at(t(1199)).to, Some(COPIED));
        // ...and gone on the crossing, back to the button under the cursor.
        assert!(l.tick(t(1200)));
        let f = l.at(t(1200));
        assert_eq!((f.from, f.to), (Some(COPIED), Some("Copy last transcript")));
        assert_eq!(COPIED_LINGER, Duration::from_millis(1000));
    }

    /// A held flash is a still image. Asking for frames through the whole
    /// second would be a second of pushing identical pixels — the very cost the
    /// resident pill is built to avoid.
    #[test]
    fn a_held_flash_asks_for_no_frames() {
        let mut l = Label::new(t(0));
        l.flash(COPIED, t(0));
        assert!(l.is_running(t(50)), "the fade in still has frames");
        assert!(!l.is_running(t(500)), "a settled flash wants no frames");
    }

    /// The flash outranks the hover while it holds — including the cursor
    /// moving on to another button, which is where "Copied" would otherwise be
    /// replaced by a name a beat after the click that earned it.
    #[test]
    fn a_flash_outranks_the_hover_underneath_it() {
        let mut l = Label::new(t(0));
        l.set_hover(Some(button(Action::Copy)), t(0));
        l.flash(COPIED, t(0));
        assert!(!l.set_hover(Some(button(Action::Settings)), t(200)));
        assert_eq!(l.at(t(200)).to, Some(COPIED));
        // And when it expires, the hover it was covering is what surfaces.
        assert!(l.tick(t(1000)));
        assert_eq!(l.at(t(1000)).to, Some("Settings"));
    }

    /// The bar collapsing takes everything with it, flash included: a "Copied"
    /// left standing would sit over a recording pill saying nothing about it.
    #[test]
    fn dismissing_the_label_drops_a_running_flash() {
        let mut l = Label::new(t(0));
        l.set_hover(Some(button(Action::Copy)), t(0));
        l.flash(COPIED, t(0));
        assert!(l.dismiss(t(200)));
        assert!(l.at(t(310)).is_blank());
        // And the hover underneath is gone too, so it cannot surface later.
        assert!(!l.tick(t(2000)));
    }

    /// A reset is a wipe, not a fade — for a window that is being destroyed,
    /// where there is nothing left to fade out on.
    #[test]
    fn a_reset_leaves_no_tail_for_the_next_window() {
        let mut l = Label::new(t(0));
        l.set_hover(Some(button(Action::Settings)), t(0));
        l.reset(t(50));
        assert!(l.at(t(50)).is_blank());
        assert!(!l.is_running(t(50)));
    }

    /// Every button has a name, and they are the ones the ticket settled. The
    /// bar is icon-only; a button whose name were empty would be unlearnable.
    #[test]
    fn every_button_is_named() {
        let names: Vec<&str> = BUTTONS.iter().map(|b| b.name).collect();
        assert_eq!(names, vec!["Copy last transcript", "Dictate", "Settings"]);
        assert!(names.iter().all(|n| !n.trim().is_empty()));
    }
}
