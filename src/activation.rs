// Activation FSM — translates raw hotkey press/release events into Start/Stop
// session events, according to the selected mode.

use std::time::{Duration, Instant};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Hold { double_press_lock: bool },
    Toggle,
}

#[derive(Debug)]
pub enum InEvent {
    Pressed(Instant),
    Released(Instant),
}

#[derive(Debug, PartialEq, Eq)]
pub enum OutEvent {
    Start,
    Stop,
    Ignore,
}

pub struct Fsm {
    mode: Mode,
    recording: bool,
    locked: bool,
    last_press: Option<Instant>,
    busy_until: Option<Instant>,
    dbl_window: Duration,
}

impl Fsm {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            recording: false,
            locked: false,
            last_press: None,
            busy_until: None,
            dbl_window: Duration::from_millis(300),
        }
    }

    // Suppress presses until `until`. Wired into `step` (and tested), but not
    // yet engaged by the app event loop — kept for a future debounce.
    #[allow(dead_code)]
    pub fn mark_busy(&mut self, until: Instant) {
        self.busy_until = Some(until);
    }

    pub fn step(&mut self, ev: InEvent) -> OutEvent {
        let now = match ev {
            InEvent::Pressed(t) | InEvent::Released(t) => t,
        };
        if matches!(self.busy_until, Some(t) if now < t) {
            return OutEvent::Ignore;
        }

        match (self.mode, ev) {
            (Mode::Toggle, InEvent::Pressed(_)) => {
                self.recording = !self.recording;
                if self.recording {
                    OutEvent::Start
                } else {
                    OutEvent::Stop
                }
            }
            (Mode::Toggle, InEvent::Released(_)) => OutEvent::Ignore,

            (Mode::Hold { double_press_lock }, InEvent::Pressed(t)) => {
                let is_double = double_press_lock
                    && self
                        .last_press
                        .map_or(false, |p| t.duration_since(p) <= self.dbl_window);
                self.last_press = Some(t);
                if self.locked {
                    self.locked = false;
                    self.recording = false;
                    OutEvent::Stop
                } else if is_double {
                    self.locked = true;
                    if self.recording {
                        OutEvent::Ignore
                    } else {
                        self.recording = true;
                        OutEvent::Start
                    }
                } else if !self.recording {
                    self.recording = true;
                    OutEvent::Start
                } else {
                    OutEvent::Ignore
                }
            }
            (Mode::Hold { .. }, InEvent::Released(_)) => {
                if self.locked {
                    OutEvent::Ignore
                } else if self.recording {
                    self.recording = false;
                    OutEvent::Stop
                } else {
                    OutEvent::Ignore
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Instant {
        // Build a stable monotonic series for tests.
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_millis(ms)
    }

    #[test]
    fn toggle_mode_flips_on_each_press() {
        let mut f = Fsm::new(Mode::Toggle);
        assert_eq!(f.step(InEvent::Pressed(t(0))), OutEvent::Start);
        assert_eq!(f.step(InEvent::Released(t(50))), OutEvent::Ignore);
        assert_eq!(f.step(InEvent::Pressed(t(100))), OutEvent::Stop);
    }

    #[test]
    fn hold_mode_starts_on_press_stops_on_release() {
        let mut f = Fsm::new(Mode::Hold { double_press_lock: false });
        assert_eq!(f.step(InEvent::Pressed(t(0))), OutEvent::Start);
        assert_eq!(f.step(InEvent::Released(t(200))), OutEvent::Stop);
    }

    #[test]
    fn hold_with_lock_double_press_holds_through_release() {
        let mut f = Fsm::new(Mode::Hold { double_press_lock: true });
        // First press: start recording (will release immediately to set up double-tap).
        assert_eq!(f.step(InEvent::Pressed(t(0))), OutEvent::Start);
        assert_eq!(f.step(InEvent::Released(t(50))), OutEvent::Stop);
        // Second press within window: lock.
        assert_eq!(f.step(InEvent::Pressed(t(100))), OutEvent::Start);
        // Release after lock: ignored.
        assert_eq!(f.step(InEvent::Released(t(200))), OutEvent::Ignore);
        // Next press: unlock + stop.
        assert_eq!(f.step(InEvent::Pressed(t(1000))), OutEvent::Stop);
    }

    #[test]
    fn busy_window_ignores_presses() {
        let mut f = Fsm::new(Mode::Toggle);
        f.mark_busy(t(500));
        assert_eq!(f.step(InEvent::Pressed(t(100))), OutEvent::Ignore);
        assert_eq!(f.step(InEvent::Pressed(t(600))), OutEvent::Start);
    }
}
