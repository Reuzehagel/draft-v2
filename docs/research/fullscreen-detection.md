# Detecting "a fullscreen app is focused" on Windows 10/11

Research for making the resident pill auto-hide while a fullscreen application has focus.

Status: research only — nothing implemented. All API claims cite Microsoft Learn; signatures are
checked against the vendored source of the `windows` crate version this repo pins (0.58.0, see
`Cargo.toml`). Anything not backed by a primary source is explicitly labelled **[community /
unverified]**.

---

## 1. Recommendation

**Use a geometry check as the primary signal, `SHQueryUserNotificationState` as a secondary
signal, and OR them together. Trigger on `EVENT_SYSTEM_FOREGROUND` via `SetWinEventHook`, with a
1 s polling backstop folded into the existing `about_to_wait` loop.**

Concretely, hide the resident pill when **any** of these hold:

| # | Signal | Catches |
|---|--------|---------|
| A | Foreground window's rect covers its monitor's `rcMonitor` (within a small tolerance), after the guards in §5 | borderless-windowed games, browser F11 / video fullscreen, VLC, UWP video apps, most exclusive-fullscreen games too |
| B | `SHQueryUserNotificationState()` returns `QUNS_RUNNING_D3D_FULL_SCREEN` or `QUNS_BUSY` | legacy DX exclusive fullscreen, Presentation Settings |
| C | `SHQueryUserNotificationState()` returns `QUNS_NOT_PRESENT` or `QUNS_PRESENTATION_MODE` | screensaver, locked machine, inactive Fast User Switching session, presentation mode |

Why both:

* **The geometry check is the load-bearing one.** Modern games default to borderless-windowed and
  Microsoft actively pushes developers off exclusive fullscreen: "you may want to reconsider
  whether your application actually needs a fullscreen exclusive mode, since the benefits of a flip
  model borderless window include faster Alt-Tab switching and better integration with modern
  display features"
  ([For best performance, use DXGI flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)).
  A borderless-fullscreen window is, structurally, just a window whose rect equals the monitor
  rect — geometry is the only signal that definitionally catches it.
* **`SHQueryUserNotificationState` is the cheap safety net.** It is the API Microsoft designates
  for exactly this decision ("Applications should call **SHQueryUserNotificationState** and test
  the return value before displaying any notification UI … Notifications should only be displayed
  if this API returns `QUNS_ACCEPTS_NOTIFICATIONS`",
  [SHQueryUserNotificationState](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shqueryusernotificationstate)).
  A resident always-on-top pill is notification-shaped UI, so honouring it is the semantically
  correct thing to do — and it gives us screensaver/lock/presentation-mode suppression for free,
  which the geometry check cannot see at all.
* Neither alone is sufficient: geometry misses exclusive-fullscreen edge cases and every
  non-fullscreen "do not disturb" state; `SHQueryUserNotificationState` has no per-monitor
  resolution and its `QUNS_BUSY` behaviour under borderless flip-model presentation is not
  documented (§3).

**Do not** build the design on `ABN_FULLSCREENAPP`, `IsWindowArranged`, or DWM attributes as
primary signals — see §4 for why each is unsuitable.

Grounding in this codebase:

* The pill HWND and its ex-styles live in `src/pill/window.rs` — `apply_layered_styles` sets
  `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`, and
  `PillWindow::create` positions it bottom-centre of `el.primary_monitor()`.
* The event pump is `App::about_to_wait` in `src/main.rs`; it already polls several channels each
  wakeup and sets `ControlFlow::wait_duration(Duration::from_millis(50))` when the pill is not
  active, `ControlFlow::WaitUntil(now + 1000/PILL_FRAME_RATE_HZ)` when it is. This is the natural
  home for the fullscreen check.
* Note for whoever picks this up: there is currently **no** cursor-position/hover poll in the tree
  (grep for `GetCursorPos` returns nothing). The only existing cadences are the two above.
* The visibility decision itself belongs in the pure core (`src/session.rs`), which already returns
  `Command`s rather than performing effects — the Win32 query should be an input into
  `Session`/`PillAdapter`, not a call made from inside the renderer.

---

## 2. Approach 1 — foreground rect vs monitor rect

### The primitives

* [`GetForegroundWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getforegroundwindow)
  — "Retrieves a handle to the foreground window (the window with which the user is currently
  working)." Critically: "The foreground window can be **NULL** in certain circumstances, such as
  when a window is losing activation." Handle the null.
* [`GetWindowRect`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect)
  — screen coordinates, bottom-right exclusive. Two documented gotchas: "GetWindowRect is
  virtualized for DPI" and "In Windows Vista and later, the Window Rect now may include invisible
  resize borders." The latter is why a tolerance is required (§5).
* [`MonitorFromWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-monitorfromwindow)
  — "retrieves a handle to the display monitor that has the largest area of intersection with the
  bounding rectangle of a specified window." `MONITOR_DEFAULTTONULL` returns NULL if the window
  intersects no monitor. Also: "If the window is currently minimized, **MonitorFromWindow** uses
  the rectangle of the window before it was minimized."
* [`GetMonitorInfo`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getmonitorinfoa)
  / [`MONITORINFO`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-monitorinfo)
  — `rcMonitor` is the full monitor rect "expressed in virtual-screen coordinates. Note that if the
  monitor is not the primary display monitor, some of the rectangle's coordinates may be negative
  values." `cbSize` **must** be set before the call. `rcWork` is the work area (excludes taskbar) —
  compare against `rcMonitor`, not `rcWork`, or every maximized window reads as fullscreen.
* [`GetShellWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getshellwindow)
  — "Retrieves a handle to the Shell's desktop window… If no Shell process is present, the return
  value is **NULL**."
* [`GetDesktopWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdesktopwindow)
  — "an area on top of which other windows are painted"; its rect is the whole primary screen, so
  it would trivially match the monitor rect if it ever became foreground.

### Coverage by scenario

| Scenario | Geometry check result |
|---|---|
| Borderless-windowed game (modern default) | **Detected.** Window rect == monitor rect by construction. |
| Exclusive-fullscreen D3D game | **Usually detected** — DXGI resizes the HWND to the output. But the window is on top of everything anyway, so a mis-detection here is visually harmless; signal B covers it. |
| Browser fullscreen (F11 / video) | **Detected.** Chrome/Firefox/Edge resize the top-level HWND to the monitor rect. Note the foreground HWND does not change on F11 — this is the case that needs `EVENT_OBJECT_LOCATIONCHANGE` or the polling backstop (§6). |
| VLC fullscreen | **Detected** (top-level window resized to the monitor). |
| Netflix UWP app fullscreen | **Detected** — the CoreWindow is a real top-level HWND (class `Windows.UI.Core.CoreWindow`) sized to the monitor. `SHQueryUserNotificationState` may separately report `QUNS_APP` ("A Windows Store app is running"), which is *not* on its own a reason to hide. |

---

## 3. Approach 2 — `SHQueryUserNotificationState`

Signature: `SHSTDAPI SHQueryUserNotificationState(QUERY_USER_NOTIFICATION_STATE *pquns)`, header
`shellapi.h`, DLL `Shell32.dll`, minimum Windows Vista
([docs](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shqueryusernotificationstate)).

Documented values ([QUERY_USER_NOTIFICATION_STATE](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ne-shellapi-query_user_notification_state)),
verbatim:

| Value | Docs | Should the pill hide? |
|---|---|---|
| `QUNS_NOT_PRESENT` (1) | "A screen saver is displayed, the machine is locked, or a nonactive Fast User Switching session is in progress." | Yes |
| `QUNS_BUSY` (2) | "A full-screen application is running or Presentation Settings are applied." | Yes |
| `QUNS_RUNNING_D3D_FULL_SCREEN` (3) | "A full-screen (exclusive mode) Direct3D application is running." | Yes |
| `QUNS_PRESENTATION_MODE` (4) | "The user has activated Windows presentation settings to block notifications and pop-up messages." | Yes |
| `QUNS_ACCEPTS_NOTIFICATIONS` (5) | "None of the other states are found, notifications can be freely sent." | No |
| `QUNS_QUIET_TIME` (6) | "the first hour after a new user logs into his or her account for the first time" | No — this is about balloon spam, not occlusion |
| `QUNS_APP` (7) | "A Windows Store app is running." | No — a foregrounded Store app is not necessarily fullscreen |

Three caveats that matter here:

1. **The states are mutually exclusive and prioritised.** "during quiet time, if the user is in one
   of the other blocked modes (QUNS_NOT_PRESENT, QUNS_BUSY, QUNS_PRESENTATION_MODE, or
   QUNS_RUNNING_D3D_FULL_SCREEN) SHQueryUserNotificationState returns only that value, and does not
   report QUNS_QUIET_TIME." So a single scalar — you cannot ask "fullscreen AND presentation mode".
2. **No change notification for fullscreen.** "Top-level windows receive a `WM_SETTINGCHANGE`
   message when the user turns presentation settings on or off, and also when the user's session is
   locked or unlocked. **Note that there are no notifications sent when the user starts or stops a
   full-screen application.**" This is the single strongest argument for keeping a polling
   backstop rather than going event-only.
3. **Desktop apps only / not callable from a service.** The reference lists "Windows Vista
   [desktop apps only]" and the state is *per interactive user session*. Draft is a tray-resident
   desktop process in the user's session, so this is satisfied — but it is worth recording that a
   future "run as a service" idea would break this signal.

### Behaviour under modern DWM / flip-model presentation

There is **no primary-source statement** about what `SHQueryUserNotificationState` returns for a
borderless-windowed or DXGI flip-model "fullscreen" app. What the docs do establish:

* `QUNS_RUNNING_D3D_FULL_SCREEN` is defined narrowly as "(exclusive mode) Direct3D", and DXGI flip
  model swapchains do not use exclusive fullscreen — DirectFlip/Independent Flip is composition
  bypass for an ordinary window, not an exclusive mode
  ([flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)).
  It therefore should not be expected for borderless games.
* `QUNS_BUSY` is defined as "A full-screen application is running", which is the shell's own
  notion of full-screen — the same notion that hides the taskbar and drives `ABN_FULLSCREENAPP`.

**[community / unverified]** The widely reported behaviour is that borderless-fullscreen apps and
browser F11 fullscreen surface as `QUNS_BUSY`, and that `QUNS_RUNNING_D3D_FULL_SCREEN` is
essentially only seen with legacy exclusive-mode D3D. This matches the definitions above but is
**not** documented, and should be verified empirically on a Win11 box before being relied on. Even
if it holds, `QUNS_BUSY` is machine-global: it says nothing about *which* monitor is covered, which
is why it cannot replace the geometry check on a multi-monitor setup.

---

## 4. Approach 3 — everything else, and why it is not the answer

* **`ABN_FULLSCREENAPP` + `SHAppBarMessage`.** This is literally the shell telling you a
  full-screen app opened or closed: "Notifies an appbar when a full-screen application is opening
  or closing… `fOpen` … **TRUE** if the application is opening or **FALSE** if it is closing"
  ([ABN_FULLSCREENAPP](https://learn.microsoft.com/en-us/windows/win32/shell/abn-fullscreenapp)).
  It is exactly the event we want. The problem is the delivery mechanism: the notification "is sent
  in the form of an application-defined message that is set by the
  [ABM_NEW](https://learn.microsoft.com/en-us/windows/win32/shell/abm-new) message" — i.e. you must
  **register as an appbar**. Registering an appbar makes the shell treat Draft as desktop
  furniture that reserves screen edge space and participates in work-area negotiation
  ([SHAppBarMessage](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shappbarmessage)
  — `ABM_QUERYPOS`/`ABM_SETPOS`). That is a large, user-visible side effect for a floating pill, and
  it is not reversible without careful `ABM_REMOVE` teardown on every exit path. **Rejected**, but
  worth documenting as the "correct" API that is unusable for structural reasons.
* **`ABM_GETSTATE`.** "Retrieves the autohide and always-on-top states of the Windows taskbar" —
  returns `ABS_AUTOHIDE` and/or `ABS_ALWAYSONTOP`, with the note that "As of Windows 7,
  ABS_ALWAYSONTOP is no longer returned because the taskbar is always in that state"
  ([ABM_GETSTATE](https://learn.microsoft.com/en-us/windows/win32/shell/abm-getstate)). This tells
  you about the *user's taskbar setting*, not about a fullscreen app. Only use is as a guard: if
  the taskbar is set to autohide, a maximized window's rect can equal `rcMonitor`, which would be a
  false positive for the geometry check (§5). **Useful as a guard, not as a signal.**
* **`IsWindowArranged`.** "Determines whether a window is arranged (that is, whether it's
  snapped)… You should treat *arranged* as a window state similar to *maximized*. Arranged,
  maximized, and minimized are mutually exclusive states"
  ([IsWindowArranged](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-iswindowarranged),
  Windows 10 1903+). This detects Snap layouts, which is the *opposite* of fullscreen. It is
  useful only as a negative guard (an arranged window is not fullscreen). Note the docs' caveat:
  "At this time, this function does not have an associated header file or library file" — but the
  `windows` crate 0.58 does export it (`Win32::UI::WindowsAndMessaging::IsWindowArranged`), so no
  `GetProcAddress` dance is needed if we ever want it.
* **`DwmGetWindowAttribute`.** No DWM attribute reports "this window is fullscreen"
  ([DwmGetWindowAttribute](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetwindowattribute)).
  Two attributes are still worth knowing about: `DWMWA_EXTENDED_FRAME_BOUNDS`, which
  `GetWindowRect`'s own docs recommend for "the visible window bounds, not including the invisible
  resize borders" (with the warning that "unlike the Window Rect, the DWM Extended Frame Bounds are
  not adjusted for DPI"), and `DWMWA_CLOAKED`, for skipping cloaked windows (UWP suspended
  windows, other virtual desktops). Prefer the tolerance in §5 over `DWMWA_EXTENDED_FRAME_BOUNDS` —
  mixing a DPI-adjusted rect (`MONITORINFO`) with a non-DPI-adjusted one is a bug factory.
* **`ITaskbarList` / `Shell_NotifyIcon`.** Neither exposes fullscreen state. `ITaskbarList` is
  taskbar button manipulation; `Shell_NotifyIcon` is tray icons. Not applicable.
* **`IVirtualDesktopManager`.** Only answers "is this window on the current virtual desktop"
  ([IVirtualDesktopManager](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager)).
  Irrelevant: we query the *foreground* window, which is by definition on the current desktop.

---

## 5. False positives / negatives and concrete guards

Guards, in the order they should run:

1. **Null foreground.** `GetForegroundWindow()` can return NULL "when a window is losing
   activation" ([docs](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getforegroundwindow)).
   Treat NULL as "unknown" → **keep the previous decision**, do not flip to visible. Otherwise the
   pill flickers on during every Alt-Tab.
2. **Our own windows.** Compare against the pill HWND (`LayeredSurface::hwnd` in
   `src/pill/window.rs`) and, more robustly, against our PID via `GetWindowThreadProcessId` +
   `GetCurrentProcessId`. The pill has `WS_EX_NOACTIVATE` so it should never be foreground, but the
   settings subprocess is a *different* PID, so a PID check must whitelist it or use HWND identity.
3. **Shell / desktop windows.** Skip if `hwnd == GetShellWindow()` or `hwnd == GetDesktopWindow()`.
   Additionally check the class name via `GetClassNameW` against `Progman`, `WorkerW`, and
   `Shell_TrayWnd` — `WorkerW` in particular is the wallpaper host and is monitor-sized, so it is a
   guaranteed false positive if it ever lands in the foreground (it does, transiently, when the
   desktop is clicked). **[community / unverified]** that `WorkerW` reaches the foreground on
   modern builds; the check costs nothing either way.
4. **Invisible / cloaked windows.** `IsWindowVisible(hwnd)` must be true. Optionally
   `DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, …)` and skip nonzero.
5. **Maximized-with-autohide-taskbar.** With the taskbar autohidden, `rcWork == rcMonitor`, so a
   plain maximized window matches. Guard with `!IsZoomed(hwnd)` (maximized windows are not
   fullscreen) — cheap and decisive. `IsWindowArranged(hwnd)` covers the snapped case similarly.
   `ABM_GETSTATE`/`ABS_AUTOHIDE` is an alternative but is a global setting, not per-window.
6. **DPI / off-by-one.** `GetWindowRect` "is virtualized for DPI"; `MONITORINFO.rcMonitor` is in
   virtual-screen coordinates. Because winit 0.30 calls `SetProcessDpiAwarenessContext` with
   `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` at event-loop creation
   (`winit-0.30/src/platform_impl/windows/event_loop.rs`, `become_dpi_aware()`), Draft is
   per-monitor-v2 aware, so both rects are real physical pixels on the same monitor and no
   virtualization skew applies. Still allow a tolerance of ~2 px per edge for the invisible resize
   borders the `GetWindowRect` docs warn about, and require the window rect to **cover** the
   monitor (`left <= mon.left + tol && top <= mon.top + tol && right >= mon.right - tol && bottom
   >= mon.bottom - tol`) rather than to equal it — some apps overshoot by a pixel.
7. **Multi-monitor / "fullscreen on the other screen".** `MonitorFromWindow(fg,
   MONITOR_DEFAULTTONULL)` gives the fullscreen window's monitor; `MonitorFromWindow(pill_hwnd,
   MONITOR_DEFAULTTONEAREST)` gives the pill's. **Only hide when the two HMONITORs are equal.**
   Without this, a game on monitor 2 kills the pill on monitor 1 — a real regression for the
   dictate-while-gaming use case. Note this guard cannot be applied to the
   `SHQueryUserNotificationState` signal, which is session-global; accept that
   `QUNS_RUNNING_D3D_FULL_SCREEN` hides the pill everywhere (exclusive fullscreen historically
   minimises/blanks other outputs anyway).
8. **Remote desktop.** Under RDP the session's "monitor" is the RDP client viewport; a maximized
   window inside it will match `rcMonitor` if the RDP client itself is fullscreen. Guards 5 and 7
   handle the common cases. Also `WTSGetActiveConsoleSessionId`/`GetSystemMetrics(SM_REMOTESESSION)`
   is available if we ever want to just always show the pill under RDP.
9. **Screensaver / locked / secure desktop (UAC).** The geometry check is blind to all three: the
   secure desktop is a *different desktop*, and `GetForegroundWindow` from our desktop does not see
   it. `SHQueryUserNotificationState` returns `QUNS_NOT_PRESENT` for "A screen saver is displayed,
   the machine is locked, or a nonactive Fast User Switching session is in progress" — this is the
   main reason signal C earns its place. For UAC specifically, `EVENT_SYSTEM_DESKTOPSWITCH` ("The
   active desktop has been switched",
   [event constants](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)) is
   the precise trigger if it ever matters; the pill is on the user desktop and simply is not
   composited onto the secure desktop, so it is probably a non-issue.
10. **Hysteresis.** Windows transiently reports odd geometry mid-transition (entering fullscreen,
    Alt-Tab, monitor mode changes). Require the same answer twice, or debounce show-again by
    ~250–500 ms, so the pill does not blink during transitions. Hiding should be immediate;
    re-showing should be debounced.

Known **false negatives** to accept:
* A game that renders exclusive-fullscreen without resizing its HWND — covered by signal B.
* An app that covers the monitor with several windows rather than one — not detectable this way,
  and not worth chasing.
* Apps that go fullscreen without taking foreground (e.g. a background video wall) — out of scope;
  "focused" is the requirement.

---

## 6. Triggering: WinEvent hook vs polling

### `SetWinEventHook`

[`SetWinEventHook`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)
with `EVENT_SYSTEM_FOREGROUND` (0x0003) — "The foreground window has changed. The system sends
this event even if the foreground window has changed to another window in the same thread… the
*hwnd* parameter is the handle to the window that is in the foreground"
([event constants](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)).

Two facts from the reference that decide the integration shape:

* "The client thread that calls **SetWinEventHook** must have a message loop in order to receive
  events." Draft's winit event loop is that message loop — register the hook **on the winit
  thread**, after `EventLoop::new()`.
* "For out-of-context events, the event is delivered on the same thread that called
  **SetWinEventHook**." So with `WINEVENT_OUTOFCONTEXT` the callback runs re-entrantly from inside
  winit's message pump. Do essentially nothing in it: push the HWND to a `crossbeam_channel` (the
  crate is already a dependency) or set an `AtomicBool` dirty flag, and do the actual Win32 queries
  in `App::about_to_wait` alongside the existing `try_recv` drains. The docs also warn about
  reentrancy ("events are completed out of sequence unless the hook function handles this
  situation"), which the channel approach sidesteps entirely.
* Use `WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS` — "Prevents this instance of the hook from
  receiving the events that are generated by threads in this process", which for free implements
  guard 2 for the main process.
* Unhook with `UnhookWinEvent` on exit.

### `EVENT_OBJECT_LOCATIONCHANGE`

Needed because **the foreground window does not change when an app enters fullscreen in place**
(browser F11, a game switching display mode, VLC double-click). `EVENT_OBJECT_LOCATIONCHANGE`
(0x800B) — "An object has changed location, shape, or size… This event is generated in response to
a change in the top-level object within the object hierarchy; it is not generated for any children
that the object might have" ([event constants](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)).

This event is **extremely high-volume** if hooked globally — it fires for carets, tooltips, every
animated window. Mitigations, in order of preference:

1. Do not hook it. Rely on the polling backstop (below) — a ~1 s delay before the pill hides on F11
   is acceptable for a passive overlay.
2. If latency matters, hook it scoped to the current foreground window's process/thread
   (`SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE, …, idProcess,
   idThread, …)` — "If the *idProcess* parameter is nonzero and *idThread* is zero, the hook
   function receives the specified events from all threads in that process"), re-registering it on
   every `EVENT_SYSTEM_FOREGROUND`. Filter callbacks to `idObject == OBJID_WINDOW` and `idChild ==
   CHILDID_SELF` and `hwnd == current foreground` before doing any work.

### Polling

`SHQueryUserNotificationState`'s docs are explicit: "there are no notifications sent when the user
starts or stops a full-screen application." So even a perfect event setup needs a backstop.

**Recommended cadence: check every 1000 ms when idle; skip the check entirely while the pill is in
an active dictation state** (during dictation the user is deliberately using Draft, and the pill's
own 30 Hz redraw loop is running — hiding mid-capture would be wrong anyway).

Justification:
* `App::about_to_wait` in `src/main.rs` already wakes every 50 ms while idle
  (`ControlFlow::wait_duration(Duration::from_millis(50))`). A 1 s check is 1 wakeup in 20 — no new
  timer, no new thread, no change to the wake cadence, so zero additional power cost.
* One check is ~6 user32/shell32 calls (`GetForegroundWindow`, `GetWindowRect`,
  `MonitorFromWindow` ×2, `GetMonitorInfoW`, `IsZoomed`, `SHQueryUserNotificationState`) — all
  in-process or a cheap kernel transition, on the order of tens of microseconds. Running it at
  50 Hz would also be affordable, but 1 Hz is enough for a passive overlay and keeps the
  `SHQueryUserNotificationState` call (which crosses into shell32) rare.
* With `EVENT_SYSTEM_FOREGROUND` hooked, the common case (Alt-Tab into a game) is handled in
  milliseconds and the 1 s poll only covers in-place fullscreen transitions.

If we decide **not** to hook WinEvents at all (simplest possible first cut), poll at **250 ms**
instead — still 1 wakeup in 5, still imperceptible cost, and it bounds the worst-case
"pill visible over a game" window to a quarter second.

---

## 7. `windows` crate: features and module paths

This repo pins `windows = "0.58"` under `[target.'cfg(windows)'.dependencies]` in `Cargo.toml`.
Current features include `Win32_Foundation`, `Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`.

**Features to add:**

```toml
windows = { version = "0.58", features = [
    # ... existing ...
    "Win32_UI_Shell",          # SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE, SHAppBarMessage
    "Win32_UI_Accessibility",  # SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK, WINEVENTPROC
    "Win32_System_Threading",  # GetCurrentProcessId  (already enabled)
    "Win32_Graphics_Dwm",      # only if DWMWA_CLOAKED is wanted
] }
```

(Verified against `windows-0.58.0/Cargo.toml`: `Win32_UI_Shell = ["Win32_UI"]`,
`Win32_UI_Accessibility = ["Win32_UI"]`, `Win32_Graphics_Dwm = ["Win32_Graphics"]`.)

**Module paths (verified in the vendored 0.58.0 source):**

| Item | Path | Feature |
|---|---|---|
| `GetForegroundWindow`, `GetShellWindow`, `GetDesktopWindow`, `GetWindowRect`, `GetClassNameW`, `GetWindowThreadProcessId`, `IsWindowVisible`, `IsZoomed`, `IsWindowArranged` | `windows::Win32::UI::WindowsAndMessaging` | `Win32_UI_WindowsAndMessaging` |
| `EVENT_SYSTEM_FOREGROUND`, `EVENT_OBJECT_LOCATIONCHANGE`, `WINEVENT_OUTOFCONTEXT`, `WINEVENT_SKIPOWNPROCESS`, `OBJID_WINDOW`, `CHILDID_SELF` | `windows::Win32::UI::WindowsAndMessaging` (constants, `u32`) | `Win32_UI_WindowsAndMessaging` |
| `SetWinEventHook`, `UnhookWinEvent`, `HWINEVENTHOOK`, `WINEVENTPROC` | `windows::Win32::UI::Accessibility` | `Win32_UI_Accessibility` |
| `MonitorFromWindow`, `GetMonitorInfoW`, `MONITORINFO`, `HMONITOR`, `MONITOR_DEFAULTTONEAREST`, `MONITOR_DEFAULTTONULL` | `windows::Win32::Graphics::Gdi` | `Win32_Graphics_Gdi` |
| `SHQueryUserNotificationState`, `QUERY_USER_NOTIFICATION_STATE`, `QUNS_*`, `SHAppBarMessage`, `APPBARDATA`, `ABM_GETSTATE`, `ABS_AUTOHIDE` | `windows::Win32::UI::Shell` | `Win32_UI_Shell` |
| `DwmGetWindowAttribute`, `DWMWA_CLOAKED` | `windows::Win32::Graphics::Dwm` | `Win32_Graphics_Dwm` |
| `HWND`, `RECT`, `BOOL` | `windows::Win32::Foundation` | `Win32_Foundation` |
| `GetCurrentProcessId` | `windows::Win32::System::Threading` | `Win32_System_Threading` |

Exact 0.58 signatures (copied from the vendored source — note these differ from 0.5x/0.6x):

```rust
pub unsafe fn GetForegroundWindow() -> HWND;
pub unsafe fn GetShellWindow() -> HWND;
pub unsafe fn GetWindowRect<P0>(hwnd: P0, lprect: *mut RECT) -> windows_core::Result<()>;
pub unsafe fn MonitorFromWindow<P0>(hwnd: P0, dwflags: MONITOR_FROM_FLAGS) -> HMONITOR;
pub unsafe fn GetMonitorInfoW<P0>(hmonitor: P0, lpmi: *mut MONITORINFO) -> BOOL;
pub unsafe fn SHQueryUserNotificationState() -> windows_core::Result<QUERY_USER_NOTIFICATION_STATE>;
pub unsafe fn SetWinEventHook<P0>(
    eventmin: u32, eventmax: u32, hmodwineventproc: P0,
    pfnwineventproc: WINEVENTPROC, idprocess: u32, idthread: u32, dwflags: u32,
) -> HWINEVENTHOOK;
pub type WINEVENTPROC = Option<unsafe extern "system" fn(
    hwineventhook: HWINEVENTHOOK, event: u32, hwnd: HWND,
    idobject: i32, idchild: i32, ideventthread: u32, dwmseventtime: u32)>;
```

Note `SHQueryUserNotificationState` in windows-rs 0.58 **returns** the state rather than taking an
out-param, and `GetWindowRect` returns `Result<()>` rather than `BOOL`.

### Sketch

```rust
// src/pill/fullscreen.rs (proposed)
#![cfg(windows)]

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_NOT_PRESENT,
    QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetShellWindow,
    GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, IsZoomed,
};

/// Slack for the "invisible resize borders" GetWindowRect can include.
const EDGE_TOLERANCE: i32 = 2;

/// True when the pill should hide itself.
///
/// `pill_hwnd` is `LayeredSurface::hwnd` from `crate::pill::window`; it is used
/// both to skip our own window and to compare monitors, so a fullscreen app on
/// a *different* monitor does not hide a pill the user can still see.
pub fn should_hide(pill_hwnd: HWND) -> bool {
    unsafe { session_blocks_ui() || foreground_covers_pill_monitor(pill_hwnd) }
}

/// Signals B and C: session-global "do not disturb" states.
unsafe fn session_blocks_ui() -> bool {
    match SHQueryUserNotificationState() {
        Ok(s) => {
            s == QUNS_RUNNING_D3D_FULL_SCREEN
                || s == QUNS_BUSY
                || s == QUNS_NOT_PRESENT
                || s == QUNS_PRESENTATION_MODE
        }
        // Shell not ready / call failed: do not hide on an unknown.
        Err(_) => false,
    }
}

/// Signal A: the focused window covers the monitor the pill lives on.
unsafe fn foreground_covers_pill_monitor(pill_hwnd: HWND) -> bool {
    let fg = GetForegroundWindow();
    if fg.is_invalid() {
        // "can be NULL ... when a window is losing activation" — unknown, not false.
        return false;
    }
    if fg == pill_hwnd || fg == GetShellWindow() || fg == GetDesktopWindow() {
        return false;
    }
    if !IsWindowVisible(fg).as_bool() {
        return false;
    }
    // Maximized (and snapped) windows are explicitly not fullscreen; this is the
    // guard against "maximized + autohidden taskbar".
    if IsZoomed(fg).as_bool() {
        return false;
    }
    // Our own process (settings subprocess is a different PID, so also check HWND above).
    let mut pid = 0u32;
    GetWindowThreadProcessId(fg, Some(&mut pid));
    if pid == GetCurrentProcessId() {
        return false;
    }
    if is_shell_class(fg) {
        return false;
    }

    let fg_mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
    if fg_mon.is_invalid() {
        return false;
    }
    let pill_mon = MonitorFromWindow(pill_hwnd, MONITOR_DEFAULTTONEAREST);
    if fg_mon != pill_mon {
        return false; // fullscreen on another screen: leave the pill alone
    }

    let Some(mon) = monitor_rect(fg_mon) else {
        return false;
    };
    let mut win = RECT::default();
    if GetWindowRect(fg, &mut win).is_err() {
        return false;
    }

    let t = EDGE_TOLERANCE;
    win.left <= mon.left + t
        && win.top <= mon.top + t
        && win.right >= mon.right - t
        && win.bottom >= mon.bottom - t
}

unsafe fn monitor_rect(mon: HMONITOR) -> Option<RECT> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // Compare against rcMonitor, NOT rcWork — rcWork excludes the taskbar and
    // every maximized window would match it.
    GetMonitorInfoW(mon, &mut mi).as_bool().then_some(mi.rcMonitor)
}

unsafe fn is_shell_class(hwnd: HWND) -> bool {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    if n <= 0 {
        return false;
    }
    let name = String::from_utf16_lossy(&buf[..n as usize]);
    matches!(name.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd")
}
```

Hook registration, for the event-driven variant (register on the winit thread, e.g. from
`App::resumed` or right after `EventLoop::new()` in `src/main.rs`):

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

static FOREGROUND_DIRTY: AtomicBool = AtomicBool::new(true);

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK, _event: u32, _hwnd: windows::Win32::Foundation::HWND,
    _idobject: i32, _idchild: i32, _thread: u32, _time: u32,
) {
    // Runs re-entrantly inside winit's message pump: do nothing but flag.
    FOREGROUND_DIRTY.store(true, Ordering::Relaxed);
}

/// Must be called from the thread that owns the winit event loop — the docs
/// require a message loop, and out-of-context events are delivered on the
/// thread that registered the hook.
pub unsafe fn install() -> HWINEVENTHOOK {
    SetWinEventHook(
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_FOREGROUND,
        None, // no DLL: WINEVENT_OUTOFCONTEXT
        Some(on_foreground),
        0, // all processes
        0, // all threads
        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
    )
}

pub unsafe fn uninstall(hook: HWINEVENTHOOK) {
    let _ = UnhookWinEvent(hook);
}
```

In `App::about_to_wait` (`src/main.rs`), alongside the existing drains:

```rust
// Re-evaluate on a foreground change, or once a second as a backstop —
// SHQueryUserNotificationState's docs state no notification is sent when a
// full-screen application starts or stops.
let due = self.last_fullscreen_check.elapsed() >= Duration::from_secs(1);
if FOREGROUND_DIRTY.swap(false, Ordering::Relaxed) || due {
    self.last_fullscreen_check = Instant::now();
    let hide = pill::fullscreen::should_hide(self.pill.hwnd());
    // Feed into the pure core rather than touching the window here, so the
    // resident-pill visibility rule stays testable in src/session.rs.
    let cmds = self.session.on_occlusion_changed(hide, Instant::now());
    self.run_commands(cmds, el);
}
```

---

## 8. Open questions to settle empirically

1. Does `SHQueryUserNotificationState` return `QUNS_BUSY` for a borderless-fullscreen game and for
   browser F11 on Windows 11? (Undocumented; §3.) If it does, signal B alone might cover most
   cases — but the multi-monitor guard still requires signal A.
2. Does `WorkerW` actually reach the foreground on Windows 11 24H2+? Guard is free either way.
3. How do Netflix/other UWP media apps report — `QUNS_APP` alongside a monitor-sized CoreWindow, or
   `QUNS_BUSY`?
4. Whether the pill should also hide on `QUNS_APP` when the Store app happens to be fullscreen —
   the geometry check already handles that, so probably not.

---

## Sources

All Microsoft Learn:

- [SHQueryUserNotificationState](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shqueryusernotificationstate)
- [QUERY_USER_NOTIFICATION_STATE](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ne-shellapi-query_user_notification_state)
- [GetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getforegroundwindow)
- [GetWindowRect](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect)
- [MonitorFromWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-monitorfromwindow)
- [GetMonitorInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getmonitorinfoa)
- [MONITORINFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-monitorinfo)
- [GetShellWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getshellwindow)
- [GetDesktopWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdesktopwindow)
- [IsWindowArranged](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-iswindowarranged)
- [SetWinEventHook](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)
- [Event Constants](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)
- [SHAppBarMessage](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shappbarmessage)
- [ABM_GETSTATE](https://learn.microsoft.com/en-us/windows/win32/shell/abm-getstate)
- [ABN_FULLSCREENAPP](https://learn.microsoft.com/en-us/windows/win32/shell/abn-fullscreenapp)
- [DwmGetWindowAttribute](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetwindowattribute)
- [DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)
- [IVirtualDesktopManager](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager)
- [For best performance, use DXGI flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)

Crate sources (checked locally, not blog posts):
`windows-0.58.0` (`Cargo.toml` feature graph; `src/Windows/Win32/{UI/WindowsAndMessaging,
UI/Shell, UI/Accessibility, Graphics/Gdi, Graphics/Dwm}/mod.rs`), `winit-0.30.13`
(`src/platform_impl/windows/dpi.rs`, `event_loop.rs`).
