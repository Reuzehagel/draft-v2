// The fullscreen watcher — the pill getting out of the way of a game, a video,
// or a presentation.
//
// This is not only polish. A topmost window over a fullscreen app can force the
// desktop compositor back into composed mode — watts, not microwatts — which is
// what makes auto-hide load-bearing on a laptop rather than a nicety. It also
// decides *how* the pill leaves: `SW_HIDE`, never alpha 0 and never cloaking,
// both of which keep a composited topmost window on screen and pay exactly the
// cost this exists to avoid. The hide itself is the Pill core's `Command::Hide`
// (see [`crate::pill::window::PillWindow::hide`]); all this module decides is
// when.
//
// Suppression is of the **resident presence only, never session feedback**. A
// chord press is an explicit request, and dictating with no bars and no
// green/red flash is worse than a pill briefly over a game — which the Pill
// core already guarantees, because activity outranks presence.
//
// Two halves, split the way `pill::monitor` is:
//
//   [`Watcher`] — pure. A [`Probe`] plus the home monitor in, "suppressed or
//                 not" out, and only when the answer changed. It owns the
//                 cadence and every guard, which is what makes them assertable
//                 without a game to alt-tab into.
//   `win`       — the Win32 half: take the probe, and hear about a foreground
//                 change.
//
// **Two signals, OR'd**, because neither alone is enough:
//
//   Geometry — the foreground window's rect against its monitor's `rcMonitor`.
//              The load-bearing one: modern games default to borderless
//              windowed, and browser F11, VLC and UWP video all resize a real
//              top-level window to the monitor. It is also the only signal with
//              any idea *which* monitor is covered.
//   `SHQueryUserNotificationState` — the shell's own "do not disturb", which
//              catches legacy DX exclusive fullscreen, the screensaver, a
//              locked machine and presentation mode. Session-global, so it
//              cannot be qualified by monitor, and it is not a substitute for
//              the geometry check on a multi-monitor desk.
//
// **Both an event and a poll drive it.** `EVENT_SYSTEM_FOREGROUND` catches
// alt-tabbing into a game in milliseconds, but no event fires when an app
// enters fullscreen *in place* — a browser going F11 does not change the
// foreground window, and `SHQueryUserNotificationState` is documented as
// sending no notification when a full-screen application starts or stops. So
// the poll is not a fallback for a missing hook; it is the only thing that sees
// half the transitions at all.
//
// One thing the research recommended and this does not do: **debounce the
// re-show** by 250–500 ms against transient geometry mid-transition. Hiding is
// immediate and revealing is not debounced, because the case that motivated it
// — the pill flashing back on mid-alt-tab — is already answered by treating a
// null foreground as unknown, and because the probe cadence would turn a 300 ms
// debounce into up to a second of nub-less desktop after a game closes. If a
// blink is ever seen in practice, this is where it goes.

use crate::pill::monitor::{MonitorId, Rect};
use std::time::{Duration, Instant};

/// How often the watcher re-probes with nothing having happened. One wakeup in
/// twenty of the app loop's own 50 ms cadence, and about six user32/shell32
/// calls when it fires — a passive overlay does not need to react faster than
/// this to a transition no event reports.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Slack per edge, in physical pixels, for the invisible resize borders
/// `GetWindowRect` is documented to include — and for the apps that overshoot
/// the monitor by a pixel. The test is *covers*, not *equals*, for the same
/// reason.
pub const EDGE_TOLERANCE: i32 = 2;

/// The foreground window, as the guards need to see it. Plain data, so every
/// rule below is a fact about a struct rather than about a desktop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Foreground {
    /// The monitor holding it — the one the share check compares against the
    /// pill's home.
    pub monitor: MonitorId,
    /// The window's own rect.
    pub rect: Rect,
    /// That monitor's `rcMonitor` — the **full** rect, never `rcWork`. Against
    /// the work area every maximized window reads as fullscreen.
    pub monitor_rect: Rect,
    /// Maximized. Explicitly not fullscreen, and the guard that matters: with
    /// an autohidden taskbar `rcWork == rcMonitor`, so a plain maximized window
    /// covers the monitor exactly.
    pub zoomed: bool,
    pub visible: bool,
    /// The shell's own furniture — the desktop, `Progman`, `WorkerW` (the
    /// wallpaper host, which is monitor-sized and does reach the foreground
    /// transiently), the taskbar — or one of our own windows. Never a reason to
    /// hide.
    pub shell: bool,
}

/// One sample of the world. `foreground` is `None` for the cases the docs call
/// out as *unknown* rather than *no*: a null foreground window (which happens
/// on every alt-tab, "when a window is losing activation"), a window on no
/// monitor, a rect that could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Probe {
    pub foreground: Option<Foreground>,
    /// `SHQueryUserNotificationState` said the session is blocking UI —
    /// exclusive-mode D3D, a full-screen app by the shell's own reckoning, a
    /// screensaver, a locked machine, or presentation mode.
    pub session_blocked: bool,
}

/// Whether the pill should be suppressed, or `None` for "unknown — keep
/// whatever was decided last".
///
/// Unknown is a real answer here, not a shrug. Treating a null foreground as
/// "not fullscreen" would flash the pill back on over the game during every
/// alt-tab, which is the failure this whole feature exists to avoid.
fn suppresses(probe: &Probe, home: Option<MonitorId>) -> Option<bool> {
    // Session-global and unqualifiable by monitor: a screensaver or a locked
    // machine is not "on one screen", and exclusive fullscreen historically
    // blanks the others anyway.
    if probe.session_blocked {
        return Some(true);
    }
    let fg = probe.foreground?;
    if fg.shell || !fg.visible || fg.zoomed {
        return Some(false);
    }
    // A game on the *other* screen is not a reason to take the pill off this
    // one — that would be a regression for dictating while something plays
    // fullscreen elsewhere.
    if Some(fg.monitor) != home {
        return Some(false);
    }
    Some(covers(fg.rect, fg.monitor_rect))
}

/// Whether `window` covers `monitor`, within [`EDGE_TOLERANCE`] per edge.
fn covers(window: Rect, monitor: Rect) -> bool {
    let t = EDGE_TOLERANCE;
    window.left <= monitor.left + t
        && window.top <= monitor.top + t
        && window.right >= monitor.right - t
        && window.bottom >= monitor.bottom - t
}

/// The watcher's whole state: the last answer, and when it last looked.
#[derive(Default)]
pub struct Watcher {
    suppressed: bool,
    /// `None` before the first probe — and after [`Watcher::rearm`], which is
    /// what makes residency coming back on look again immediately rather than
    /// up to a second later.
    last: Option<Instant>,
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current answer, for the adapter's presence composition. The pill's
    /// presence is decided by three facts at once — residency, this, and hover
    /// — so the watcher states its own rather than pushing it.
    pub fn suppressed(&self) -> bool {
        self.suppressed
    }

    /// Look again on the very next poll rather than up to a second later.
    /// Residency going off or coming back takes this path: while it is off
    /// there is no hook and no probe, so whatever was last decided is stale by
    /// construction.
    ///
    /// It does **not** clear the answer, for the same reason a null foreground
    /// doesn't. Clearing would show the nub over a fullscreen app for the one
    /// probe that then says "unknown" — and erring towards hidden is the whole
    /// disposition of this module.
    pub fn rearm(&mut self) {
        self.last = None;
    }

    /// Sample, if this is a loop that should. Returns the new answer only when
    /// it *changed*; almost every call returns `None`, and most do not probe at
    /// all.
    ///
    /// `probe` is a closure rather than a value because taking it is the
    /// expensive part — six user32/shell32 calls that must not run on a loop
    /// that has no reason to look.
    pub fn poll<F>(
        &mut self,
        now: Instant,
        foreground_changed: bool,
        home: Option<MonitorId>,
        probe: F,
    ) -> Option<bool>
    where
        F: FnOnce() -> Probe,
    {
        let due = self
            .last
            .is_none_or(|t| now.saturating_duration_since(t) >= POLL_INTERVAL);
        if !due && !foreground_changed {
            return None;
        }
        // Stamped before the probe, so an unknown answer costs a look rather
        // than starting a retry loop: the foreground change that follows an
        // alt-tab brings the next probe along in milliseconds anyway, and
        // nothing is on screen wrongly in the meantime — the prior answer
        // stands.
        self.last = Some(now);
        let suppressed = suppresses(&probe(), home)?;
        if suppressed == self.suppressed {
            return None;
        }
        self.suppressed = suppressed;
        Some(suppressed)
    }
}

/// The Win32 half: taking the probe, and hearing about a foreground change.
#[cfg(windows)]
pub use win::{foreground_changed, probe, ForegroundHook};

#[cfg(not(windows))]
pub fn probe() -> Probe {
    Probe::default()
}

#[cfg(not(windows))]
pub fn foreground_changed() -> bool {
    false
}

/// Off Windows there is nothing to hook — the probe is empty, so the pill is
/// never suppressed.
#[cfg(not(windows))]
pub struct ForegroundHook;

#[cfg(not(windows))]
impl ForegroundHook {
    pub fn install() -> Option<Self> {
        None
    }
}

#[cfg(windows)]
mod win {
    use super::{Foreground, Probe};
    use crate::pill::monitor::{MonitorId, Rect};
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONULL,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE, QUNS_NOT_PRESENT,
        QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowRect,
        GetWindowThreadProcessId, IsWindowVisible, IsZoomed, EVENT_SYSTEM_FOREGROUND,
        WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    };

    /// Set by the WinEvent callback, drained by the app loop. An atomic rather
    /// than a channel because there is nothing to carry: the callback's whole
    /// job is to say "look again", and the probe that follows reads the
    /// foreground for itself.
    static FOREGROUND_CHANGED: AtomicBool = AtomicBool::new(false);

    /// Whether a foreground change has been seen since the last time this was
    /// asked. Always false with no hook installed, which is what makes
    /// residency-off free: no hook, no callbacks, and the poll never fires.
    pub fn foreground_changed() -> bool {
        FOREGROUND_CHANGED.swap(false, Ordering::Relaxed)
    }

    /// One sample of the world, for [`super::Watcher::poll`].
    pub fn probe() -> Probe {
        Probe {
            session_blocked: session_blocks_ui(),
            foreground: unsafe { foreground() },
        }
    }

    /// The shell's own "do not disturb". A failed call is *not* a reason to
    /// hide — losing the pill because shell32 was busy would be unrecoverable
    /// without a dictation.
    fn session_blocks_ui() -> bool {
        match unsafe { SHQueryUserNotificationState() } {
            Ok(state) => blocks_ui(state),
            Err(e) => {
                tracing::debug!(error = %e, "user notification state unavailable");
                false
            }
        }
    }

    /// Which notification states mean *nothing may be on screen anywhere*.
    ///
    /// This signal is session-global — it cannot be qualified by monitor — so
    /// only states that are themselves global belong here: exclusive-mode D3D
    /// (which historically blanks the other outputs anyway), a screensaver, a
    /// locked machine, presentation mode. Which is also exactly what this
    /// signal was asked for: legacy DX exclusive, screensaver, lock screen.
    ///
    /// **`QUNS_BUSY` is deliberately not one of them.** It is the shell's own
    /// "a full-screen application is running", and the reported behaviour is
    /// that borderless and browser-F11 fullscreen surface as exactly that —
    /// the common case, with no idea which monitor is covered. Honouring it
    /// would take the pill off monitor 1 for a game on monitor 2, which is the
    /// one thing the share check exists to prevent. Geometry already catches
    /// that case, on the right monitor.
    fn blocks_ui(state: QUERY_USER_NOTIFICATION_STATE) -> bool {
        matches!(
            state,
            QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_NOT_PRESENT | QUNS_PRESENTATION_MODE
        )
    }

    /// The foreground window as the guards need it, or `None` for the states
    /// the docs call unknown: no foreground window ("when a window is losing
    /// activation"), a window on no monitor, a rect that could not be read.
    unsafe fn foreground() -> Option<Foreground> {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
        if monitor.is_invalid() {
            return None;
        }
        let monitor_rect = monitor_rect(monitor)?;
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some(Foreground {
            monitor: monitor.0 as MonitorId,
            rect: from_win(rect),
            monitor_rect,
            zoomed: IsZoomed(hwnd).as_bool(),
            visible: IsWindowVisible(hwnd).as_bool(),
            shell: is_shell(hwnd),
        })
    }

    /// `rcMonitor` — the full monitor rect. Deliberately not `rcWork`: against
    /// the work area every maximized window covers its monitor.
    unsafe fn monitor_rect(monitor: HMONITOR) -> Option<Rect> {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info)
            .as_bool()
            .then(|| from_win(info.rcMonitor))
    }

    /// Whether this window is the shell's furniture or our own. `WorkerW` is
    /// the wallpaper host and is monitor-sized, so it is a guaranteed false
    /// positive if it ever lands in the foreground.
    ///
    /// The process check covers both our windows at once: the pill carries
    /// `WS_EX_NOACTIVATE` and should never be foreground, but the settings
    /// subprocess genuinely takes focus — and it is a *different* PID, so it is
    /// caught by geometry and `IsZoomed` rather than here.
    unsafe fn is_shell(hwnd: HWND) -> bool {
        if hwnd == GetShellWindow() || hwnd == GetDesktopWindow() {
            return true;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return true;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        if n <= 0 {
            return false;
        }
        let class = String::from_utf16_lossy(&buf[..n as usize]);
        matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd")
    }

    fn from_win(r: RECT) -> Rect {
        Rect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }

    /// The `EVENT_SYSTEM_FOREGROUND` hook, alive exactly as long as the pill is
    /// resident. Out-of-context, so the callback runs on the thread that
    /// registered it — which must be, and is, the one with the message loop.
    pub struct ForegroundHook(HWINEVENTHOOK);

    impl ForegroundHook {
        /// Install it. Must be called from the winit event-loop thread: the
        /// docs require the registering thread to have a message loop, and
        /// out-of-context events are delivered on that same thread.
        ///
        /// `None` on failure, which costs only reaction speed — the 1 s poll
        /// still sees every transition.
        pub fn install() -> Option<Self> {
            let hook = unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(on_foreground),
                    // Every process and every thread but our own: SKIPOWNPROCESS
                    // means opening settings is not a foreground change worth
                    // waking up for.
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if hook.is_invalid() {
                tracing::warn!("foreground hook unavailable; the pill polls only");
                return None;
            }
            // Whatever the flag was left holding from a previous installation
            // is not news. The first poll probes unconditionally anyway.
            FOREGROUND_CHANGED.store(false, Ordering::Relaxed);
            tracing::debug!("foreground hook installed");
            Some(Self(hook))
        }
    }

    impl Drop for ForegroundHook {
        fn drop(&mut self) {
            let _ = unsafe { UnhookWinEvent(self.0) };
        }
    }

    /// Runs re-entrantly inside winit's message pump, so it does exactly one
    /// thing: raise the flag. Every Win32 query happens later, on the app
    /// loop's own terms.
    unsafe extern "system" fn on_foreground(
        _hook: HWINEVENTHOOK,
        _event: u32,
        _hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        FOREGROUND_CHANGED.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::UI::Shell::{
            QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY, QUNS_QUIET_TIME,
        };

        /// The states this signal answers for, and the ones it must not.
        ///
        /// `QUNS_BUSY` is the load-bearing exclusion: it is session-global, and
        /// it is what borderless and F11 fullscreen are reported to surface as
        /// — so honouring it would hide the pill on one monitor for a game on
        /// another, which geometry already handles on the right one.
        #[test]
        fn only_the_session_wide_states_block_the_pill_everywhere() {
            for state in [
                QUNS_RUNNING_D3D_FULL_SCREEN,
                QUNS_NOT_PRESENT,
                QUNS_PRESENTATION_MODE,
            ] {
                assert!(blocks_ui(state), "{state:?} should block");
            }
            for state in [
                QUNS_BUSY,
                // A foregrounded Store app is not necessarily fullscreen, and
                // quiet time is about balloon spam rather than occlusion.
                QUNS_APP,
                QUNS_QUIET_TIME,
                QUNS_ACCEPTS_NOTIFICATIONS,
            ] {
                assert!(!blocks_ui(state), "{state:?} should not block");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_millis(ms)
    }

    const LAPTOP: MonitorId = 1;
    const EXTERNAL: MonitorId = 2;

    fn screen(id: MonitorId) -> Rect {
        // Side by side, so the external's rect is nowhere near the laptop's —
        // a covering window on one cannot be mistaken for one on the other.
        let left = if id == LAPTOP { 0 } else { 1920 };
        Rect {
            left,
            top: 0,
            right: left + 1920,
            bottom: 1080,
        }
    }

    /// A borderless-windowed game on `id`: its rect is its monitor's, exactly.
    fn fullscreen_on(id: MonitorId) -> Probe {
        Probe {
            foreground: Some(Foreground {
                monitor: id,
                rect: screen(id),
                monitor_rect: screen(id),
                zoomed: false,
                visible: true,
                shell: false,
            }),
            session_blocked: false,
        }
    }

    /// An ordinary window on `id`, covering most of it but not all.
    fn windowed_on(id: MonitorId) -> Probe {
        let mut probe = fullscreen_on(id);
        let fg = probe.foreground.as_mut().unwrap();
        fg.rect.top += 40;
        probe
    }

    /// What this probe decides, with the pill at home on the laptop. `None` is
    /// the unknown that keeps whatever was decided last.
    fn verdict(probe: Probe) -> Option<bool> {
        suppresses(&probe, Some(LAPTOP))
    }

    /// The load-bearing case: a borderless game on the pill's own monitor.
    #[test]
    fn a_fullscreen_window_on_the_home_monitor_suppresses_the_pill() {
        assert_eq!(verdict(fullscreen_on(LAPTOP)), Some(true));
    }

    /// And the regression that guard exists to prevent: a game on the other
    /// screen must not take away a pill the user can still see.
    #[test]
    fn a_fullscreen_window_on_another_monitor_leaves_the_pill_alone() {
        assert_eq!(verdict(fullscreen_on(EXTERNAL)), Some(false));
        // And it gives the pill back: a game on the home monitor closing while
        // one on the other screen takes focus is a reveal, not a hold.
        let mut w = Watcher::new();
        assert_eq!(
            w.poll(t(0), false, Some(LAPTOP), || fullscreen_on(LAPTOP)),
            Some(true)
        );
        assert_eq!(
            w.poll(t(2_000), false, Some(LAPTOP), || fullscreen_on(EXTERNAL)),
            Some(false)
        );
    }

    /// With no home monitor derived yet there is nothing to share, so nothing
    /// is suppressed by geometry.
    #[test]
    fn with_no_home_monitor_geometry_suppresses_nothing() {
        assert_eq!(suppresses(&fullscreen_on(LAPTOP), None), Some(false));
    }

    /// Every guard, as one table. Each of these covers its monitor exactly and
    /// is still not a fullscreen app.
    #[test]
    fn the_guards_reject_windows_that_merely_cover_the_monitor() {
        let covering = |mutate: fn(&mut Foreground)| {
            let mut probe = fullscreen_on(LAPTOP);
            mutate(probe.foreground.as_mut().unwrap());
            probe
        };
        let cases = [
            // Maximized with an autohidden taskbar: rcWork == rcMonitor, so the
            // geometry matches and IsZoomed is the only thing that says no.
            ("maximized", covering(|fg| fg.zoomed = true)),
            // The wallpaper host is monitor-sized and does reach the foreground.
            ("shell window", covering(|fg| fg.shell = true)),
            ("invisible", covering(|fg| fg.visible = false)),
        ];
        for (name, probe) in cases {
            assert_eq!(verdict(probe), Some(false), "{name}");
        }
    }

    /// A null foreground — every alt-tab passes through one — is unknown, not
    /// "nothing is fullscreen". Answering it would flash the pill back over the
    /// game mid-switch.
    #[test]
    fn a_null_foreground_keeps_the_prior_answer() {
        let mut w = Watcher::new();
        w.poll(t(0), false, Some(LAPTOP), || fullscreen_on(LAPTOP));
        assert!(w.suppressed());
        assert_eq!(w.poll(t(2_000), false, Some(LAPTOP), Probe::default), None);
        assert!(w.suppressed(), "an unknown foreground gave the pill back");
    }

    /// The second signal: session-global, so it hides the pill wherever it is
    /// and whatever the foreground says — a locked machine and a screensaver
    /// are not "on one monitor".
    #[test]
    fn the_shells_do_not_disturb_state_suppresses_on_its_own() {
        let probe = Probe {
            foreground: Some(windowed_on(EXTERNAL).foreground.unwrap()),
            session_blocked: true,
        };
        assert_eq!(verdict(probe), Some(true));
        // Including with no foreground window at all — the screensaver case.
        assert_eq!(
            verdict(Probe {
                session_blocked: true,
                ..Probe::default()
            }),
            Some(true)
        );
    }

    /// Covers, not equals: the invisible resize borders `GetWindowRect`
    /// includes, and the apps that overshoot by a pixel, both have to pass.
    #[test]
    fn the_cover_test_allows_a_little_slack_at_every_edge() {
        let mon = screen(LAPTOP);
        let inset = |by: i32| Rect {
            left: mon.left + by,
            top: mon.top + by,
            right: mon.right - by,
            bottom: mon.bottom - by,
        };
        assert!(covers(inset(EDGE_TOLERANCE), mon));
        assert!(!covers(inset(EDGE_TOLERANCE + 1), mon));
        // Overshoot is fullscreen too.
        assert!(covers(inset(-10), mon));
        // And an ordinary window is not.
        assert_eq!(verdict(windowed_on(LAPTOP)), Some(false));
    }

    /// The whole cadence: a probe costs six Win32 calls, so it happens once a
    /// second — or the moment the foreground changed, which is what makes
    /// alt-tabbing into a game feel instant.
    #[test]
    fn the_watcher_probes_once_a_second_or_when_the_foreground_changed() {
        let probes = Cell::new(0);
        let take = || {
            probes.set(probes.get() + 1);
            windowed_on(LAPTOP)
        };
        let mut w = Watcher::new();
        // The first poll always looks: there is no prior answer to keep.
        w.poll(t(0), false, Some(LAPTOP), take);
        assert_eq!(probes.get(), 1);
        // The app loop wakes twenty times a second and this is not its business.
        for ms in [50, 100, 500, 999] {
            w.poll(t(ms), false, Some(LAPTOP), take);
        }
        assert_eq!(probes.get(), 1, "the idle loop probed");
        // A foreground change does not wait for the interval.
        w.poll(t(120), true, Some(LAPTOP), take);
        assert_eq!(probes.get(), 2);
        // And the interval runs from the last probe, event or poll alike.
        w.poll(t(1_119), false, Some(LAPTOP), take);
        assert_eq!(probes.get(), 2);
        w.poll(t(1_120), false, Some(LAPTOP), take);
        assert_eq!(probes.get(), 3);
    }

    /// Only changes are reported: the adapter turns each answer into a presence
    /// change, and a pill re-suppressed once a second forever would be a mode
    /// change once a second forever.
    #[test]
    fn an_unchanged_answer_is_not_reported() {
        let mut w = Watcher::new();
        assert_eq!(
            w.poll(t(0), false, Some(LAPTOP), || fullscreen_on(LAPTOP)),
            Some(true)
        );
        for ms in [1_000, 2_000, 3_000] {
            assert_eq!(
                w.poll(t(ms), false, Some(LAPTOP), || fullscreen_on(LAPTOP)),
                None
            );
        }
        assert_eq!(
            w.poll(t(4_000), false, Some(LAPTOP), || windowed_on(LAPTOP)),
            Some(false)
        );
    }

    /// Residency toggled off and back on: whatever was decided while there was
    /// no hook and no probe is stale, so the next poll looks immediately rather
    /// than up to a second later.
    #[test]
    fn rearming_looks_again_at_once_without_giving_the_pill_back_first() {
        let mut w = Watcher::new();
        w.poll(t(0), false, Some(LAPTOP), || fullscreen_on(LAPTOP));
        assert!(w.suppressed());
        w.rearm();
        // The answer is kept, not cleared: an unknown probe here would
        // otherwise put the nub back over the game for a second.
        assert!(w.suppressed());
        // That first probe happens well inside the interval, which is the point
        // of rearming — and it lands on an unknown, which changes nothing.
        assert_eq!(w.poll(t(10), false, Some(LAPTOP), Probe::default), None);
        assert!(w.suppressed());
        // The foreground change that resolves the alt-tab brings the next one.
        assert_eq!(
            w.poll(t(20), true, Some(LAPTOP), || windowed_on(LAPTOP)),
            Some(false)
        );
    }
}
