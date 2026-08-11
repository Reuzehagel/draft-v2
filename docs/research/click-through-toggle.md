# Toggling click-through (`WS_EX_TRANSPARENT`) on the pill's layered window

Research for making the resident pill clickable only while it is hover-expanded, and for detecting
that hover at all.

Status: research only — nothing implemented. Every Win32 claim cites Microsoft Learn. Claims about
winit and the `windows` crate are checked against the vendored sources of the versions this repo
resolves to (`winit-0.30.13`, `windows-0.58.0` — see `Cargo.toml`). Anything that is inference
rather than documented behaviour is labelled **[inference]**; anything I could not establish at all
is in §10.

---

## 1. Recommendation

**Flip `WS_EX_TRANSPARENT` with a bare `SetWindowLongPtrW(hwnd, GWL_EXSTYLE, …)` on the winit
event-loop thread. Do not call `SetWindowPos` afterwards. Do not use winit's
`Window::set_cursor_hittest` — it is the trap, not the shortcut.**

The hazard recorded at `src/pill/window.rs:231` is specific to `WS_EX_LAYERED`, and it is
documented: clearing and re-setting the layering style bit is the *sanctioned recovery* from a
mode conflict, and it necessarily takes the window out of per-pixel-alpha mode for the gap.
`WS_EX_TRANSPARENT` carries no such hazard — it is a hit-testing/paint-ordering attribute, not the
bit that owns the layered surface. Nothing in the layered-window documentation ties the surface's
lifetime to it.

Concretely:

| Step | Call | Why |
|---|---|---|
| Expand | `SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !WS_EX_TRANSPARENT)` | pill becomes a mouse target |
| Collapse | `SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex \| WS_EX_TRANSPARENT)` | back to click-through |
| Never | `SetWindowPos(..., SWP_FRAMECHANGED, ...)` | not required for this bit, and it is the one call that could repaint (§4) |
| Never | `winit::Window::set_cursor_hittest(..)` | it clears `WS_EX_LAYERED` too (§5) — the exact documented flicker |

Read-modify-write the current ex-style each time (`GetWindowLongPtrW` then mask), never a
literal — the pill's other four ex-styles must survive the flip.

Hover detection: **`GetCursorPos` + a cached `GetWindowRect`, checked on every existing event-loop
wakeup** (50 ms idle / ~33 ms while the pill is active). No new thread, no new timer, no hook. Both
APIs return physical pixels in virtual-screen coordinates for this process, so they are directly
comparable (§7).

**One guard is load-bearing for the standing "pill must never take focus" constraint:** while
`WS_EX_TRANSPARENT` is *off*, `WS_EX_NOACTIVATE` has a documented loophole — see §6. That guard, not
the flicker question, is the real risk in this design.

---

## 2. What `WS_EX_TRANSPARENT` actually is

The [Extended Window Styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles)
reference describes only the *painting* half:

> **WS_EX_TRANSPARENT** — The window should not be painted until siblings beneath the window (that
> were created by the same thread) have been painted. The window appears transparent because the
> bits of underlying sibling windows have already been painted.

That description says nothing about the mouse, and taken alone it would not support this design at
all. The hit-testing half is documented separately, on the Layered Windows section of
[Window Features](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features), and it is
unambiguous and exactly on point for our window:

> Hit testing of a layered window is based on the shape and transparency of the window. This means
> that the areas of the window that are color-keyed or whose alpha value is zero will let the mouse
> messages through. However, if the layered window has the **WS_EX_TRANSPARENT** extended window
> style, the shape of the layered window will be ignored and the mouse events will be passed to
> other windows underneath the layered window.

Three things follow, and all three matter for the design:

1. **For a layered window the style is a clean on/off switch for mouse targeting.** On → all mouse
   events pass through. Off → hit testing falls back to shape and alpha.
2. **The pill's rounded corners stay click-through for free.** With the style cleared, hit testing
   is "based on the shape and transparency" — alpha-0 pixels still let mouse messages through. The
   design does not need a region, a `SetWindowRgn`, or a manual corner test: `UpdateLayeredWindow`
   already publishes the alpha channel that the hit test reads. This also means the *nub* state,
   whose fill is mostly transparent at the edges, will behave sanely if it is ever made clickable.
3. **The style is read at hit-test time, not baked into the window at creation.** Nothing in this
   text or in [`UpdateLayeredWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)
   suggests the surface is rebuilt when the style changes.

Raymond Chen's
[Like the cake, `WS_EX_TRANSPARENT` is a lie, or at least not the entire truth](https://devblogs.microsoft.com/oldnewthing/20121217-00/?p=5823)
(near-primary — Chen is the Win32 shell maintainer, but this is a blog, not a spec) confirms the
style is overloaded across painting and hit-testing, and lists cases where the *painting* half is
inert: windows that are not siblings, and windows in different processes. Those caveats are about
painting order between cooperating sibling windows and do not touch the layered-window hit-testing
rule quoted above — which is the only rule this design relies on.

The enumeration Chen defers to is
[WindowFromPoint, ChildWindowFromPoint, RealChildWindowFromPoint, when will it all end?](https://devblogs.microsoft.com/oldnewthing/20101230-00?p=11873),
and it matters for §7: **different hit-test entry points define "transparent" differently.**
`WindowFromPoint` treats a window as transparent when it answers `HTTRANSPARENT` to
`WM_NCHITTEST` (and treats cross-process windows as opaque regardless); it is
`ChildWindowFromPointEx`/`RealChildWindowFromPoint` that key off the `WS_EX_TRANSPARENT` *style*.
So the popular claim "`WindowFromPoint` skips `WS_EX_TRANSPARENT` windows" is, at best, about the
wrong function.

---

## 3. Does flipping it drop the per-pixel-alpha surface?

**No — and the hazard the codebase already knows about is a different, documented one.**

The comment at `src/pill/window.rs:231` says re-arming `WS_EX_LAYERED` "briefly drops the window out
of per-pixel-alpha mode". That is not a quirk; it is the documented mode-conflict recovery, from the
same Layered Windows section:

> Please note that after **SetLayeredWindowAttributes** has been called, subsequent
> **UpdateLayeredWindow** calls will fail until the layering style bit is cleared and set again.

So: `SetLayeredWindowAttributes` and `UpdateLayeredWindow` are mutually exclusive modes; something
calling the former puts the window in redirected mode; the *only* documented way back is to clear
and re-set `WS_EX_LAYERED`. Clearing that bit is by definition "this is no longer a layered window",
which is why a frame of bare rectangle escapes. The `rearm_layered` + retry dance in `present()` is
the correct shape, and the comment is right about the cost.

`WS_EX_TRANSPARENT` is not that bit. The surface is owned by `WS_EX_LAYERED` — the
[`UpdateLayeredWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)
reference says the `hWnd` parameter is "A handle to a layered window. A layered window is created by
specifying **WS_EX_LAYERED**", and names no other style. There is no documented interaction between
`WS_EX_TRANSPARENT` and the layered surface's contents, position, or validity.

**[inference]** Because the design's flip is a pure read-modify-write that touches only
`WS_EX_TRANSPARENT`, `WS_EX_LAYERED` is never momentarily absent, so the mechanism behind the
observed flicker cannot fire. I found no primary source stating "changing `WS_EX_TRANSPARENT` does
not repaint a layered window" — the argument is that no source connects them, plus the local
evidence in §4.

One real interaction to keep in mind: the flip is one more `SetWindowLongPtrW(GWL_EXSTYLE)` writer
on a window that already has two others (`apply_layered_styles`, `rearm_layered`) plus winit itself
(§5). Every writer must read-modify-write, and they must all run on the same thread (§6), or one
will clobber another's bits.

---

## 4. Is `SetWindowPos(SWP_FRAMECHANGED)` required?

**No — and it is the only step in this design that could plausibly cause a visible repaint, so
leave it out.**

The reason to think it might be required is real. [`SetWindowLongPtrW`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw)
Remarks:

> Certain window data is cached, so changes you make using **SetWindowLongPtr** will not take effect
> until you call the [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos) function.

and [`SetWindowPos`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)
Remarks:

> If you have changed certain window data using SetWindowLong, you must call **SetWindowPos** for
> the changes to take effect. Use the following combination for *uFlags*:
> `SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED`.

Neither doc says *which* data is cached. The flag description does:

> **SWP_FRAMECHANGED** (0x0020) — Applies new frame styles set using the SetWindowLong function.
> Sends a [WM_NCCALCSIZE](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-nccalcsize)
> message to the window, even if the window's size is not being changed.

The cached data is the **frame** — the non-client metrics that `WM_NCCALCSIZE` recomputes.
`WS_EX_TRANSPARENT` contributes nothing to the frame: it is consulted live by the hit-test path
(§2), and the pill is `with_decorations(false)` with no non-client area to recalculate anyway.

Two independent supports:

- **This codebase already relies on the bare call working.** `rearm_layered` (`window.rs:318`) flips
  `WS_EX_LAYERED` with two `SetWindowLongPtrW` calls and *no* `SetWindowPos`, and the immediately
  following `UpdateLayeredWindow` retry succeeds — which it could not if the style change were still
  sitting in a cache. Empirical, in-repo, for the neighbouring bit.
- **Note the flag values.** `SWP_FRAMECHANGED` is `0x0020` and `SWP_DRAWFRAME` is also `0x0020` —
  the same bit, documented as "Draws a frame (defined in the window's class description) around the
  window." Asking for a frame redraw on a window whose entire appearance is published out-of-band by
  `UpdateLayeredWindow` is asking for exactly the class of glitch #20 is trying to avoid.

If a future measurement shows the flip genuinely does not take effect without it, the safe form is
`SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW` (plus `SWP_FRAMECHANGED`).
`SWP_NOACTIVATE` is mandatory — without it, "the window is activated and moved to the top of either
the topmost or non-topmost group". `SWP_NOZORDER` keeps `hWndInsertAfter` ignored, so topmost status
is untouched; note the converse trap from the same page — a window *becomes* topmost via
`HWND_TOPMOST` only when `SWP_NOZORDER` is absent, so passing `SWP_NOZORDER` cannot accidentally
demote us. `SWP_NOREDRAW` means "no repainting of any kind occurs", which for a layered window costs
nothing because the next `blit_and_present()` republishes the whole surface anyway
(`UpdateLayeredWindow` "always updates the entire window").

---

## 5. The winit hazard — bigger than the flicker question

Two findings from `winit-0.30.13`'s source, both of which change what the design should do.

### 5a. `set_cursor_hittest` clears `WS_EX_LAYERED` as well

winit exposes exactly the API this design wants — and it is unusable here.
`Window::set_cursor_hittest(bool)` (`src/platform_impl/windows/window.rs:632`) sets the
`WindowFlags::IGNORE_CURSOR_EVENT` flag, and `WindowFlags::to_window_styles`
(`src/platform_impl/windows/window_state.rs:301`) maps it to:

```rust
if self.contains(WindowFlags::IGNORE_CURSOR_EVENT) {
    style_ex |= WS_EX_TRANSPARENT | WS_EX_LAYERED;
}
```

The two bits are ORed together, so turning hit-testing *back on* clears `WS_EX_LAYERED` too — the
precise operation `window.rs:231` documents as dropping the surface for a frame. **Calling
`set_cursor_hittest(true)` would reproduce the pill-flashes-as-a-rectangle bug on purpose.**

### 5b. Any winit window-state change overwrites *all* our ex-styles

`WindowFlags::apply_diff` (`window_state.rs:396`) does:

```rust
SetWindowLongW(window, GWL_STYLE,   style    as i32);
SetWindowLongW(window, GWL_EXSTYLE, style_ex as i32);
// then SetWindowPos(SWP_NOZORDER|SWP_NOMOVE|SWP_NOSIZE|SWP_FRAMECHANGED [|SWP_NOACTIVATE])
```

It writes the ex-style computed purely from winit's own `WindowFlags` — an **absolute write, not a
read-modify-write**. `to_window_styles` starts from `WS_EX_WINDOWEDGE | WS_EX_ACCEPTFILES` and adds
only bits winit itself models; **`WS_EX_NOACTIVATE` and `WS_EX_TOOLWINDOW` are not among them**
(they appear nowhere in winit's `window.rs`/`window_state.rs` — `set_skip_taskbar` uses
`ITaskbarList`, not `WS_EX_TOOLWINDOW`). So every ex-style bit `apply_layered_styles` set behind
winit's back — `WS_EX_LAYERED`, `WS_EX_TRANSPARENT`, `WS_EX_NOACTIVATE`, `WS_EX_TOOLWINDOW` — is
wiped whenever winit changes any window flag: `set_visible`, `set_window_level`, `set_decorations`,
`set_resizable`, minimise/maximise.

**This already happens on every pill creation.** `PillAdapter::set_mode` in `src/main.rs` runs
`apply_layered_styles` → first `UpdateLayeredWindow` → **`pw.show()`, which is
`Window::set_visible(true)`** — i.e. the ex-style is established and then immediately overwritten,
with `WS_EX_NOACTIVATE` among the casualties. The cheapest fix is to stop calling winit's
visibility API on the pill at all and use `ShowWindow(hwnd, SW_SHOWNOACTIVATE)` on the raw HWND,
re-asserting the ex-style right after. That is worth doing regardless of #20 — it removes the
loudest source of the `E_INVALIDARG` retry *and* closes a focus hole.

**This retires a wrong hypothesis in the existing code comment.** `window.rs:236` blames winit for
re-entering `SetLayeredWindowAttributes`. It does not: `SetLayeredWindowAttributes` appears nowhere
in winit 0.30.13's source (grepped; the only layered references are `WS_EX_LAYERED` in
`event_loop.rs:940` for winit's internal helper window and in `window_state.rs`). The real mechanism
behind the observed `E_INVALIDARG` is almost certainly 5b — winit's `apply_diff` dropping
`WS_EX_LAYERED` entirely, after which `UpdateLayeredWindow` cannot succeed because the window is no
longer layered. **[inference]** — the failure mode is the same and the retry-with-re-arm fix works
for both causes, so the existing code is correct either way; only the comment is wrong. Worth a
follow-up ticket to correct it, and worth re-asserting all five ex-styles (not just re-arming
`WS_EX_LAYERED`) in the failure path.

Design consequence: **the pill's ex-style is jointly owned by us and by winit, and winit wins any
race.** The hover flip must therefore be idempotent and re-derivable from the pill's own state — set
it from the Pill core's current mode on every relevant transition rather than assuming the last
write survived. It is cheap: `GetWindowLongPtrW` + a compare + a conditional `SetWindowLongPtrW`.

---

## 6. Thread affinity, and the focus loophole

### Thread

The flip must run **on the winit event-loop thread that created the HWND**.

- [`SetWindowLongPtrW`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw)
  is UIPI-gated ("fails if the process that owns the window … is at a higher process privilege in
  the UIPI hierarchy") — a *process* restriction, not a thread one. Cross-thread in-process is not
  documented as failing.
- `SetWindowPos` is likewise **not** documented as forbidden cross-thread (I looked; the sentence
  often quoted to that effect is not on Learn — see §10). What *is* documented is that it blocks:
  `SWP_ASYNCWINDOWPOS` exists precisely because "If the calling thread and the thread that owns the
  window are attached to different input queues, the system posts the request to the thread that
  owns the window. This prevents the calling thread from blocking its execution."
- winit agrees: its own `set_cursor_hittest` wraps the work in
  `self.thread_executor.execute_in_thread(…)` rather than calling inline.

In practice this is free — the hover poll already belongs in `App::about_to_wait`, which *is* that
thread.

### Focus: the guard that actually matters

Clearing `WS_EX_TRANSPARENT` does not by itself endanger focus. `WS_EX_NOACTIVATE` is documented as:

> A top-level window created with this style does not become the foreground window when the user
> clicks it. The system does not bring this window to the foreground when the user minimizes or
> closes the foreground window.
> — [Extended Window Styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles)

So a click on the expanded pill delivers `WM_LBUTTONDOWN` without activation, which is exactly what
the design wants. **But there is a documented hole.** Raymond Chen,
[My window has the `WS_EX_NOACTIVATE` extended style, but it got activated anyway](https://devblogs.microsoft.com/oldnewthing/20240919-00/?p=110283)
(near-primary): when the user has enabled *Activate a window by hovering over it*, the
active-window-tracking path **does not check `WS_EX_NOACTIVATE`** — it sends
[`WM_MOUSEACTIVATE`](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-mouseactivate)
instead, and the fix is to return `MA_NOACTIVATE` (or `MA_NOACTIVATEANDEAT`) from it.

For Draft this is not cosmetic: hover-to-activate on the pill would steal foreground from the window
the user is about to paste into, which is the one failure the product cannot absorb.

- While `WS_EX_TRANSPARENT` is set (the resident state) the pill is not a mouse target at all, so
  the tracking path cannot see it. The exposure exists **only** in the hover-expanded window — i.e.
  exactly the moment this design creates.
- winit 0.30.13 does **not** handle `WM_MOUSEACTIVATE` (grepped: no occurrences), so it falls to
  `DefWindowProc`, which returns `MA_ACTIVATE`.
- Returning `MA_NOACTIVATE` therefore requires subclassing the wndproc
  (`SetWindowLongPtrW(GWLP_WNDPROC, …)` + `CallWindowProc`) — winit offers no hook for it. That is a
  real cost and should be weighed in the implementation ticket rather than assumed free.
- Also note winit calls `SetCapture` on `WM_LBUTTONDOWN` (`event_loop.rs:1786`). Mouse capture is
  not activation and does not move foreground, but it is another behaviour the pill inherits by
  becoming a mouse target.

---

## 7. Hit-test mechanics with `WS_EX_TRANSPARENT` set

### Coordinate space — the two APIs are directly comparable here

[`GetCursorPos`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getcursorpos)
returns "the position of the mouse cursor, in screen coordinates" and says nothing about DPI.
[`GetWindowRect`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect)
says outright: **"GetWindowRect is virtualized for DPI."** Whether "virtualized" changes anything
depends entirely on the calling thread's DPI awareness, and
[High DPI Desktop Application Development](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows)
spells out the mechanism:

> When an HWND or process is running as either DPI unaware or system DPI aware, it can be bitmap
> stretched by Windows. When this happens, Windows scales and converts DPI-sensitive information
> from some APIs to the coordinate space of the calling thread. … Be aware that if you call any
> system API from a DPI-unaware or system-DPI-aware thread context, the return value might be
> virtualized.

and for Per-Monitor v2, which is what this process runs as:

> Registering a process as running in PMv2 awareness mode results in: … 2. The application seeing
> the raw pixels of each display 3. The application never being bitmap scaled by Windows

**What this process actually is** (checked, not assumed): there is no `build.rs`, no `*.manifest`,
and no `winres`/`embed-resource` dependency in `Cargo.toml` — so no manifest sets DPI awareness.
winit sets it at runtime: `EventLoop::new` calls `become_dpi_aware()`
(`winit-0.30.13/src/platform_impl/windows/event_loop.rs:199`), which calls
`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`, falling back to
`PER_MONITOR_AWARE` if unavailable (`platform_impl/windows/dpi.rs:20-31`).

**Conclusion:** the process is PMv2, so no virtualization happens on any thread, and both
`GetCursorPos` and `GetWindowRect` return **physical device pixels in virtual-screen coordinates**.
A raw `PtInRect`-style comparison is correct. Do **not** convert through `PillWindow::scale` — that
factor exists to scale the *drawing*, and mixing it into the hit test would double-apply DPI.

Corollaries:

- `GetPhysicalCursorPos` is unnecessary. It "Retrieves the position of the cursor in physical
  coordinates" — under PMv2 that is what `GetCursorPos` already returns.
- Do not cache a scale factor and reuse it. The DPI guidance's own pitfall list calls out exactly
  this: "It is a very common practice to cache font sizes and DPI values at process initialization.
  When updating an application to become per-monitor DPI aware, DPI-sensitive data must be
  reevaluated whenever a new DPI is encountered." `PillWindow::scale` is captured once in `create()`
  from the primary monitor and never updated — a latent bug for the pill generally, and it must not
  become a dependency of the hit test.
- If any future code runs the hit test off a thread whose DPI context was changed with
  `SetThreadDpiAwarenessContext`, this reasoning breaks. Keep it on the event-loop thread.

### Rect caching and DPI changes

`GetWindowRect` is a syscall per poll; the pill's rect changes only when the pill moves, resizes
(nub ↔ recording ↔ expanded — all of which the Pill core drives), or the monitor DPI changes.

- Cache the rect; invalidate it on every geometry transition the Pill core produces.
- Invalidate it on `WM_DPICHANGED` — winit surfaces this as `WindowEvent::ScaleFactorChanged`.
  [`GetDpiForWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdpiforwindow)
  returns "The DPI of the monitor where the window is located" for a per-monitor-aware window and is
  the right way to re-derive scale at that moment.
- **[inference]** Simplest correct policy: just call `GetWindowRect` every poll and skip the cache
  entirely. At 20 Hz idle that is 20 user32 calls/second, which is noise next to the 30 Hz
  `UpdateLayeredWindow` the pill already does when active. Caching is an optimisation, not a
  requirement — and an uncached read cannot go stale.

### Cadence

**Poll on every existing `about_to_wait` wakeup: 50 ms idle, ~33 ms while the pill is active.** No
new timer and no new thread — the same argument that carried #19's 1 s fullscreen poll, only this
one wants every wakeup rather than one in twenty, because hover feedback is a direct response to the
user's hand and 1 s would feel broken.

50 ms worst-case hover latency sits under the ~100 ms threshold at which a UI response stops reading
as instantaneous, and the expand animation itself is 90–160 ms (#18), so the poll is not the
dominant term. Each poll is two user32 calls and an integer rect test.

If hover-in ever feels laggy, the lever is to drop the idle wait to 25 ms rather than to add a
thread or a hook.

### Why not `WindowFromPoint` or `GetCursorInfo`

[`WindowFromPoint`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-windowfrompoint)
is the wrong tool, twice over:

- Its documented Remarks mention only hidden and disabled windows — **the reference says nothing
  about `WS_EX_TRANSPARENT` at all**, and per Chen's enumeration (§2) the style-based rule belongs
  to `ChildWindowFromPointEx`/`RealChildWindowFromPoint`; `WindowFromPoint` keys off an
  `HTTRANSPARENT` answer to `WM_NCHITTEST` instead. The widely-repeated "it skips transparent
  windows" is about the wrong function. Do not build on it.
- More decisively, even if it does skip us: while the pill is transparent, `WindowFromPoint` over
  the pill returns the window *underneath*. That answers "what is beneath the cursor", not "is the
  cursor over the pill" — the question the design is actually asking. And once the style is cleared
  it would return the pill, making it useless as a *trigger* for clearing the style. It is
  circular.

`GetCursorInfo`/[`CURSORINFO`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-cursorinfo)
gives `ptScreenPos` (same value as `GetCursorPos`) plus `flags`: `0` = hidden, `CURSOR_SHOWING`,
`CURSOR_SUPPRESSED` ("the system is not drawing the cursor because the user is providing input
through touch or pen instead of the mouse"). It adds one genuinely useful thing — **don't expand the
pill when there is no visible cursor**, which suppresses phantom hovers during touch/pen input and
while a game or video player has hidden the cursor. Cheap and worth taking as a cheap secondary
guard; it does not replace the rect test.

[`TrackMouseEvent`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-trackmouseevent)
/`WM_MOUSELEAVE` and `WM_MOUSEMOVE` are unavailable in the transparent state for the same reason
everything else is: mouse events are "passed to other windows underneath the layered window" (§2),
so the pill's wndproc never sees them, and `WM_MOUSEHOVER`/`WM_MOUSELEAVE` are defined relative to
a client area the cursor never entered.

Once the style is cleared, that reverses — winit delivers
`CursorMoved`/`CursorEntered`/`CursorLeft` and `MouseInput` normally (`event_loop.rs:1643`,
`:1781`). So the clean shape is **poll to enter the expanded state; use real events while
expanded**. Keep the poll running as the exit backstop rather than trusting `CursorLeft` alone,
since §5b means the style can be flipped out from under the event stream at any moment.

---

## 8. Hazards and guards

| Hazard | Guard |
|---|---|
| **Negative coordinates on multi-monitor.** "When the primary monitor is not in the upper left of the virtual screen, parts of the virtual screen have negative coordinates. … all applications should be designed to work with negative coordinates" ([The Virtual Screen](https://learn.microsoft.com/en-us/windows/win32/gdi/the-virtual-screen)); reinforced by [WM_NCHITTEST](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-nchittest) — "Systems with multiple monitors can have negative x- and y- coordinates, and LOWORD and HIWORD treat the coordinates as unsigned quantities" | Keep everything `i32`. `POINT`/`RECT` are already signed; never cast to `u32`/`usize` en route. The pill's own `create()` already computes `x`/`y` as `i32` from `monitor_pos` — stay consistent. |
| **Exclusive bottom-right in `RECT`.** "the bottom-right coordinates of the returned rectangle are exclusive … the pixel at (right, bottom) lies immediately outside" ([GetWindowRect](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect)) | Test `x >= left && x < right && y >= top && y < bottom`. Off-by-one here is a one-pixel dead band along two edges — annoying and hard to spot. |
| **Something on top of the pill.** The pill is `WS_EX_TOPMOST`, but so are other topmost windows, and z-order among them is not ours to know. A naive rect test expands the pill under a window that covers it. | No clean primary-source answer (§10). Practical mitigations: reuse #19's fullscreen check to suppress hover entirely while a fullscreen app owns the pill's monitor, and gate on `CURSOR_SHOWING`. Accept the residual case (a topmost overlay from another app sitting exactly over an 86×42 strip 80 px above the taskbar) as rare. |
| **DPI change at runtime.** Monitor scale change, dock/undock, remote-desktop reconnect. | Invalidate the cached rect on `ScaleFactorChanged`/`WM_DPICHANGED`, or don't cache (§7). Also re-derive `PillWindow::scale`, which today never updates. |
| **winit clobbering the ex-style.** §5b. | Re-derive and re-assert the full ex-style from the Pill core's mode rather than trusting the last write; extend the `present()` failure path to re-assert all five bits, not just `WS_EX_LAYERED`. |
| **Focus theft via hover-activation.** §6. | Subclass to return `MA_NOACTIVATE` from `WM_MOUSEACTIVATE`, or verify empirically that the setting is off/irrelevant before shipping. Do not assume `WS_EX_NOACTIVATE` alone covers it. |
| **Style flip racing the render thread.** `present()` may call `rearm_layered` concurrently with a hover flip if they ever land on different threads. | Both on the event-loop thread (§6). Read-modify-write in both. |

---

## 9. What the design should *not* do

- **Do not use `winit::Window::set_cursor_hittest`.** §5a — it clears `WS_EX_LAYERED` with the
  transparent bit and reintroduces the documented flicker (and drops `WS_EX_NOACTIVATE` besides).
- **Do not call any winit window mutator on the pill after `apply_layered_styles`.**
  `set_visible`, `set_window_level`, `set_decorations`, `set_resizable`, `set_minimized`,
  `set_maximized` all funnel into `apply_diff` and clobber the ex-style (§5b). `pw.show()` is
  currently one of these.
- **Do not call `SetWindowPos(SWP_FRAMECHANGED)` "to be safe".** §4 — it is the only step that can
  repaint, and `SWP_FRAMECHANGED` shares its value with `SWP_DRAWFRAME`. If it is ever needed, it
  must carry `SWP_NOACTIVATE`.
- **Do not write `GWL_EXSTYLE` with a literal.** Read-modify-write, always; four other bits live
  there.
- **Do not toggle `WS_EX_LAYERED` as part of the hover flip**, for any reason.
- **Do not build hover detection on `WindowFromPoint`.** §7 — undocumented for this purpose and
  logically circular.
- **Do not add a `WH_MOUSE_LL` hook, and do not use one to swallow clicks.** See below.
- **Do not scale cursor coordinates by `PillWindow::scale`.** §7 — the process is PMv2; the numbers
  are already physical pixels.
- **Do not poll from a worker thread.** DPI context and style-write thread affinity both argue for
  the event-loop thread.

### The rejected alternative: keep `WS_EX_TRANSPARENT` on permanently

The map (#15) records "Hover is detected by polling the cursor position, not by making the window a
mouse target — `WS_EX_TRANSPARENT` stays on except while expanded." The stricter variant — *never*
clear it, and synthesize clicks from a global input source — is worth naming because it would make
§5 and §6 moot. It should still be rejected.

To detect a click on a window that is not a mouse target you need one of:

- **`WH_MOUSE_LL`.** The [LowLevelMouseProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc)
  reference is a catalogue of reasons not to: "This hook is called in the context of the thread that
  installed it. The call is made by sending a message to the thread that installed the hook.
  Therefore, the thread that installed the hook must have a message loop" — i.e. it runs
  **synchronously on every mouse event system-wide, on winit's pump**, and every user of the machine
  pays for our worst frame. Worse: "The hook procedure should process a message in less time than
  the data entry specified in the **LowLevelHooksTimeout** value … If the hook procedure times out …
  on Windows 7 and later, the hook is silently removed without being called. **There is no way for
  the application to know whether the hook is removed.**" A dictation app that also loads a local
  ONNX model on this thread is a bad candidate for a sub-second hard deadline with silent permanent
  failure. Microsoft's own note steers away: "In most cases where the application needs to use low
  level hooks, it should monitor raw input instead."
  And consuming the click ("may return a nonzero value to prevent the system from passing the
  message to … the target window procedure") means stealing a click from whatever application is
  underneath — unacceptable in an app whose whole contract is not to disturb the focused window.
- **`GetAsyncKeyState(VK_LBUTTON)` polling.** Cannot consume the click at all, so the app under the
  pill receives it too — every pill button press also clicks whatever is behind it. Also aliases:
  at 50 ms a fast click can be missed entirely.
- **Raw input (`WM_INPUT`).** Microsoft's recommended alternative to low-level hooks, but it reports
  *deltas from the device*, not the resolved cursor position, and it also cannot consume the event.

All three lose the thing that makes the flip attractive: with `WS_EX_TRANSPARENT` cleared, Windows
does the hit test against per-pixel alpha for free (§2), routes the click to exactly one window, and
winit hands it to us as an ordinary `MouseInput` event with the right coordinates. The flip's costs
— a wndproc subclass for `WM_MOUSEACTIVATE`, and discipline about winit's ex-style writes — are
bounded and local. The permanent-transparent alternative's costs are unbounded and land on other
applications.

**Recommendation stands: flip it.**

---

## 10. Unverified

Stated plainly rather than guessed:

1. **That flipping `WS_EX_TRANSPARENT` produces no visible artifact on a live `UpdateLayeredWindow`
   surface has not been observed** — only argued from the absence of any documented coupling (§3).
   This is the literal question #20 asks and it wants ten minutes on a real machine: flip the bit at
   30 Hz for a few seconds and watch the pill. If it *does* glitch, the fallback is to leave the
   style permanently set and accept the map's expanded-pill click target being driven some other way
   — which §9 argues is worse, so it would reopen the design.
2. **Whether `SetWindowPos(SWP_FRAMECHANGED)` visibly repaints a layered window** is not documented
   either way. §4 argues it is unnecessary, so the question should stay academic.
3. **What `WindowFromPoint` returns for a `WS_EX_TRANSPARENT` *layered* window specifically** is
   still open. Chen's enumeration (§2) settles which *definition* each function uses, but the
   interaction of the layered alpha channel with `WM_NCHITTEST`/`HTTRANSPARENT` for our window is
   not pinned down anywhere. The design does not depend on it (§7 rejects `WindowFromPoint`
   outright).
4. **The `E_INVALIDARG` attribution in `window.rs:236` is inferred, not proven** (§5b). winit
   demonstrably does not call `SetLayeredWindowAttributes`, and demonstrably does overwrite
   `GWL_EXSTYLE` wholesale — but nobody has caught the actual failure in a debugger. Cheap
   confirmation: log `GetWindowLongPtrW(hwnd, GWL_EXSTYLE)` at the failure site and check whether
   `WS_EX_LAYERED` is missing.
5. **The `WS_EX_LAYERED` re-arm flicker itself is undocumented.** No Microsoft or Chen source
   describes the one-frame bare-rectangle flash; the comment at `window.rs:231` is the only record
   of it. It should stay, but as a labelled empirical observation, not a citation.
6. **The often-quoted "`SetWindowPos` cannot be called by a thread other than the one that created
   the window" is not on Learn.** I looked for it and it is not in the reference. The real
   cross-thread caveat is the blocking behaviour implied by `SWP_ASYNCWINDOWPOS` (§6). The
   same-thread rule still stands on the `WM_STYLECHANGING`/pump-blocking argument, but do not cite
   a sentence that does not exist.
7. **Whether hover-activation (§6) is reachable for a `WS_EX_TOOLWINDOW` + `WS_EX_NOACTIVATE`
   layered window** is not documented; Chen describes the general loophole, not our exact style
   combination. Needs a real box with "Activate a window by hovering over it" turned on.
8. **No reliable way to answer "is the pill the visibly topmost thing at this point"** was found.
   §8 proposes mitigations, not a solution.

---

## 11. `windows` crate notes

Everything except `GetDpiForWindow` is already available under the features `Cargo.toml` enables
(`Win32_Foundation`, `Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`). Verified against the
vendored `windows-0.58.0`:

- `GetCursorPos` — `Win32/UI/WindowsAndMessaging/mod.rs:1115`; signature is
  `unsafe fn GetCursorPos(*mut POINT) -> windows_core::Result<()>` (returns `Result`, not `BOOL`).
- `GetWindowRect` — same module, `:1591`, also `Result<()>`.
- `SetWindowPos` — same module, `:3050`, takes `SET_WINDOW_POS_FLAGS`.
- `GetWindowLongPtrW` / `SetWindowLongPtrW` / `GWL_EXSTYLE` / `WS_EX_TRANSPARENT` — already imported
  in `src/pill/window.rs`.
- `GetDpiForWindow` — `Win32/UI/HiDpi/mod.rs:74`; **requires adding the `Win32_UI_HiDpi` feature.**
  Only needed if the design chooses to re-derive scale itself rather than take winit's
  `ScaleFactorChanged`.

Sketch (shape only — not compiled):

```rust
/// The ex-styles Draft owns and winit does not model. winit's `apply_diff`
/// rewrites GWL_EXSTYLE wholesale from its own flags on every window-state
/// change, so these must be re-asserted, not assumed (§5b).
#[cfg(windows)]
const PILL_EX_BASE: u32 =
    WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;

/// `click_through == true` -> WS_EX_TRANSPARENT set, mouse passes through
/// (the resident state). `false` -> the pill is a hit-test target, and Windows
/// tests against per-pixel alpha, so the rounded corners stay click-through
/// for free (§2).
#[cfg(windows)]
unsafe fn set_pill_ex_styles(hwnd: HWND, click_through: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT,
    };
    let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let mut want = cur | PILL_EX_BASE;
    if click_through {
        want |= WS_EX_TRANSPARENT.0;
    } else {
        want &= !WS_EX_TRANSPARENT.0;
    }
    if want != cur {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want as isize);
    }
    // Deliberately no SetWindowPos: WS_EX_TRANSPARENT is not frame data (§4).
}

/// Called from `App::about_to_wait`, on the winit event-loop thread.
#[cfg(windows)]
fn cursor_over_pill(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};
    unsafe {
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_err() {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        // PMv2 process: both are physical pixels in virtual-screen space (§7).
        // Signed throughout — the virtual screen has negative coordinates (§8).
        // Right/bottom are exclusive (§8).
        p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
    }
}
```

Wiring, in the existing `about_to_wait` path (already the right thread, already 30 Hz while the
pill is up):

```rust
let hovered = cursor_over_pill(pill.hwnd());
if hovered != self.expanded {
    self.expanded = hovered;
    unsafe { set_pill_ex_styles(pill.hwnd(), !self.expanded) };
    // Re-present on the same tick so a clobbered WS_EX_LAYERED can't survive a frame.
}
```

And — independently of #20 — replace `PillWindow::show()`'s `self.window.set_visible(true)` with
`ShowWindow(hwnd, SW_SHOWNOACTIVATE)` on the raw HWND followed by `set_pill_ex_styles`, so winit's
`apply_diff` never runs on the pill at all (§5b).

Note the hit test is a *rect* test, not a shape test. Windows will do the alpha-accurate hit test itself
once the style is cleared (§2), so the rect test only decides *when to clear it* — a few pixels of
slop at the rounded corners means the pill may expand a moment early, never that a click lands
wrong.

---

## Sources

Microsoft Learn:

- [Extended Window Styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles)
- [Window Features — Layered Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features)
- [SetWindowLongPtrW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw)
- [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)
- [UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)
- [WM_NCCALCSIZE](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-nccalcsize)
- [WM_NCHITTEST](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-nchittest)
- [WM_DPICHANGED](https://learn.microsoft.com/en-us/windows/win32/hidpi/wm-dpichanged)
- [TrackMouseEvent](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-trackmouseevent)
- [SetWindowsHookExW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexw)
- [WM_MOUSEACTIVATE](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-mouseactivate)
- [GetCursorPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getcursorpos)
- [GetPhysicalCursorPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getphysicalcursorpos)
- [GetCursorInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getcursorinfo)
- [CURSORINFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-cursorinfo)
- [GetWindowRect](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect)
- [WindowFromPoint](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-windowfrompoint)
- [GetDpiForWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdpiforwindow)
- [High DPI Desktop Application Development on Windows](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows)
- [The Virtual Screen](https://learn.microsoft.com/en-us/windows/win32/gdi/the-virtual-screen)
- [LowLevelMouseProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc)
- [Raw Input](https://learn.microsoft.com/en-us/windows/win32/inputdev/raw-input)

Near-primary (The Old New Thing — Raymond Chen; blog, not specification):

- [Like the cake, WS_EX_TRANSPARENT is a lie, or at least not the entire truth](https://devblogs.microsoft.com/oldnewthing/20121217-00/?p=5823)
- [WindowFromPoint, ChildWindowFromPoint, RealChildWindowFromPoint, when will it all end?](https://devblogs.microsoft.com/oldnewthing/20101230-00?p=11873)
- [My window has the WS_EX_NOACTIVATE extended style, but it got activated anyway](https://devblogs.microsoft.com/oldnewthing/20240919-00/?p=110283)

Crate sources (read locally, not blog posts):
`winit-0.30.13` (`src/platform_impl/windows/{window.rs, window_state.rs, event_loop.rs, dpi.rs, util.rs}`),
`windows-0.58.0` (`src/Windows/Win32/UI/{WindowsAndMessaging,HiDpi}/mod.rs`).

This repo: `src/pill/window.rs`, `src/pill/mod.rs`, `src/main.rs` (`App::about_to_wait`),
`Cargo.toml` (no `build.rs`, no manifest, no `winres`/`embed-resource`).
