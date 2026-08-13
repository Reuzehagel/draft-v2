// The pill's home monitor — the one screen it lives on, and the policy that
// picks it.
//
// There is exactly one home monitor and exactly one pill. Not one per monitor:
// N layered windows each pay the idle cost residency exists to avoid, the
// expanded pill becomes ambiguous, and the home-monitor concept — which the
// fullscreen hide check (#45) and the hover hit test (#19) both read — dissolves.
//
// Two halves, split the way `pill::core` is:
//
//   [`Home`] — pure. Policy plus a snapshot of the displays plus the cheap
//              per-poll signals in, a [`HomeMonitor`] out when it moved. It owns
//              the latch and the cursor dwell, which is what makes both
//              assertable without a desktop to drag a mouse across.
//   `win`    — the Win32 half: enumerate the monitors, and read the two signals.
//
// The rules the pure half encodes, from #22:
//
// - Re-derive **only while the pill is idle**. It is latched at session start
//   and while expanded, so the pill cannot skate to another monitor mid-sentence
//   and cannot slide out from under the hand about to click it.
// - `focused` moves immediately: a focus change is deliberate and its monitor is
//   already settled.
// - `cursor` requires a dwell, because dragging a mouse across a bezel and back
//   would otherwise teleport the pill twice — and the pill is a
//   peripheral-vision object, where motion is the thing you notice.
// - `WM_DISPLAYCHANGE` re-derives unconditionally and breaks the latch. The
//   alternative is a pill positioned into a coordinate space that no longer
//   exists. Startup and a policy change take the same path.
// - `primary` is the universal fallback, which is also a value the user can just
//   select outright.

use std::time::{Duration, Instant};

/// How long the cursor has to sit on a new monitor before the pill follows it.
///
/// Only `cursor` has one: a focus change is a deliberate act with a settled
/// monitor, where a cursor is mid-flight most of the time it is moving at all.
pub const CURSOR_DWELL: Duration = Duration::from_millis(300);

/// An `HMONITOR`, as plain data. Kept as an integer rather than the Win32 type
/// so the whole of [`Home`] is testable off Windows and off a desktop.
pub type MonitorId = isize;

/// A screen rect in physical pixels — always `rcWork` here, never `rcMonitor`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// One connected monitor, as the policy needs to see it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// `rcWork` — the monitor minus its taskbar and any other appbar.
    pub work: Rect,
    /// Effective DPI; 96 is 100%.
    pub dpi: u32,
    pub primary: bool,
    /// `QueryDisplayConfig`'s EDID-derived `monitorDevicePath`. This, and
    /// explicitly not `\\.\DISPLAY1` — a GDI adapter slot is reassigned on
    /// replug or reorder, so a pinned slot is a setting that silently rots.
    ///
    /// `None` when the join to the display-config path failed; such a monitor
    /// simply cannot be pinned.
    pub device_path: Option<String>,
    /// `monitorFriendlyDeviceName` ("DELL U2720Q") — what the settings picker
    /// shows for a pinned monitor.
    pub friendly_name: Option<String>,
}

/// The connected monitors, as of the last enumeration. Refreshed on display
/// change, never per poll: `EnumDisplayMonitors` plus `QueryDisplayConfig` is
/// far too much work for a 50 ms loop, and the per-poll signals are only an
/// `HMONITOR` to look up in here.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Displays(Vec<MonitorInfo>);

impl Displays {
    pub fn new(monitors: Vec<MonitorInfo>) -> Self {
        Self(monitors)
    }

    pub fn all(&self) -> &[MonitorInfo] {
        &self.0
    }

    fn by_id(&self, id: MonitorId) -> Option<&MonitorInfo> {
        self.0.iter().find(|m| m.id == id)
    }

    fn by_path(&self, path: &str) -> Option<&MonitorInfo> {
        self.0
            .iter()
            .find(|m| m.device_path.as_deref() == Some(path))
    }

    /// The primary monitor, or — if Windows named none, which it does while the
    /// topology is mid-change — whatever is first. The universal fallback has to
    /// answer with *something* as long as any monitor exists at all.
    fn primary(&self) -> Option<&MonitorInfo> {
        self.0.iter().find(|m| m.primary).or_else(|| self.0.first())
    }
}

/// The pill's home monitor: everything placement, the hide check and the hit
/// test read. Copy, because it is a value that gets handed around, not a
/// handle that gets held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HomeMonitor {
    pub id: MonitorId,
    pub work: Rect,
    pub dpi: u32,
}

impl HomeMonitor {
    /// The scale factor the pill renders at. A *property of the home monitor* —
    /// which is the whole point: captured once at window creation it is already
    /// wrong on a mixed-DPI desk, and routinely wrong once the pill can move.
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Where the pill window sits on this monitor, in physical pixels.
    ///
    /// Anchored to `rcWork`, so the visual gap is identical on every monitor,
    /// the pill cannot collide with a taskbar, and a left- or top-docked taskbar
    /// is handled for free by the work rect's own origin.
    pub fn placement(&self) -> Rect {
        let scale = self.scale();
        let w = (crate::pill::geom::ENVELOPE_W as f32 * scale).round() as i32;
        let h = (crate::pill::geom::ENVELOPE_H as f32 * scale).round() as i32;
        let margin = (crate::pill::PILL_BOTTOM_MARGIN as f32 * scale).round() as i32;
        let left = self.work.left + (self.work.width() - w) / 2;
        let top = self.work.bottom - h - margin;
        Rect {
            left,
            top,
            right: left + w,
            bottom: top + h,
        }
    }
}

impl From<&MonitorInfo> for HomeMonitor {
    fn from(m: &MonitorInfo) -> Self {
        Self {
            id: m.id,
            work: m.work,
            dpi: m.dpi,
        }
    }
}

/// How the home monitor is derived. `focused` is the default because Draft
/// pastes into the foreground window: the pill is feedback about text that is
/// going to land *there*, so a pill on another monitor is feedback in the wrong
/// place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// The monitor holding the foreground window.
    Focused,
    /// The monitor holding the cursor, after [`CURSOR_DWELL`].
    Cursor,
    /// The primary monitor — and the fallback every other policy lands on.
    Primary,
    /// The monitor named by the pinned device path.
    Pinned,
}

impl Policy {
    /// Whether the app loop has to sample the foreground window for this policy.
    /// `primary` and `pinned` need nothing per poll at all.
    pub fn samples_foreground(self) -> bool {
        self == Policy::Focused
    }

    pub fn samples_cursor(self) -> bool {
        self == Policy::Cursor
    }
}

/// The cheap per-poll signals. `None` means either "not sampled, this policy
/// doesn't want it" or "Windows named no monitor" — which resolve the same way,
/// through the fallback.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Signals {
    pub focused: Option<MonitorId>,
    pub cursor: Option<MonitorId>,
}

/// Why [`Home::update`] is being called, which is what decides whether the latch
/// applies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    /// The app loop's ordinary sample. Honoured only while the pill is idle.
    Poll { idle: bool },
    /// Re-derive now, latch or no latch: `WM_DISPLAYCHANGE`, a DPI change,
    /// startup, and a policy change all arrive here.
    Rederive,
}

/// The home monitor and the policy deriving it — the pure half.
pub struct Home {
    policy: Policy,
    pinned_path: Option<String>,
    current: Option<HomeMonitor>,
    /// The monitor the cursor is currently accumulating dwell on, and when it
    /// arrived. Only `cursor` ever sets it.
    dwell: Option<(MonitorId, Instant)>,
}

impl Home {
    pub fn new(policy: Policy, pinned_path: Option<String>) -> Self {
        Self {
            policy,
            pinned_path,
            current: None,
            dwell: None,
        }
    }

    /// Adopt a policy from a config reload. Does not itself derive — the caller
    /// follows with a [`Trigger::Rederive`], on the same path startup takes.
    pub fn configure(&mut self, policy: Policy, pinned_path: Option<String>) {
        self.policy = policy;
        self.pinned_path = pinned_path;
        self.dwell = None;
    }

    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// The home monitor, or `None` before the first derivation.
    ///
    /// This is the readable state the rest of the pill is meant to consult:
    /// the fullscreen hide check (#45) has to require the fullscreen window to
    /// share it. That check doesn't exist yet, which is the only reason nothing
    /// outside the tests calls this — the rule is here, and asserted, ahead of
    /// it. (The hover hit test reads the same monitor's placement through the
    /// pill window, which is handed one of these at every move.)
    #[allow(dead_code)]
    pub fn current(&self) -> Option<HomeMonitor> {
        self.current
    }

    /// Re-derive the home monitor. Returns it only when it *changed* — a
    /// different monitor, or the same one whose work area or DPI moved under it.
    /// `None` is "nothing to do", which is what almost every poll returns.
    pub fn update(
        &mut self,
        trigger: Trigger,
        displays: &Displays,
        signals: Signals,
        now: Instant,
    ) -> Option<HomeMonitor> {
        let forced = trigger == Trigger::Rederive;
        if !forced {
            // Latched: a session is running, or the pill is expanded under a
            // hand. Drop any dwell in progress rather than banking it — when the
            // pill comes back to idle the cursor gets to make its case afresh.
            if trigger != (Trigger::Poll { idle: true }) {
                self.dwell = None;
                return None;
            }
        }

        // No monitors at all (mid-topology-change): keep whatever we have. A
        // home monitor that no longer exists is still better than none, and the
        // `WM_DISPLAYCHANGE` that follows will settle it.
        let candidate = self.resolve(displays, signals)?;

        let same_monitor = self.current.map(|c| c.id) == Some(candidate.id);
        if !forced && !same_monitor && self.policy == Policy::Cursor {
            match self.dwell {
                // Still the same candidate — has it been there long enough?
                Some((id, since))
                    if id == candidate.id
                        && now.saturating_duration_since(since) >= CURSOR_DWELL => {}
                Some((id, _)) if id == candidate.id => return None,
                // A new candidate (or the first): start its clock.
                _ => {
                    self.dwell = Some((candidate.id, now));
                    return None;
                }
            }
        }
        // Either we are committing, or the cursor came back to the monitor it
        // started on — the bezel round trip, where the dwell is abandoned.
        self.dwell = None;

        if self.current == Some(candidate) {
            return None;
        }
        self.current = Some(candidate);
        Some(candidate)
    }

    /// Re-read the *current* home monitor's work area and DPI from a fresh
    /// snapshot, without re-running the policy. Returns it only when something
    /// about it moved.
    ///
    /// This is what a DPI change is: the monitor the pill is already on
    /// rescaled underneath it. Deliberately not a re-derivation — the latch is
    /// broken by `WM_DISPLAYCHANGE` and nothing else, and a pill that hopped
    /// screens because the user dragged a scaling slider mid-sentence would be
    /// exactly the skate the latch exists to prevent.
    ///
    /// A home monitor that has vanished from the snapshot is left alone: the
    /// `WM_DISPLAYCHANGE` that took it away is what answers for that.
    pub fn refresh(&mut self, displays: &Displays) -> Option<HomeMonitor> {
        let current = self.current?;
        let fresh = HomeMonitor::from(displays.by_id(current.id)?);
        if fresh == current {
            return None;
        }
        self.current = Some(fresh);
        Some(fresh)
    }

    /// The monitor this policy names right now, falling back to primary — which
    /// covers a foreground window Windows won't place, a cursor off every
    /// monitor, and a pinned path that no longer resolves.
    fn resolve(&self, displays: &Displays, signals: Signals) -> Option<HomeMonitor> {
        let named = match self.policy {
            Policy::Focused => signals.focused.and_then(|id| displays.by_id(id)),
            Policy::Cursor => signals.cursor.and_then(|id| displays.by_id(id)),
            Policy::Primary => None,
            Policy::Pinned => self
                .pinned_path
                .as_deref()
                .and_then(|path| displays.by_path(path)),
        };
        named.or_else(|| displays.primary()).map(HomeMonitor::from)
    }
}

/// The Win32 half: what the pure core cannot know.
#[cfg(windows)]
pub use win::{cursor_monitor, cursor_pos, enumerate, foreground_monitor};

/// Sample only the signals this policy actually reads. `primary` and `pinned`
/// cost nothing per poll, which is most of what makes the poll affordable.
pub fn sample(policy: Policy) -> Signals {
    Signals {
        focused: policy
            .samples_foreground()
            .then(foreground_monitor)
            .flatten(),
        cursor: policy.samples_cursor().then(cursor_monitor).flatten(),
    }
}

#[cfg(not(windows))]
pub fn enumerate() -> Displays {
    Displays::default()
}

#[cfg(not(windows))]
pub fn foreground_monitor() -> Option<MonitorId> {
    None
}

#[cfg(not(windows))]
pub fn cursor_monitor() -> Option<MonitorId> {
    None
}

#[cfg(not(windows))]
pub fn cursor_pos() -> Option<(i32, i32)> {
    None
}

#[cfg(windows)]
mod win {
    use super::{Displays, MonitorId, MonitorInfo, Rect};
    use std::collections::HashMap;
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::{BOOL, LPARAM, POINT, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
        MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONULL,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetForegroundWindow, MONITORINFOF_PRIMARY,
    };

    /// `ERROR_SUCCESS`. The display-config calls return a bare `WIN32_ERROR`
    /// rather than a `Result`, so this is the check.
    const OK: u32 = 0;

    /// What a monitor reports when its DPI can't be read. 96 is 100%, which is
    /// the right guess and the one every non-per-monitor-aware path assumes.
    const DEFAULT_DPI: u32 = 96;

    /// Every connected monitor, joined to its EDID-derived device path.
    pub fn enumerate() -> Displays {
        let paths = device_paths();
        let mut monitors = Vec::new();
        for hmonitor in enum_monitors() {
            let Some((work, primary, gdi_device)) = monitor_info(hmonitor) else {
                continue;
            };
            let (device_path, friendly_name) = paths
                .get(&gdi_device)
                .cloned()
                .map_or((None, None), |(p, f)| (Some(p), f));
            monitors.push(MonitorInfo {
                id: hmonitor.0 as MonitorId,
                work,
                dpi: monitor_dpi(hmonitor),
                primary,
                device_path,
                friendly_name,
            });
        }
        Displays::new(monitors)
    }

    /// The monitor holding the foreground window. `None` when there is no
    /// foreground window, or it does not intersect any monitor — both of which
    /// the caller resolves through the fallback rather than guessing.
    pub fn foreground_monitor() -> Option<MonitorId> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }
        let m = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
        (!m.is_invalid()).then_some(m.0 as MonitorId)
    }

    pub fn cursor_monitor() -> Option<MonitorId> {
        let pt = cursor_point()?;
        let m = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONULL) };
        (!m.is_invalid()).then_some(m.0 as MonitorId)
    }

    /// The cursor, in physical virtual-screen pixels — the space the pill's own
    /// placement is in, so the hover test compares the two directly and scales
    /// by nothing.
    pub fn cursor_pos() -> Option<(i32, i32)> {
        cursor_point().map(|pt| (pt.x, pt.y))
    }

    fn cursor_point() -> Option<POINT> {
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt) }.ok()?;
        Some(pt)
    }

    fn enum_monitors() -> Vec<HMONITOR> {
        let mut out: Vec<HMONITOR> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(collect_monitor),
                LPARAM(&mut out as *mut Vec<HMONITOR> as isize),
            );
        }
        out
    }

    unsafe extern "system" fn collect_monitor(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<HMONITOR>);
        out.push(hmonitor);
        TRUE
    }

    /// `rcWork`, the primary flag, and the GDI device name — the last only so it
    /// can be joined to a device path, never stored as the pin.
    fn monitor_info(hmonitor: HMONITOR) -> Option<(Rect, bool, String)> {
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let ok = unsafe {
            GetMonitorInfoW(
                hmonitor,
                &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
            )
        };
        if !ok.as_bool() {
            return None;
        }
        let w = info.monitorInfo.rcWork;
        Some((
            Rect {
                left: w.left,
                top: w.top,
                right: w.right,
                bottom: w.bottom,
            },
            info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            wide_to_string(&info.szDevice),
        ))
    }

    fn monitor_dpi(hmonitor: HMONITOR) -> u32 {
        let (mut x, mut y) = (0u32, 0u32);
        match unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
            // Square pixels in every configuration Windows exposes; x is the
            // one every other DPI API reports.
            Ok(()) if x > 0 => x,
            _ => DEFAULT_DPI,
        }
    }

    /// GDI device name (`\\.\DISPLAY1`) → (EDID device path, friendly name).
    ///
    /// The join is why the source name is read at all: `GetMonitorInfoW` speaks
    /// adapter slots and `QueryDisplayConfig` speaks physical monitors, and this
    /// is the only thing the two have in common.
    fn device_paths() -> HashMap<String, (String, Option<String>)> {
        let mut out = HashMap::new();
        let Some((paths, _modes)) = query_display_config() else {
            return out;
        };
        for path in paths {
            let Some(source) = source_name(&path) else {
                continue;
            };
            let Some((device_path, friendly)) = target_name(&path) else {
                continue;
            };
            out.insert(source, (device_path, friendly));
        }
        out
    }

    fn query_display_config() -> Option<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)>
    {
        // Retried once: the topology can change between sizing the buffers and
        // filling them, which is exactly the ERROR_INSUFFICIENT_BUFFER the docs
        // tell you to expect.
        for _ in 0..2 {
            let (mut n_paths, mut n_modes) = (0u32, 0u32);
            let sized = unsafe {
                GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes)
            };
            if sized.0 != OK {
                return None;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
            let res = unsafe {
                QueryDisplayConfig(
                    QDC_ONLY_ACTIVE_PATHS,
                    &mut n_paths,
                    paths.as_mut_ptr(),
                    &mut n_modes,
                    modes.as_mut_ptr(),
                    None,
                )
            };
            if res.0 == OK {
                paths.truncate(n_paths as usize);
                modes.truncate(n_modes as usize);
                return Some((paths, modes));
            }
        }
        None
    }

    fn source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
        let mut req = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        let code = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
        (code == 0).then(|| wide_to_string(&req.viewGdiDeviceName))
    }

    fn target_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<(String, Option<String>)> {
        let mut req = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            ..Default::default()
        };
        let code = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
        if code != 0 {
            return None;
        }
        let device_path = wide_to_string(&req.monitorDevicePath);
        if device_path.is_empty() {
            return None;
        }
        // A monitor whose EDID carries no name — the picker falls back to the
        // path, which is ugly but at least identifies something.
        let friendly = wide_to_string(&req.monitorFriendlyDeviceName);
        Some((device_path, (!friendly.is_empty()).then_some(friendly)))
    }

    /// A fixed-size UTF-16 field, up to its first NUL.
    fn wide_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
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

    const LAPTOP: MonitorId = 1;
    const EXTERNAL: MonitorId = 2;

    /// A 150% laptop panel left of a 100% external — the ordinary Windows
    /// mixed-DPI desk, where every crossing is also a scale change.
    fn desk() -> Displays {
        Displays::new(vec![
            MonitorInfo {
                id: LAPTOP,
                work: Rect {
                    left: 0,
                    top: 0,
                    right: 2560,
                    bottom: 1520,
                },
                dpi: 144,
                primary: true,
                device_path: Some(r"\\?\DISPLAY#LAPTOP#EDID".into()),
                friendly_name: Some("Built-in display".into()),
            },
            MonitorInfo {
                id: EXTERNAL,
                work: Rect {
                    left: 2560,
                    top: 0,
                    right: 6400,
                    bottom: 2112,
                },
                dpi: 96,
                primary: false,
                device_path: Some(r"\\?\DISPLAY#DELL#EDID".into()),
                friendly_name: Some("DELL U2720Q".into()),
            },
        ])
    }

    fn idle() -> Trigger {
        Trigger::Poll { idle: true }
    }

    fn on(id: MonitorId) -> Signals {
        Signals {
            focused: Some(id),
            cursor: Some(id),
        }
    }

    /// The whole point of the policy: four rules, four answers, off the same
    /// desk and the same signals.
    #[test]
    fn each_policy_names_its_own_monitor() {
        let desk = desk();
        let signals = Signals {
            focused: Some(EXTERNAL),
            cursor: Some(EXTERNAL),
        };
        let cases = [
            (Policy::Focused, None, EXTERNAL),
            // `cursor` is the one that has to wait, so it is asserted on its own
            // below; here it is only shown reaching the same monitor.
            (Policy::Primary, None, LAPTOP),
            (
                Policy::Pinned,
                Some(r"\\?\DISPLAY#DELL#EDID".to_string()),
                EXTERNAL,
            ),
            (
                Policy::Pinned,
                Some(r"\\?\DISPLAY#LAPTOP#EDID".to_string()),
                LAPTOP,
            ),
        ];
        for (policy, pin, expected) in cases {
            let mut home = Home::new(policy, pin.clone());
            let got = home.update(Trigger::Rederive, &desk, signals, t(0));
            assert_eq!(
                got.map(|h| h.id),
                Some(expected),
                "policy {policy:?} pin {pin:?}"
            );
        }
    }

    /// The signal the policy doesn't read is not allowed to move the pill —
    /// `focused` must ignore the cursor and `cursor` must ignore focus.
    #[test]
    fn a_policy_reads_only_its_own_signal() {
        let desk = desk();
        let mut focused = Home::new(Policy::Focused, None);
        focused.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        // The cursor wanders to the external; focus stays put.
        let signals = Signals {
            focused: Some(LAPTOP),
            cursor: Some(EXTERNAL),
        };
        assert_eq!(focused.update(idle(), &desk, signals, t(5_000)), None);
        assert_eq!(focused.current().map(|h| h.id), Some(LAPTOP));
    }

    /// No dwell for `focused`: a focus change is deliberate, and its monitor is
    /// already settled by the time we hear about it.
    #[test]
    fn focus_moves_the_pill_on_the_very_next_poll() {
        let desk = desk();
        let mut home = Home::new(Policy::Focused, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        assert_eq!(
            home.update(idle(), &desk, on(EXTERNAL), t(1)).map(|h| h.id),
            Some(EXTERNAL)
        );
    }

    /// Dragging across a bezel and back must not teleport the pill twice. It
    /// never moves at all here: the cursor is back before the dwell is up.
    #[test]
    fn a_bezel_round_trip_inside_the_dwell_never_moves_the_pill() {
        let desk = desk();
        let mut home = Home::new(Policy::Cursor, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        // Across the bezel...
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(50)), None);
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(150)), None);
        // ...and back, well inside the dwell.
        assert_eq!(home.update(idle(), &desk, on(LAPTOP), t(200)), None);
        // The abandoned dwell must not be banked: crossing again starts over,
        // so t(250) is not "300 ms since the first crossing".
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(250)), None);
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(400)), None);
        assert_eq!(home.current().map(|h| h.id), Some(LAPTOP));
    }

    /// The cursor that stays put does move it — after the dwell, not before.
    #[test]
    fn the_cursor_moves_the_pill_once_it_has_settled() {
        let desk = desk();
        let mut home = Home::new(Policy::Cursor, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(1_000)), None);
        let just_short = 1_000 + CURSOR_DWELL.as_millis() as u64 - 1;
        assert_eq!(
            home.update(idle(), &desk, on(EXTERNAL), t(just_short)),
            None
        );
        let settled = 1_000 + CURSOR_DWELL.as_millis() as u64;
        assert_eq!(
            home.update(idle(), &desk, on(EXTERNAL), t(settled))
                .map(|h| h.id),
            Some(EXTERNAL)
        );
        // And having arrived, it stops emitting.
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(9_999)), None);
    }

    /// The latch: while a session runs or the pill is expanded, nothing moves it
    /// — not even a focus change, which is otherwise instant.
    #[test]
    fn the_latch_holds_while_a_session_runs_or_the_pill_is_expanded() {
        let desk = desk();
        let mut home = Home::new(Policy::Focused, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        let latched = Trigger::Poll { idle: false };
        for ms in [10, 200, 5_000] {
            assert_eq!(home.update(latched, &desk, on(EXTERNAL), t(ms)), None);
        }
        assert_eq!(home.current().map(|h| h.id), Some(LAPTOP));
        // Back to idle, and it goes where it was always going to go.
        assert_eq!(
            home.update(idle(), &desk, on(EXTERNAL), t(5_001))
                .map(|h| h.id),
            Some(EXTERNAL)
        );
    }

    /// A dwell may not run down behind the latch either — a session that
    /// outlasts 300 ms would otherwise land the pill somewhere else the instant
    /// it ends.
    #[test]
    fn a_dwell_does_not_accumulate_behind_the_latch() {
        let desk = desk();
        let mut home = Home::new(Policy::Cursor, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(100)), None);
        // A session starts and runs for two seconds with the cursor over there.
        for ms in [150, 1_000, 2_000] {
            assert_eq!(
                home.update(Trigger::Poll { idle: false }, &desk, on(EXTERNAL), t(ms)),
                None
            );
        }
        // The first idle poll after it restarts the clock rather than firing.
        assert_eq!(home.update(idle(), &desk, on(EXTERNAL), t(2_001)), None);
        assert_eq!(
            home.update(
                idle(),
                &desk,
                on(EXTERNAL),
                t(2_001 + CURSOR_DWELL.as_millis() as u64)
            )
            .map(|h| h.id),
            Some(EXTERNAL)
        );
    }

    /// `WM_DISPLAYCHANGE` is the one thing that overrides the latch: the pill
    /// may be mid-sentence, but its coordinate space no longer exists.
    #[test]
    fn a_display_change_breaks_the_latch_and_skips_the_dwell() {
        let desk = desk();
        let mut home = Home::new(Policy::Cursor, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        // Latched, and no dwell served — it moves anyway, immediately.
        assert_eq!(
            home.update(Trigger::Rederive, &desk, on(EXTERNAL), t(1))
                .map(|h| h.id),
            Some(EXTERNAL)
        );
    }

    /// The same monitor with a different work area or DPI is still a move: a
    /// taskbar appearing, or a resolution change, both arrive this way and both
    /// leave the pill in the wrong place until it re-places.
    #[test]
    fn the_same_monitor_resized_under_the_pill_still_reports_a_move() {
        let mut home = Home::new(Policy::Primary, None);
        home.update(Trigger::Rederive, &desk(), Signals::default(), t(0));
        let mut all = desk().all().to_vec();
        all[0].work.bottom = 1400;
        all[0].dpi = 120;
        let shrunk = Displays::new(all);
        let after = home
            .update(Trigger::Rederive, &shrunk, Signals::default(), t(1))
            .expect("a resized home monitor is a move");
        assert_eq!(after.id, LAPTOP);
        assert_eq!(after.work.bottom, 1400);
        assert_eq!(after.dpi, 120);
    }

    /// A pinned monitor that is not connected — unplugged, or moved to another
    /// port, which can change the EDID path. It falls back to primary rather
    /// than leaving the pill nowhere.
    #[test]
    fn an_unresolvable_pin_falls_back_to_primary() {
        let desk = desk();
        let mut home = Home::new(Policy::Pinned, Some(r"\\?\DISPLAY#GONE#EDID".into()));
        assert_eq!(
            home.update(Trigger::Rederive, &desk, Signals::default(), t(0))
                .map(|h| h.id),
            Some(LAPTOP)
        );
        // So does a policy of `pinned` with nothing pinned yet.
        let mut unset = Home::new(Policy::Pinned, None);
        assert_eq!(
            unset
                .update(Trigger::Rederive, &desk, Signals::default(), t(0))
                .map(|h| h.id),
            Some(LAPTOP)
        );
    }

    /// Every policy falls back the same way when its signal names nothing —
    /// a foreground window Windows won't place, a cursor off every monitor.
    #[test]
    fn a_signal_naming_nothing_falls_back_to_primary() {
        let desk = desk();
        for policy in [Policy::Focused, Policy::Cursor] {
            let mut home = Home::new(policy, None);
            assert_eq!(
                home.update(Trigger::Rederive, &desk, Signals::default(), t(0))
                    .map(|h| h.id),
                Some(LAPTOP),
                "{policy:?}"
            );
        }
        // As does a signal naming a monitor that has since gone.
        let mut home = Home::new(Policy::Focused, None);
        assert_eq!(
            home.update(Trigger::Rederive, &desk, on(99), t(0))
                .map(|h| h.id),
            Some(LAPTOP)
        );
    }

    /// Windows names no primary while the topology is mid-change. The universal
    /// fallback still has to answer with something.
    #[test]
    fn with_no_primary_flagged_the_fallback_is_still_a_monitor() {
        let mut all = desk().all().to_vec();
        all[0].primary = false;
        let displays = Displays::new(all);
        let mut home = Home::new(Policy::Primary, None);
        assert_eq!(
            home.update(Trigger::Rederive, &displays, Signals::default(), t(0))
                .map(|h| h.id),
            Some(LAPTOP)
        );
    }

    /// No monitors at all: keep what we have. A stale home monitor beats none,
    /// and the display change that follows settles it.
    #[test]
    fn an_empty_desk_leaves_the_home_monitor_alone() {
        let mut home = Home::new(Policy::Primary, None);
        home.update(Trigger::Rederive, &desk(), Signals::default(), t(0));
        assert_eq!(
            home.update(
                Trigger::Rederive,
                &Displays::default(),
                Signals::default(),
                t(1)
            ),
            None
        );
        assert_eq!(home.current().map(|h| h.id), Some(LAPTOP));
    }

    /// A DPI change rescales the monitor the pill is on. It re-places, and it
    /// does *not* get to run the policy again — the latch belongs to
    /// `WM_DISPLAYCHANGE` alone.
    #[test]
    fn a_dpi_change_rescales_the_home_monitor_without_re_running_the_policy() {
        let desk = desk();
        let mut home = Home::new(Policy::Focused, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        // The laptop panel is set to 200%, and the foreground window has since
        // moved to the external — which must not be allowed to matter.
        let mut all = desk.all().to_vec();
        all[0].dpi = 192;
        all[0].work.bottom = 1140;
        let rescaled = Displays::new(all);
        let after = home
            .refresh(&rescaled)
            .expect("a rescaled monitor re-places");
        assert_eq!(after.id, LAPTOP);
        assert_eq!(after.dpi, 192);
        assert_eq!(after.scale(), 2.0);
        // Idempotent — nothing moved the second time.
        assert_eq!(home.refresh(&rescaled), None);
    }

    /// A home monitor that has gone is not this event's problem: the
    /// `WM_DISPLAYCHANGE` that unplugged it re-derives properly.
    #[test]
    fn refreshing_a_vanished_home_monitor_leaves_it_alone() {
        let desk = desk();
        let mut home = Home::new(Policy::Focused, None);
        home.update(Trigger::Rederive, &desk, on(EXTERNAL), t(0));
        let only_laptop = Displays::new(desk.all()[..1].to_vec());
        assert_eq!(home.refresh(&only_laptop), None);
        assert_eq!(home.current().map(|h| h.id), Some(EXTERNAL));
    }

    /// A policy change from settings re-derives on the same path startup takes,
    /// and abandons any dwell the old policy had going.
    #[test]
    fn changing_the_policy_re_derives_from_scratch() {
        let desk = desk();
        let mut home = Home::new(Policy::Cursor, None);
        home.update(Trigger::Rederive, &desk, on(LAPTOP), t(0));
        home.update(idle(), &desk, on(EXTERNAL), t(100));
        home.configure(Policy::Pinned, Some(r"\\?\DISPLAY#DELL#EDID".into()));
        assert_eq!(
            home.update(Trigger::Rederive, &desk, Signals::default(), t(150))
                .map(|h| h.id),
            Some(EXTERNAL)
        );
    }

    /// Only the policies with a per-poll signal cost anything per poll.
    #[test]
    fn only_the_following_policies_sample_anything() {
        assert!(Policy::Focused.samples_foreground());
        assert!(!Policy::Focused.samples_cursor());
        assert!(Policy::Cursor.samples_cursor());
        assert!(!Policy::Cursor.samples_foreground());
        for policy in [Policy::Primary, Policy::Pinned] {
            assert!(!policy.samples_foreground(), "{policy:?}");
            assert!(!policy.samples_cursor(), "{policy:?}");
        }
    }

    // ---- placement -----------------------------------------------------

    /// The gap under the pill is measured from the work area, so it is the same
    /// number of *logical* pixels on every monitor whatever its taskbar and
    /// whatever its DPI.
    #[test]
    fn the_gap_under_the_pill_is_identical_on_every_monitor() {
        for m in desk().all() {
            let home = HomeMonitor::from(m);
            let p = home.placement();
            let gap = (home.work.bottom - p.bottom) as f32 / home.scale();
            assert!(
                (gap - crate::pill::PILL_BOTTOM_MARGIN as f32).abs() <= 1.0,
                "monitor {} gap {gap}",
                m.id
            );
        }
    }

    /// Anchored to `rcWork`, so the pill sits above a bottom taskbar rather than
    /// over it — and a left-docked one shifts the horizontal centre for free.
    #[test]
    fn the_pill_clears_the_taskbar_and_centres_on_the_work_area() {
        // A monitor with a 48px bottom taskbar and a 60px left dock.
        let m = MonitorInfo {
            id: 7,
            work: Rect {
                left: 60,
                top: 0,
                right: 1920,
                bottom: 1032,
            },
            dpi: 96,
            primary: true,
            device_path: None,
            friendly_name: None,
        };
        let home = HomeMonitor::from(&m);
        let p = home.placement();
        assert!(p.bottom <= m.work.bottom, "the pill overlaps the taskbar");
        assert!(p.left >= m.work.left, "the pill overlaps the left dock");
        // Centred on the work area, not on the monitor.
        let slack = (m.work.left + m.work.right) / 2 - (p.left + p.right) / 2;
        assert!(slack.abs() <= 1, "off-centre by {slack}");
    }

    /// The window is the envelope, scaled by the home monitor — which is what
    /// makes the pill the same physical size on a 150% panel and a 100% one.
    #[test]
    fn the_window_is_the_envelope_at_the_home_monitors_scale() {
        for m in desk().all() {
            let home = HomeMonitor::from(m);
            let p = home.placement();
            let expected_w = (crate::pill::geom::ENVELOPE_W as f32 * home.scale()).round() as i32;
            let expected_h = (crate::pill::geom::ENVELOPE_H as f32 * home.scale()).round() as i32;
            assert_eq!(
                (p.width(), p.height()),
                (expected_w, expected_h),
                "{}",
                m.id
            );
        }
    }

    #[test]
    fn scale_is_the_home_monitors_dpi() {
        let mut home = HomeMonitor {
            id: 1,
            work: Rect::default(),
            dpi: 96,
        };
        assert_eq!(home.scale(), 1.0);
        home.dpi = 144;
        assert_eq!(home.scale(), 1.5);
    }
}
