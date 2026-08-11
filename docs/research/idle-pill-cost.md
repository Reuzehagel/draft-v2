# What a permanently visible pill costs when idle

Research for #21, a child of the resident-pill map (#15). Establishes the steady-state cost of an
always-on-top layered window that is present indefinitely, and the discipline that keeps it near
zero.

Status: research only — nothing implemented. Every Win32 claim cites Microsoft Learn (or, where
noted, a Microsoft-published whitepaper or the PresentMon tool's own documentation). Claims about
winit and the `windows` crate are checked against the vendored sources of the versions this repo
resolves to (`winit-0.30.13`, `windows-0.58.0`). Anything that is inference rather than documented
behaviour is labelled **[inference]**; anything I could not establish is in §9.

Prior decisions on this map are taken as given and costed, not re-litigated: the 36×10 nub (#15),
cursor-position hover polling (#15/#20), live `WS_EX_TRANSPARENT` flipping (#20), fullscreen
auto-hide (#19), 90–160 ms expand animations (#18), and the standing "the pill must never take
focus" constraint.

---

## 1. Recommendation

**A resident idle nub is cheap, and the cheap part is the part everyone worries about. The layered
surface is genuinely free: push it once and the system holds and composes it forever, with no
`WM_PAINT`, no per-frame `UpdateLayeredWindow`, and ~1.4 KB of pixels. The cost that actually
exists is the 20 Hz event-loop wakeup — and Draft already pays that today, unconditionally, for
nothing.**

Three findings decide the shape of the implementation ticket:

1. **The surface is free and documented to be.** "The **UpdateLayeredWindow** function maintains the
   window's appearance on the screen"
   ([UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)).
   The re-push triggers are a short, mostly-inferred list (§3). See §2.

2. **The wakeup is not new, and it is worse than the docs recommend.** `App::about_to_wait` already
   sets `ControlFlow::wait_duration(50 ms)` on *every* idle iteration
   (`src/main.rs:411`) — Draft wakes 20×/second today whether or not a pill exists. Worse,
   winit 0.30.13 implements `WaitUntil` with a
   [`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw)
   waitable timer armed by plain `SetWaitableTimer` — **high resolution, and not coalescable** (§5).
   The resident pill adds *zero* wakeups if the hover poll rides the existing tick. It also gives us
   the first real reason to fix a wakeup that is currently pure waste.

3. **The one genuine, unbounded risk is the compositor**, not the CPU: a topmost window over a
   fullscreen-borderless game or video player may cost the system independent flip. Microsoft
   documents three possible outcomes, not one, and does not publish the rule (§4). #19's fullscreen
   auto-hide is not a nice-to-have — it is the mitigation for the only cost on this list that can
   reach whole watts.

| Question | Answer |
|---|---|
| Can the idle nub render once and stay static? | **Yes.** One `UpdateLayeredWindow`; the system stores and composes the image. §2 |
| Does it need a `WM_PAINT` loop? | **No.** §2 |
| What invalidates the surface? | Display colour-depth change is the only *documented* one. DPI change forces a re-render because PMv2 processes are never bitmap-scaled. The rest is unverified. §3 |
| Compositor/GPU cost when idle? | ~1.4 KB of pixels at 100% DPI (5.6 KB at 200%). Draft's own supersample chain costs 22× that. §4 |
| Does it defeat a power optimisation? | **Possibly — this is the real cost.** Topmost content over a DirectFlipped app can force DWM back to composed mode. Hardware-dependent, undocumented, must be measured. §4 |
| What wakes the process? | One 20 Hz waitable timer, already present. Everything else is event-driven. §5 |
| What does the wakeup cost? | Not knowable from docs. Estimated ≲0.1% of one core; nobody has published a per-wakeup figure. §5 |
| Discipline? | §7 — sixteen checkable rules. |
| How to measure? | §8. |

---

## 2. The idle nub renders once and stays static

**Yes, and this is documented three times over.**

[Window Features → Layered Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features):

> The system automatically composes and repaints layered windows and the windows of underlying
> applications.

and, on the storage that makes that possible:

> because the system stores the image of a layered window, the system will not ask the window to
> paint if parts of it are revealed as a result of relative window moves on the desktop

(That sentence sits in the `SetLayeredWindowAttributes` paragraph, but the storage claim is stated
generally and the `UpdateLayeredWindow` reference repeats it in its own terms.)

[UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow):

> The **UpdateLayeredWindow** function maintains the window's appearance on the screen. The windows
> underneath a layered window do not need to be repainted when they are uncovered due to a call to
> **UpdateLayeredWindow**, because the system will automatically repaint them.

And the 2000-era MSDN paper Learn still hosts,
[Layered Windows (Gorokhovsky & Amadio)](https://learn.microsoft.com/en-us/previous-versions/ms997507(v=msdn.10)),
states the consequence explicitly:

> the application doesn't need to respond to WM_PAINT or other painting messages, because it has
> already provided the visual representation for the window and the system will take care of storing
> that image, composing it, and rendering it on the screen.

**Consequence for the design: one `UpdateLayeredWindow` per *state change*, never per frame.** The
idle nub is a single push at startup. It stays until something in §3 forces a re-push, or until the
Pill core derives a different geometry or mode.

### 2a. This retires a piece of the current code

`App::window_event` (`src/main.rs:344`) calls `self.pill.redraw()` on `WindowEvent::RedrawRequested`,
and `about_to_wait` calls it again at 30 Hz whenever `PillAdapter::is_active()`. `is_active()` is
`self.window.is_some()` (`src/main.rs:546`). **Under a resident design the window is always `Some`,
so as written the loop would pin itself at 30 Hz forever and re-push a byte-identical surface 30
times a second.** That is not a subtle regression; it is the single largest cost the resident design
could accidentally introduce, and it comes from a definition that stops being true the moment the
pill outlives a session.

`is_active()` must be redefined from *"a window exists"* to *"an animation or live meter is in
flight"*. Rule R1 in §7.

### 2b. `UpdateLayeredWindow` always updates the whole window

> **UpdateLayeredWindow** always updates the entire window. To update part of a window, use the
> traditional WM_PAINT and set the blend value using **SetLayeredWindowAttributes**.
> — [UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)

The partial-update route is unavailable to us anyway: `SetLayeredWindowAttributes` and
`UpdateLayeredWindow` are mutually exclusive modes, and the codebase already documents the recovery
dance (`src/pill/window.rs:231`, and #20 §3). So every push is a full 36×10 (or 104×32) blit. At
those sizes that is not a concern — see §4 — but it does mean there is no "just repaint the border"
optimisation available for the breathing/pulse states.

---

## 3. What invalidates the surface

Short version: **one documented trigger, one forced by DPI awareness, and a list of plausible ones
nobody has written down.** The honest posture is to make re-push idempotent and cheap, then re-push
defensively on all of them, rather than to reason about which are strictly necessary.

### Documented

**Display colour-depth change.** [UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow):

> An application should also process the message and re-create its layered windows when the
> display's color depth changes.

⚠️ The published sentence is grammatically broken — the message name has been dropped from the Learn
rendering. **[inference]** the message is `WM_DISPLAYCHANGE`, whose `wParam` is documented as "The
new image depth of the display, in bits per pixel"
([WM_DISPLAYCHANGE](https://learn.microsoft.com/en-us/windows/win32/gdi/wm-displaychange)) — that is
the only message that carries colour depth. Note the doc says **re-create**, not re-push: the DIB
section and the window, not just the pixels. Cheap for us; `LayeredSurface::resize` already tears
down and rebuilds the DC + DIB.

`WM_DISPLAYCHANGE` also fires on plain resolution changes ("sent to all windows when the display
resolution has changed"), which is what we actually care about: the nub is positioned relative to
the monitor's bottom edge (`PillWindow::create`, `src/pill/window.rs:48`), so a resolution change
moves it regardless of whether the surface survived.

### Forced by our DPI mode

`WM_DPICHANGED`. The process is per-monitor-v2 (established in #20 §7: winit's `become_dpi_aware()`
calls `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`), and
[High DPI Desktop Application Development](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows)
says PMv2 means:

> 2. The application seeing the raw pixels of each display 3. The application never being bitmap
> scaled by Windows

So on a DPI change **nothing scales the nub for us**. We must re-render at the new scale and
re-push, and resize the DIB. This is not "the surface was invalidated" — it is "the surface is now
the wrong size". Either way the action is the same.

Note the latent bug this touches: `PillWindow::scale` is captured once in `create()` from the
primary monitor and never updated (`src/pill/window.rs:41`). A session-scoped pill mostly gets away
with that because it is recreated per session. A resident pill will not. The same DPI page warns
about exactly this pattern:

> It is a very common practice to cache font sizes and DPI values at process initialization. When
> updating an application to become per-monitor DPI aware, DPI-sensitive data must be reevaluated
> whenever a new DPI is encountered.

### Explicitly *not* a trigger

`WM_DWMCOMPOSITIONCHANGED` is dead:

> As of Windows 8, DWM composition is always enabled, so this message is not sent regardless of
> video mode changes.
> — [WM_DWMCOMPOSITIONCHANGED](https://learn.microsoft.com/en-us/windows/win32/dwm/wm-dwmcompositionchanged)

Don't write a handler for it. "DWM composition restart" as a re-push trigger is a pre-Windows-8
concern.

`WM_THEMECHANGED` invalidates *theme handles* —

> Following the **WM_THEMECHANGED** broadcast, any existing theme handles are invalid.
> — [WM_THEMECHANGED](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-themechanged)

— and says nothing about layered surfaces. The pill draws from hard-coded colours in
`src/pill/render.rs`, not from theme handles, so this matters only if the nub is ever made
light/dark-aware. Then it is a *reason to redraw*, not an invalidation.

### Undocumented — assume nothing

I found no Microsoft statement in either direction about whether these invalidate a layered surface:
session lock/unlock, remote-desktop connect/disconnect, monitor sleep/wake, or GPU device reset /
TDR. The plumbing to observe them is documented and cheap:

- **Lock/unlock, RDP.** `WM_WTSSESSION_CHANGE` carries `WTS_SESSION_LOCK (0x7)`,
  `WTS_SESSION_UNLOCK (0x8)`, `WTS_REMOTE_CONNECT (0x3)`, `WTS_REMOTE_DISCONNECT (0x4)`,
  `WTS_CONSOLE_CONNECT/DISCONNECT`
  ([WM_WTSSESSION_CHANGE](https://learn.microsoft.com/en-us/windows/win32/termserv/wm-wtssession-change)).
  Register with
  [`WTSRegisterSessionNotification`](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsregistersessionnotification):
  "Session change notifications are sent in the form of a WM_WTSSESSION_CHANGE message. These
  notifications are sent only to the windows that have registered for them." Two gotchas from that
  page: you **must** call `WTSUnRegisterSessionNotification` before the window is destroyed, and
  there is an autostart race — "If this function is called before the dependent services of Remote
  Desktop Services have started, an RPC_S_INVALID_BINDING error code may be returned."
- **Remote session detection.** `GetSystemMetrics(SM_REMOTESESSION)` — "If the calling process is
  associated with a Terminal Services client session, the return value is nonzero"
  ([GetSystemMetrics](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getsystemmetrics)).

The design's answer to all of these should be the same and should not depend on knowing: **re-push
on the next tick after any of them, because a redundant push of a 1.4 KB surface costs nothing** (§4)
and a lost surface is a permanently invisible pill. This is rule R4.

---

## 4. What an idle always-on-top layered window inherently costs

### 4a. Memory — negligible, but Draft's own buffers are not

`UpdateLayeredWindow` is the *cheap* layering path, and Learn says why:

> using **UpdateLayeredWindow** directly uses memory more efficiently, because the system does not
> need the additional memory required for storing the image of the redirected window.
> — [Window Features → Layered Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features)

The archived paper sharpens it:

> windows redirected by the system will always carry the overhead of having to maintain a memory
> bitmap the size of the window for every redirected window. This is in addition to the memory
> normally consumed by a layered window if **UpdateLayeredWindow** was used
> — [Layered Windows (archive)](https://learn.microsoft.com/en-us/previous-versions/ms997507(v=msdn.10))

So there is one system-side bitmap per layered window, not two. Under DWM those live in video
memory: "their drawing is redirected to off-screen surfaces in video memory"
([DWM overview](https://learn.microsoft.com/en-us/windows/win32/dwm/dwm-overview)). Microsoft
publishes no per-window byte figure; the arithmetic below is just width × height × 4 and is
therefore a lower bound on what the system spends, not a measurement.

| Geometry | Logical | @100% | @150% | @200% |
|---|---|---|---|---|
| Idle nub | 36×10 | 1,440 B | 3,240 B | 5,760 B |
| Hover-expanded | 104×32 | 13,312 B | 29,952 B | 53,248 B |
| Recording pill (today) | 86×42 | 14,448 B | 32,508 B | 57,792 B |
| *(reference)* 1080p screen | 1920×1080 | 7.91 MB | — | — |

The idle nub at 200% DPI is **0.07% of one screen-sized surface**. This is not a number worth
optimising, and it is consistent with the only sizing guidance Learn gives: "For best drawing
performance by the layered window and any underlying windows, the layered window should be as small
as possible" ([UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)).
The nub obeys that better than anything else the pill has ever been.

**Draft's own allocation is 22× larger than the surface, and that is the number the ticket should
carry.** `PillWindow` holds four buffers sized off the device geometry (`src/pill/window.rs:69-75`):
`pixmap` (1×), `mid` (2×2 = 4×), `hires` (`SUPERSAMPLE` = 4, so 4×4 = 16×), plus the DIB section
(1×). Total 22× the device-pixel byte count:

| Geometry | @100% | @200% |
|---|---|---|
| Idle nub | 31.7 KB | 126.7 KB |
| Hover-expanded | 292.9 KB | 1.17 MB |

Today those are transient — created in `PillAdapter::set_mode` and dropped in `dismiss()`. Resident
means they are RSS for the life of the process. ~124 KB for a nub is fine. ~1.2 MB if the window is
permanently sized to the expanded envelope is also fine for a tray app, but it should be a decision,
not an accident.

### 4b. A design consequence worth taking: fix the window at the maximum envelope

`ensure_size()` (`src/pill/window.rs:142`) reallocates all four buffers and the DIB whenever the
window's inner size changes. #18's 90–160 ms out-cubic expand animation, driven per frame, would
otherwise mean a `SetWindowPos`-or-`UpdateLayeredWindow`-resize **plus four `Pixmap::new` and a
`CreateDIBSection` on every animation frame**.

The alternative: **size the window once to the maximum envelope of all pill states — 104×42 logical
(width from the expanded pill, height from the recording pill) — and animate pixels only.** Every
state draws into the same buffers, with alpha 0 outside the current shape. This:

- eliminates all resize churn and all `SetWindowPos` from the animation path,
- keeps `UpdateLayeredWindow`'s "always updates the entire window" property harmless (the window is
  always the same size),
- costs 1.5 MB of Draft buffers at 200% DPI instead of 127 KB, and
- **is safe for hit-testing**, because "Hit testing of a layered window is based on the shape and
  transparency of the window … areas of the window that are color-keyed or whose alpha value is zero
  will let the mouse messages through"
  ([Window Features](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features)) — the
  transparent margin around the nub is not a click target even once `WS_EX_TRANSPARENT` is cleared.

The one thing it changes is the **hover rect**: `GetWindowRect` would return the 104×42 envelope, not
the 36×10 nub, so the #20 rect test would trigger up to 34 logical px above the visible nub. That is
arguably a feature (a 10 px-tall hover target is unkind), but it must be an explicit choice — test
against a *derived* nub rect inset from the window rect, or against the window rect deliberately.
Rule R6.

**[inference]** — no source recommends this; it follows from `ensure_size`'s realloc behaviour plus
the documented alpha hit-test rule.

### 4c. DWM per-frame work — undocumented, and the docs lean the wrong way

There is **no** Microsoft statement that DWM skips per-frame work for static or occluded windows. I
looked; the agent looked. `DwmGetCompositionTimingInfo` is not the oracle people assume: since
Windows 8.1 its `hwnd` parameter "must be set to NULL. If this parameter is not set to NULL,
**DwmGetCompositionTimingInfo** returns E_INVALIDARG"
([DwmGetCompositionTimingInfo](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetcompositiontiminginfo)) —
it is system-wide, not per-window, and documents no occlusion policy. It does expose `rateCompose`,
`cFramesDisplayed`, `cFramesDropped`, `cPixelsDrawn`
([DWM_TIMING_INFO](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ns-dwmapi-dwm_timing_info)),
which makes it a *measurement* tool (§8), not a design input.

Nor can we ask whether we are occluded. `DXGI_STATUS_OCCLUDED` is DXGI-only and, per
[DXGI_STATUS](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-status), "You will
not receive DXGI_STATUS_OCCLUDED if you're using a flip model swap chain" — irrelevant to a GDI
layered window either way. `DwmGetWindowAttribute` has no occlusion attribute; the nearest,
`DWMWA_CLOAKED`, reports cloaking, and cloaking is explicitly *not* occlusion — `DWMWA_CLOAK`
"Cloaks the window such that it is not visible to the user. **The window is still composed by DWM**"
([DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)).

Note the asymmetry, because it bites again in §5: **Windows computes occlusion internally** — the
Windows 11 timer-resolution rule turns on "fully occluded" — but exposes no API to read it.

### 4d. The real cost: independent flip

This is the only item on the list that can plausibly reach whole watts, and the documentation is
**more nuanced than the folklore**.

[For best performance, use DXGI flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)
describes what a well-behaved fullscreen app gets:

> Once your swapchain has been "DirectFlipped," then the DWM can go to sleep, and only wake up when
> something changes outside of your application. Your application frames are sent directly to the
> screen, independently, with the same efficiency as fullscreen exclusive. This is "Independent
> Flip"

**The resident pill is, by construction, "something outside of the application."** But the same
paragraph continues, and this is the sentence the whole question turns on:

> If other desktop contents come on top, the DWM can either seamlessly transition back to composed
> mode, efficiently "reverse compose" the contents on top of the application before flipping it, or
> leverage MPO to maintain the independent flip mode.

Three outcomes, not one:

| Outcome | Cost |
|---|---|
| MPO keeps independent flip (nub on its own hardware overlay plane) | ~free |
| Reverse composition (DWM composes the nub into the app's buffer) | cheap — "results in less overall work to display the desktop" |
| Fall back to composed mode | DWM wakes every frame; this is the feared case |

Which one you get depends on the display, the driver, and the number of available MPO planes.
Microsoft publishes the *mechanism* — the three DirectFlip tiers ("DirectFlip", "DirectFlip with
panel fitters", "DirectFlip with multi-plane overlay (MPO)") — and then declines to publish the
decision rule, pointing at a tool instead: "Check out the
[PresentMon](https://github.com/GameTechDev/PresentMon) tool to get insight into which of the above
was used."

PresentMon's own docs define the labels you would see
([README-ConsoleApplication.md](https://raw.githubusercontent.com/GameTechDev/PresentMon/main/README-ConsoleApplication.md);
Intel/GameTechDev, authoritative for the tool, not a Microsoft spec):

- `Hardware: Independent Flip` — "the app does not have ownership of the screen, but is still
  swapping the displayed surface every frame."
- `Hardware Composed: Independent Flip` — "the app is using flip model swapchains, and has been
  granted a hardware overlay plane."
- `Composed: Flip` — "the app is windowed, is using flip model swapchains, and is sharing its
  surfaces with DWM to be composed."

**Conclusion: "a topmost window forces composed mode" is folklore, and the docs mildly contradict
it. But "a topmost window is free over a fullscreen app" is equally unsupported.** This is the one
question on this ticket that documentation cannot settle and measurement can, in about half a day
(§8).

**What #19 already buys us.** The fullscreen auto-hide is the mitigation. If the pill is hidden
whenever a fullscreen app owns the monitor, the independent-flip risk is retired *for exactly the
case where it matters* — games and full-screen video, the two workloads where a composed-mode
fallback is measurable. Two riders:

1. Hiding must mean **`ShowWindow(SW_HIDE)`**, not "draw a fully transparent surface". An
   alpha-0 layered window is still a composed window as far as DWM is concerned (cf. the
   `DWMWA_CLOAK` note in §4c — even cloaked windows are still composed). Rule R7.
2. #19's detection is foreground-rect-based and admits borderless-fullscreen apps that are *not*
   focused, and multi-monitor cases where the game is on monitor 2 and the pill on monitor 1. The
   pill should hide when a fullscreen app owns **the pill's monitor**, which is the stricter and
   correct test.

### 4e. Power guidance that does exist

The only first-party "stop rendering to save power" instruction I found is attached to a power
notification, not to overlays:

> Applications should register for this notification and stop rendering graphics content when the
> monitor is off to reduce system power consumption.
> — [Power Setting GUIDs](https://learn.microsoft.com/en-us/windows/win32/power/power-setting-guids), `GUID_MONITOR_POWER_ON`

It is directly on point and directly actionable (§6, rule R9). There is no Microsoft guidance about
always-on-top overlays specifically.

---

## 5. What wakes the process while idle

### 5a. The complete inventory

| Wakeup | Cadence | Kind | New with resident? |
|---|---|---|---|
| `about_to_wait` idle timer (`src/main.rs:411`) | 20 Hz | high-resolution waitable timer | **No — already unconditional** |
| Hover cursor poll (#15/#20) | rides the above | 2 user32 calls | No new wakeup |
| Fullscreen backstop poll (#19) | 1 Hz | rides the above (1 tick in 20) | No new wakeup |
| `EVENT_SYSTEM_FOREGROUND` hook (#19) | event-driven | out-of-context WinEvent | Yes, but event-driven |
| Tray icon menu events (`src/tray.rs:74`) | event-driven | crossbeam channel, drained on the tick | No |
| Global hotkeys (`src/hotkey.rs:166`) | event-driven | crossbeam channel, drained on the tick | No |
| Parakeet 5-min idle unload (`src/main.rs:395`) | rides the tick | `try_lock` + `elapsed` | No |
| Settings-child `try_wait` (`src/main.rs:350`) | rides the tick | one syscall | No |

**The resident pill adds no new wakeup source.** Everything it needs rides a timer Draft already
arms 20 times a second, on every idle iteration, forever.

### 5b. Nobody calls `timeBeginPeriod` — but winit does something adjacent

Checked, not assumed. `timeBeginPeriod` / `timeGetDevCaps` / `NtSetTimerResolution` appear nowhere in
`winit-0.30.13`, `cpal-0.15.3`, `global-hotkey-0.6.4`, `tray-icon-0.19.3`, or `eframe-0.29.1`
(grepped the vendored registry sources). Draft does not call it either. So Draft is not raising the
**global** timer resolution, and the Windows 10 2004+ rule means it could not affect other processes
if it did:

> Starting with Windows 10, version 2004, this function no longer affects global timer resolution.
> For processes which call this function, Windows uses the lowest value … requested by any process.
> For processes which have not called this function, Windows does not guarantee a higher resolution
> than the default system resolution.
> — [timeBeginPeriod](https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod)

**But winit takes the high-resolution path by a different door.** `winit-0.30.13`'s
`create_high_resolution_timer` (`src/platform_impl/windows/event_loop.rs:637`) calls:

```rust
CreateWaitableTimerExW(ptr::null(), ptr::null(),
                       CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS)
```

and `set_high_resolution_timer` (`:664`) arms it with plain **`SetWaitableTimer`** — not
`SetWaitableTimerEx`, so **no `TolerableDelay`, hence no coalescing**. The handle is then passed to
`MsgWaitForMultipleObjectsEx(1, [timer], …, QS_ALLINPUT, MWMO_INPUTAVAILABLE)` (`:751`).

Learn on that flag:

> **CREATE_WAITABLE_TIMER_HIGH_RESOLUTION** — Creates a high resolution timer. Use this value for
> time-critical situations when short expiration delays on the order of a few milliseconds are
> unacceptable.
> — [CreateWaitableTimerExW](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw)

So Draft's idle heartbeat is a **high-resolution, uncoalescable, 20 Hz kernel timer**, chosen by
winit on our behalf. That is the exact opposite of what Microsoft's own guidance asks for, twice
over (§5c). It is not caused by the resident pill — it is there today — but the resident pill is the
first feature that gives anyone a reason to look at it.

### 5c. What Microsoft actually recommends, and how far we are from it

The **Windows Timer Coalescing** whitepaper (Microsoft, 2009 — a Microsoft-published document, linked
from [SetCoalescableTimer](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setcoalescabletimer);
hosted at `download.microsoft.com/download/9/C/5/9C5B2167-8017-4BAE-9FDE-D599BAC8184A/TimerCoal.docx`)
gives the mechanism:

> many processor power management technologies require a minimum amount of idle time to obtain a net
> power-savings benefit. If the processor is idle for only very short periods of time, the power that
> is required to enter and exit the low-power state can be greater than the power that is saved.

and the priority order:

> We recommend that software developers evaluate their code first for opportunities to remove
> periodic activity. If it is possible, periodic activity should be changed to event-driven or
> interrupt-based designs.

and, failing that, the numbers:

> Software timers that use timer coalescing should specify a minimum of 32 milliseconds (ms) for the
> timer expiration tolerance.

> you should use timer periods of 50, 100, 250, 500, and 1,000 ms. Similarly, tolerable delay values
> of 50, 100, 150, and 250 ms are appropriate.

The 32 ms figure is mirrored on Learn: "a caller should specify a *TolerableDelay* value of at least
32 milliseconds. This value equals two default system clock intervals of 15.6 milliseconds"
([KeSetCoalescableTimer](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-kesetcoalescabletimer)).
The 15.6 ms default tick is also on Learn: "for Windows running on an x86 processor, the default
interval between system clock ticks is typically about 15 milliseconds"
([High-Resolution Timers](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/high-resolution-timers)),
along with the general rule we are on the right side of by period but the wrong side of by
resolution: "drivers should avoid setting the period of a long-running high-resolution timer to a
value that is less than the default interval between system clock ticks."

Scored against that guidance:

| Guidance | Draft today |
|---|---|
| Prefer event-driven over periodic | ✗ — polls three empty crossbeam channels 20×/s |
| Period should be 50/100/250/500/1000 ms | ✓ — 50 ms exactly |
| Tolerance ≥ 32 ms | ✗ — none; `SetWaitableTimer` has no tolerance |
| Don't use a high-resolution timer for long-running periodic work | ✗ — winit uses `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` |

**The single highest-leverage change available on this whole map is to make the idle loop
event-driven — `ControlFlow::Wait` (winit passes `INFINITE`, arms no timer at all) — and wake it by
posting to the event loop when something actually happens.** The reason `about_to_wait` polls today
is that hotkey and tray-menu events arrive on crossbeam channels from other threads
(`src/hotkey.rs:166`, `src/tray.rs:75`), and an indefinite `MsgWaitForMultipleObjectsEx` will not
wake for a channel send. The fix is `EventLoop::create_proxy()` — have each producer call
`EventLoopProxy::send_event` after the channel send, which posts a message to winit's internal
window and wakes the pump.

That is a bigger change than #21's ticket, and the resident pill needs a poll for hover anyway. So
the realistic target is **rule R10**: keep the tick, but make its period a function of state, and
suspend it entirely when nothing can be hovered.

### 5d. What 20 Hz actually costs — say plainly that nobody knows

**No Microsoft source gives a per-wakeup cost in joules, watts, or CPU time.** The whitepaper gives
the mechanism (C-state entry/exit overhead can exceed the savings) and the 32 ms threshold, and stops
there. The widely-repeated "1 ms timer resolution costs up to 25% battery" figure was traced only to
secondary blogs and is **not** in the Microsoft whitepaper — do not cite it.

The defensible statement, labelled as an estimate:

> **[inference / estimate]** Each idle tick does ~4 channel `try_recv`s, a `try_lock` + `elapsed`, a
> `try_wait`, and (with the resident pill) `GetCursorPos` + `GetWindowRect` + an integer rect test,
> plus 1-in-20 the #19 fullscreen check. That is on the order of tens of microseconds of work plus
> one thread wake and scheduler dispatch. At 20 Hz: **on the order of 1 ms of CPU per second, i.e.
> ≲0.1% of one core.** The tick itself — the wake, not the work — is plausibly the larger term on a
> deep-idle laptop, and its cost is not knowable without measurement.

Two things bound the blast radius, and both are documented:

**When the screen is off on a modern-standby machine, the process is suspended outright.** The
Desktop Activity Moderator adds interactive-session processes to a job object subject to suspension:

> If the process was created in an interactive session (session 1 or higher), DAM adds the process to
> a job object subject to **suspension**

> Processes that are subject to suspension have all their threads suspended (not allowed to run under
> any circumstances); app state (process memory) is maintained

> When the screen is on, the DAM is disengaged and does not impact any processes on the system.
> — [Desktop Activity Moderator](https://learn.microsoft.com/en-us/windows/win32/w8cookbook/desktop-activity-moderator)

So **the 20 Hz poll contributes nothing to modern-standby drain** — Draft is frozen. (Caveat: that
page is scoped "Clients – Windows 8"; whether the identical mechanism governs Windows 11 modern
standby is not stated. §9.) The page also warns to expect "inconsistencies in timer behavior" across
the transition, which matters for the Parakeet idle-unload deadline and for any `Instant`-based
animation the pill holds across a suspend — rule R14.

**When the screen is off but the machine is not in modern standby**, nothing suspends us, and that is
precisely the window `GUID_SESSION_DISPLAY_STATUS` exists to close (§6).

### 5e. The Windows 11 visibility heuristic cuts the other way

> Starting with Windows 11, if a window-owning process becomes fully occluded, minimized, or
> otherwise invisible or inaudible to the end user, Windows does not guarantee a higher resolution
> than the default system resolution.
> — [timeBeginPeriod](https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod)

and the same heuristic on the QoS side:

> By default in Windows 11 if a window owning process becomes fully occluded, minimized, or otherwise
> non-visible to the end user, and non-audible, Windows may automatically ignore the timer resolution
> request
> — [SetProcessInformation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessinformation)

Today Draft is a tray app with no visible window when idle — squarely in the "otherwise invisible"
bucket, and therefore a candidate for automatic throttling. **A permanently visible topmost nub
plausibly moves Draft out of that bucket permanently.** Whether Windows classifies a 36×10 always-on-
top toolwindow as "visible to the end user" is **not documented anywhere** (§9) — but the direction
of the risk is clear, and it is the one genuinely new cost the word "resident" introduces.

Two mitigations, both documented and both cheap:

- **`PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION`** — "any current timer resolution requests
  made by the process will be ignored. Timers belonging to the process are no longer guaranteed to
  expire with higher timer resolution, which can improve power efficiency"
  ([SetProcessInformation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessinformation)).
  This is the direct antidote to §5b: it tells Windows to ignore the high-resolution request winit
  makes on our behalf. Worked example is on that page.
- **EcoQoS** via `PROCESS_POWER_THROTTLING_EXECUTION_SPEED` — "The system will try to increase power
  efficiency through strategies such as reducing CPU frequency or using more power efficient cores.
  EcoQoS should be used when the work is not contributing to the foreground user experience."
  Tempting for a tray app, and **wrong for Draft**: the same page says "EcoQoS should not be used for
  performance critical or foreground user experiences", and this process runs an ONNX transcription
  model on demand. If it is ever adopted it must be toggled off around a capture, not set once.

### 5f. The `EVENT_SYSTEM_FOREGROUND` hook is the cheap one

Event-driven, which is the direction the whitepaper asks for. The documented costs are about
latency-in-your-callback, not idle power:

> Out-of-context hook functions are noticeably slower than in-context hook functions due to
> marshaling.

> The USER component of the operating system allocates memory for events that are handled by
> out-of-context hook functions. The memory is freed when the hook functions return. If a hook
> function does not process events quickly enough, USER resources are lowered, eventually resulting
> in a fault or extremely slow response times.
> — [Out-of-Context Hook Functions](https://learn.microsoft.com/en-us/windows/win32/winauto/out-of-context-hook-functions)

#19 already prescribes the right shape (register `EVENT_SYSTEM_FOREGROUND..EVENT_SYSTEM_FOREGROUND`
with `WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS`; do nothing in the callback but flag and
return). Nothing here changes for a resident pill. Note it delivers on the winit thread — "For
out-of-context events, the event is delivered on the same thread that called **SetWinEventHook**"
([SetWinEventHook](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)) —
so it also serves as a free wake for a would-be `ControlFlow::Wait` loop.

The tray icon and `RegisterHotKey` are both purely passive: they cost a registration and deliver
window messages. Neither is a timer. (No citation needed and none claimed — this is a statement about
what the APIs are, not about their measured cost.)

---

## 6. Guard APIs, and which of the near-duplicates is current

[Power Setting GUIDs](https://learn.microsoft.com/en-us/windows/win32/power/power-setting-guids)
settles the display-state question cleanly, and the answer is *not* the one the ticket guessed at:

- **`GUID_MONITOR_POWER_ON` — deprecated.** "**Windows 8 and Windows Server 2012:** New applications
  should use **GUID_CONSOLE_DISPLAY_STATE** instead of this notification." Do not use it. (It does
  carry the useful "stop rendering when the monitor is off" sentence quoted in §4e, but the
  instruction outlives the API.)
- **`GUID_SESSION_DISPLAY_STATUS` — this is ours.** "The display associated with the application's
  session has been powered on or off… This notification is sent only to user-mode applications." And
  the explicit steer: "**All applications that run in an interactive user-mode session should use
  this setting.** When kernel-mode applications register for monitoring the status, they should use
  **GUID_CONSOLE_DISPLAY_STATE** instead." Payload is `MONITOR_DISPLAY_STATE`: `PowerMonitorOff` (0),
  `PowerMonitorOn` (1), `PowerMonitorDim` (2).
- **`GUID_CONSOLE_DISPLAY_STATE`** — same payload; the session-0/kernel-mode choice. Not ours.

Two more worth taking for free on the same registration:

- **`GUID_SESSION_USER_PRESENCE`** — `PowerUserInactive` (2) means "The user activity timeout has
  elapsed with no interaction from the user". A strictly better idle signal than a timer.
- **`GUID_POWER_SAVING_STATUS`** — `0x1` means "Battery saver is on. Save energy where possible." An
  explicit instruction from the OS to back off.

Registration: [`RegisterPowerSettingNotification`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerpowersettingnotification)
with `DEVICE_NOTIFY_WINDOW_HANDLE` — "Notifications are sent using WM_POWERBROADCAST messages with a
*wParam* parameter of PBT_POWERSETTINGCHANGE."

For suspend/resume, [`RegisterSuspendResumeNotification`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registersuspendresumenotification)
("Similar to PowerRegisterSuspendResumeNotification, but operates in user mode and can take a window
handle"), delivering `PBT_APMSUSPEND` / `PBT_APMRESUMEAUTOMATIC`. The DAM page names these as the
opt-in path for suspension notice.

**A wrinkle worth flagging for the implementation ticket:** all of these are delivered to a *window
procedure*, and winit does not surface `WM_POWERBROADCAST` or `WM_WTSSESSION_CHANGE` as
`WindowEvent`s. Consuming them means subclassing the pill's (or a dedicated message-only window's)
wndproc — the same cost #20 §6 already identified for `WM_MOUSEACTIVATE`. **The two should share one
subclass.** Rule R16.

---

## 7. The discipline — sixteen rules

Written as checkable assertions the implementation ticket can be reviewed against.

### Rendering

| # | Rule |
|---|---|
| **R1** | Redefine `PillAdapter::is_active()`. It must mean *"an animation or live meter is running"*, not `window.is_some()`. Today's definition would pin the loop at 30 Hz forever the moment the pill becomes resident (§2a). |
| **R2** | **Never call `UpdateLayeredWindow` when the derived render state is unchanged.** Hash or compare the inputs to the renderer — `(Geom, PillMode, bar_heights, pulse, alpha, scale)` — and early-return on equality. The idle nub's inputs are constant, so this reduces to exactly one push. |
| **R3** | Drop `WindowEvent::RedrawRequested → pill.redraw()`. A layered window has no paint cycle to service (§2); servicing it re-pushes an identical surface for free-of-charge nothing. |
| **R4** | Re-push (and re-create the DIB) on: `WM_DISPLAYCHANGE`, `WM_DPICHANGED`/`ScaleFactorChanged`, `WTS_SESSION_UNLOCK`, `WTS_CONSOLE_CONNECT`, `WTS_REMOTE_CONNECT`, and `PBT_APMRESUMEAUTOMATIC`. Only the first two are documented as necessary (§3); the rest are cheap insurance against an invisible pill. |
| **R5** | Re-derive `PillWindow::scale` from `GetDpiForWindow` on every DPI change. Do not keep the value captured in `create()` (`src/pill/window.rs:41`) — it is a latent bug a resident pill will expose. |
| **R6** | Decide explicitly whether the window is fixed at the 104×42 envelope (no resize churn, larger hover rect — §4b) or resized per state. If fixed, the hover test must use a derived nub rect, not the raw `GetWindowRect`. |
| **R7** | Hiding means `ShowWindow(hwnd, SW_HIDE)`. Never "hide" by pushing an alpha-0 surface — that is still a composed topmost window (§4d). |

### Polling

| # | Rule |
|---|---|
| **R8** | Suspend the hover poll entirely whenever the pill is hidden — fullscreen auto-hide (#19), user toggle-off, or monitor off. A hidden pill cannot be hovered; polling for it is pure waste. |
| **R9** | Register for `GUID_SESSION_DISPLAY_STATUS`. On `PowerMonitorOff`, hide the pill and stop all polling; on `PowerMonitorOn`, restore and re-push. This is the documented instruction: "stop rendering graphics content when the monitor is off to reduce system power consumption" (§4e, §6). |
| **R10** | Make the tick period a function of state, not a constant. Suggested ladder: **animating** 33 ms → **cursor within N px of the pill's monitor bottom edge** 50 ms → **cursor elsewhere, or on another monitor** 250 ms → **pill hidden / display off / session locked** `ControlFlow::Wait` (no timer at all). The cheap discriminator is `GetCursorPos` alone — no `GetWindowRect` needed to know the cursor is 800 px away. |
| **R11** | Coalesce every periodic check onto the same tick. #19's 1 Hz fullscreen backstop, the Parakeet idle-unload check, and the settings-child `try_wait` must all be counters on the hover tick, never separate timers. This is already how the code is written; keep it that way. |
| **R12** | Gate hover on `GetCursorInfo`'s `CURSOR_SHOWING` (#20 §7). A suppressed or hidden cursor — touch, pen, a game that hid it — must not expand the pill. |
| **R13** | On `WTS_SESSION_LOCK`, hide and stop polling; on `WTS_SESSION_UNLOCK`, restore. Remember `WTSUnRegisterSessionNotification` before window destruction, and handle the `RPC_S_INVALID_BINDING` autostart race (§3). |
| **R14** | Treat any `Instant`-based deadline as unreliable across a suspend/resume or modern-standby transition — DAM documents "inconsistencies in timer behavior" (§5d). On `PBT_APMRESUMEAUTOMATIC`, re-derive pill state from scratch rather than resuming an in-flight animation. |

### Process

| # | Rule |
|---|---|
| **R15** | Never call `timeBeginPeriod`, and add a test or a CI grep that fails if it (or a dependency) starts to. Consider `PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION` to neutralise winit's `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` request (§5b, §5e). Do **not** adopt EcoQoS unconditionally — it is documented as wrong for latency-sensitive work, and transcription is exactly that. |
| **R16** | One wndproc subclass, shared. `WM_MOUSEACTIVATE` (#20 §6), `WM_POWERBROADCAST`, `WM_WTSSESSION_CHANGE`, and `WM_DISPLAYCHANGE` all need it and winit surfaces none of them. Building four is four chances to clobber `GWLP_WNDPROC`. |

### The rule that is not on this list

*"Reduce the idle poll below 50 ms for snappier hover."* #20 §7 floats 25 ms as the lever if hover
feels laggy. Under §5c that doubles the wakeup rate to fix a latency the 90–160 ms expand animation
already dominates. **Prefer R10's proximity ladder** — it makes hover-in *faster* near the pill and
much cheaper everywhere else.

---

## 8. How to measure it

Half a day on a real laptop, on battery, falsifies everything above.

### 8a. The independent-flip question (the one that matters)

The only experiment on this list that can change the design.

1. Install [PresentMon](https://github.com/GameTechDev/PresentMon) — Microsoft's own docs point at it
   for exactly this ("Check out the PresentMon tool to get insight into which of the above was used").
2. Run a borderless-fullscreen game or a fullscreen video, on battery, with the display at its native
   resolution.
3. Record `PresentMode` for ~60 s **with the resident pill visible** and again **with it hidden**
   (temporarily disable #19's auto-hide so the pill really is on top).
4. Read the mode column against the PresentMon definitions in §4d.

**Alarming result:** the mode changes from `Hardware: Independent Flip` or
`Hardware Composed: Independent Flip` (pill hidden) to `Composed: Flip` (pill visible). That means
the pill is costing the system its independent flip on this hardware, and #19's auto-hide is
load-bearing rather than cosmetic.

**Reassuring result:** the mode is unchanged, i.e. MPO absorbed the pill onto its own plane.

Pair it with a package-power reading (`powercfg /srumutil`, HWiNFO's CPU/GPU package power, or Intel
Power Gadget on Intel parts) across the same two conditions. **Alarming:** a difference above ~1 W.
Below ~0.2 W is noise.

Also worth one line: call `IDXGIOutput6::CheckHardwareCompositionSupport`
([docs](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_6/nf-dxgi1_6-idxgioutput6-checkhardwarecompositionsupport))
on the target display once, to know whether the machine has overlay planes at all. It describes the
output, not our window, but it tells you which of the three §4d outcomes is even available.

### 8b. The wakeup cost

`powercfg /energy /duration 60` with the machine idle, screen on, nothing focused. Two runs: Draft
running with a resident pill, and Draft not running.

- Read the **"Platform Timer Resolution"** section. **Alarming:** Draft appears in the list of
  processes requesting a period below the default. It should not — nothing in the dependency tree
  calls `timeBeginPeriod` (§5b) — and if it does, something changed.
- Read **"CPU Utilization: Individual process"**. **Alarming:** Draft above ~0.5% average. Expected:
  ≲0.1% (§5d).

For a per-second view, Windows Performance Recorder (`wpr -start CPU -start Power`) → Windows
Performance Analyzer, and look at the **"Timer Resolution"** and **"CPU Usage (Precise)"** graphs
filtered to `draft.exe`. The number to watch is *context switches per second* for the winit thread:
**expected ~20/s; alarming above ~60/s** (that would mean the tick is firing faster than intended, or
`is_active()` has regressed per R1).

Battery: `powercfg /batteryreport` after two matched 2-hour idle sessions (pill resident vs. Draft
not running), screen on, same brightness. **Alarming:** more than ~1% additional drain over 2 hours.

### 8c. The static-surface claim

Cheap and worth doing once, because the whole §2 argument rests on it.

Instrument `LayeredSurface::present` with a counter and a `tracing` line. Launch Draft, leave it idle
for 10 minutes, then exercise: lock and unlock the machine, sleep and wake the monitor, change
resolution, change DPI scaling, unplug and replug an external display, connect over RDP and
disconnect.

**Expected:** exactly one push at startup, and then one per event in the R4 list — **zero pushes
during the idle 10 minutes**. **Alarming:** any push not attributable to a state change, and above
all a nonzero count during a quiet minute.

Separately: after each of those events, is the pill still visible and correct *without* a re-push?
That is the empirical answer to §3's undocumented list — comment out the re-push for one event at a
time and look at the screen.

### 8d. DWM composition rate

`DwmGetCompositionTimingInfo(NULL, …)` sampled once a second, logging `cFramesDisplayed` and
`cPixelsDrawn` ([DWM_TIMING_INFO](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ns-dwmapi-dwm_timing_info)).
It is system-wide, so it only tells you whether DWM is composing at all — which is exactly the
question in 8a, from a second angle and without installing anything. **Alarming:** `cFramesDisplayed`
advancing at the refresh rate while a fullscreen app is supposedly independently flipping.

---

## 9. Unverified

Stated plainly rather than guessed. Where a commonly-repeated claim has no primary source, it is
listed here rather than cited.

1. **That `WM_DISPLAYCHANGE` is the message the `UpdateLayeredWindow` "re-create your layered
   windows" sentence refers to.** The published sentence has lost its message name (§3). The
   inference is strong — it is the only message carrying colour depth — but it is an inference.
2. **Whether `WM_DPICHANGED` invalidates a layered surface.** No Microsoft doc discusses layered
   windows and DPI together. The PMv2 "never bitmap scaled" rule means we must re-render regardless,
   so the design does not depend on the answer.
3. **Whether session lock/unlock, RDP connect/disconnect, monitor sleep/wake, or GPU device reset /
   TDR invalidate a layered surface.** Nothing found in either direction. R4 re-pushes on all of them
   defensively; 8c settles it empirically.
4. **"Layered window surfaces are limited to screen size."** Widely repeated; **not found** in
   `UpdateLayeredWindow`, `Window Features`, or the archived MSDN paper. The only sizing statement is
   the qualitative "should be as small as possible".
5. **"Layered window bitmaps live in system memory, not video memory."** The DWM overview says
   redirection surfaces are in video memory, which arguably contradicts the folklore for Win7+, but
   no statement specific to layered surfaces exists.
6. **A byte-cost figure for DWM's per-window redirection surface.** The mechanism is documented; the
   numbers never are. §4a's table is width × height × 4 arithmetic, a lower bound.
7. **"DWM does no per-frame work for a static or occluded window."** Not documented. The only
   adjacent statement is that DWM "can go to sleep" once an app is DirectFlipped — which is about the
   *app*, not about us.
8. **The exact conditions under which a topmost overlay breaks independent flip / MPO handoff.**
   Microsoft documents three possible outcomes and points at PresentMon. Any claim that "a topmost
   window forces composed mode" is folklore and the docs mildly contradict it; any claim that it is
   free is equally unsupported. **This is the single most important open question on this ticket.**
9. **How many MPO planes a given display/driver offers, and per-vendor behaviour.** Not documented.
10. **A joules- or watts-per-wakeup figure.** The Timer Coalescing whitepaper gives the mechanism and
    the 32 ms threshold, never a number. The "1 ms timer resolution costs up to 25% battery" figure
    traces only to secondary blogs and is **not** in the whitepaper — do not cite it.
11. **Whether `MsgWaitForMultipleObjectsEx`'s `dwMilliseconds` timeout participates in timer
    coalescing.** No tolerance parameter exists and Learn says nothing either way. Moot here, because
    winit supplies a separate high-resolution waitable timer object (§5b) and waits on that.
12. **Whether DAM suspension applies unchanged on Windows 11 modern standby.** The DAM page is
    explicitly scoped "Clients – Windows 8"; the Windows 11 modern standby pages do not mention it.
    §5d's "the poll costs nothing in modern standby" conclusion inherits that caveat.
13. **How Windows 11 classifies a 36×10 always-on-top toolwindow for the "fully occluded … or
    otherwise invisible to the end user" timer-resolution and QoS heuristics.** This is the one cost
    the word *resident* genuinely introduces (§5e) and it is undocumented.
14. **`GetCursorPos` cost.** Nothing documented — no rate guidance, no caching note. Any claim that
    it is cheap (or expensive) is uncited folklore, including the estimate in §5d.
15. **What Task Manager's "Power usage" column measures.** No Learn page found describing it. A
    `Microsoft.Windows.EventTracing` trace-processor API named `UseEnergyEstimationData` exists, which
    I did not fetch and cannot characterise. Treat the Task Manager column as a smoke alarm, not a
    measurement — hence §8's reliance on `powercfg` and WPA instead.
16. **That the R10 proximity ladder actually feels good.** A 250 ms poll when the cursor is far means
    up to 250 ms before the pill starts expanding as the cursor arrives — but the cursor has to cross
    the "near" band first, which drops the period to 50 ms before it reaches the pill. **[inference]**
    that this is imperceptible; it wants a prototype, not more reading.

---

## 10. `windows` crate notes

Beyond what #19 and #20 already list, a resident pill's guards need:

| Symbol | Module | Cargo feature |
|---|---|---|
| `RegisterPowerSettingNotification`, `UnregisterPowerSettingNotification`, `POWERBROADCAST_SETTING`, `PBT_POWERSETTINGCHANGE`, `PBT_APMSUSPEND`, `PBT_APMRESUMEAUTOMATIC`, `WM_POWERBROADCAST`, `DEVICE_NOTIFY_WINDOW_HANDLE` | `windows::Win32::UI::WindowsAndMessaging` / `Win32::System::Power` | `Win32_System_Power` (new) |
| `GUID_SESSION_DISPLAY_STATUS`, `GUID_SESSION_USER_PRESENCE`, `GUID_POWER_SAVING_STATUS`, `MONITOR_DISPLAY_STATE` | `windows::Win32::System::Power` | `Win32_System_Power` (new) |
| `WTSRegisterSessionNotification`, `WTSUnRegisterSessionNotification`, `WM_WTSSESSION_CHANGE`, `WTS_SESSION_LOCK`, `WTS_SESSION_UNLOCK` | `windows::Win32::System::RemoteDesktop` | `Win32_System_RemoteDesktop` (new) |
| `SetProcessInformation`, `PROCESS_POWER_THROTTLING_STATE`, `ProcessPowerThrottling`, `PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION` | `windows::Win32::System::Threading` | `Win32_System_Threading` (already enabled) |
| `GetDpiForWindow` | `windows::Win32::UI::HiDpi` | `Win32_UI_HiDpi` (new — #20 flagged this too) |
| `SetWindowLongPtrW(GWLP_WNDPROC, …)`, `CallWindowProcW` | `windows::Win32::UI::WindowsAndMessaging` | already enabled |
| `ShowWindow`, `SW_HIDE`, `SW_SHOWNOACTIVATE` | `windows::Win32::UI::WindowsAndMessaging` | already enabled |

`WTSRegisterSessionNotification` needs `wtsapi32.lib`, which windows-rs links via the
`Win32_System_RemoteDesktop` feature.

---

## Sources

Microsoft Learn:

- [Window Features — Layered Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features)
- [UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)
- [SetLayeredWindowAttributes](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setlayeredwindowattributes)
- [Layered Windows (Gorokhovsky & Amadio, archived MSDN paper)](https://learn.microsoft.com/en-us/previous-versions/ms997507(v=msdn.10))
- [WM_DISPLAYCHANGE](https://learn.microsoft.com/en-us/windows/win32/gdi/wm-displaychange)
- [WM_DWMCOMPOSITIONCHANGED](https://learn.microsoft.com/en-us/windows/win32/dwm/wm-dwmcompositionchanged)
- [WM_THEMECHANGED](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-themechanged)
- [High DPI Desktop Application Development on Windows](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows)
- [Desktop Window Manager overview](https://learn.microsoft.com/en-us/windows/win32/dwm/dwm-overview)
- [DwmGetCompositionTimingInfo](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetcompositiontiminginfo)
- [DWM_TIMING_INFO](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ns-dwmapi-dwm_timing_info)
- [DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)
- [For best performance, use DXGI flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)
- [DXGI flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-flip-model)
- [DXGI_STATUS](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-status)
- [IDXGIOutput6::CheckHardwareCompositionSupport](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_6/nf-dxgi1_6-idxgioutput6-checkhardwarecompositionsupport)
- [timeBeginPeriod](https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod)
- [SetProcessInformation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessinformation)
- [CreateWaitableTimerExW](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw)
- [SetCoalescableTimer](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setcoalescabletimer)
- [SetTimer](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-settimer)
- [KeSetCoalescableTimer](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-kesetcoalescabletimer)
- [High-Resolution Timers (kernel)](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/high-resolution-timers)
- [MsgWaitForMultipleObjectsEx](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-msgwaitformultipleobjectsex)
- [Desktop Activity Moderator](https://learn.microsoft.com/en-us/windows/win32/w8cookbook/desktop-activity-moderator)
- [Power Setting GUIDs](https://learn.microsoft.com/en-us/windows/win32/power/power-setting-guids)
- [RegisterPowerSettingNotification](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerpowersettingnotification)
- [RegisterSuspendResumeNotification](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registersuspendresumenotification)
- [WM_WTSSESSION_CHANGE](https://learn.microsoft.com/en-us/windows/win32/termserv/wm-wtssession-change)
- [WTSRegisterSessionNotification](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsregistersessionnotification)
- [GetSystemMetrics](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getsystemmetrics)
- [GetCursorPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getcursorpos)
- [SetWinEventHook](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)
- [Out-of-Context Hook Functions](https://learn.microsoft.com/en-us/windows/win32/winauto/out-of-context-hook-functions)
- [Modern Standby Wake Sources](https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/modern-standby-wake-sources)

Microsoft-published, not on Learn:

- Windows Timer Coalescing whitepaper (Jan 2009), linked from the `SetCoalescableTimer` reference —
  `https://download.microsoft.com/download/9/C/5/9C5B2167-8017-4BAE-9FDE-D599BAC8184A/TimerCoal.docx`

Tool documentation (authoritative for the tool, not a Microsoft specification):

- [PresentMon README-ConsoleApplication.md](https://raw.githubusercontent.com/GameTechDev/PresentMon/main/README-ConsoleApplication.md) — present-mode definitions
- [PresentMon](https://github.com/GameTechDev/PresentMon)

Crate sources (read locally):
`winit-0.30.13` (`src/platform_impl/windows/event_loop.rs` — `create_high_resolution_timer` :637,
`set_high_resolution_timer` :664, `wait_for_messages` :690-762), `windows-0.58.0`.
Grepped for `timeBeginPeriod` / `timeGetDevCaps` / `NtSetTimerResolution` across `winit-0.30.13`,
`cpal-0.15.3`, `global-hotkey-0.6.4`, `tray-icon-0.19.3`, `eframe-0.29.1` — no hits.

This repo: `src/pill/window.rs`, `src/pill/mod.rs`, `src/pill/render.rs`, `src/main.rs`
(`App::about_to_wait` :349-413, `PillAdapter` :520-633), `src/hotkey.rs`, `src/tray.rs`,
`Cargo.toml`.

Sibling research: `docs/research/click-through-toggle.md` (#20, branch
`research/click-through-toggle`), `docs/research/fullscreen-detection.md` (#19, branch
`research/fullscreen-detection`).
