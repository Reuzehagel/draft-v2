# The pill

## Looking and measuring

To *look* at the pill without launching anything:

```
cargo test -- --ignored pill::preview
```

writes every mode (over a light and a dark desktop) and every transition (as a filmstrip) to `target/pill-preview/`, through the same `Geom`s and renderer the real window uses. Reach for it whenever you touch `geom.rs` or `render.rs` — it catches what unit tests don't, and has already caught a conceal that left its bar row behind. It cannot show timing, so how a transition *feels* is still a question for the running app.

What a frame *costs* is `cargo test --release -- --ignored pill::bench --nocapture`: median time and allocations per animated frame, per mode and scale. Run it before and after any change to `render.rs`, `surface.rs` or the meter; commit ec51a1c (#106) records the current baseline.

## Rules

- The resident pill is on screen doing nothing most of the time, so it must cost nothing: a settled pill asks for no frames, and the system maintains the layered surface. `PillAdapter::wants_frame` is the one gate for frames — no `RedrawRequested → redraw()` path, no per-frame push. `PillWindow::repush` runs only for the events that can invalidate the surface: display topology, DPI, lock/RDP/wake, and a home-monitor move — not `WM_DWMCOMPOSITIONCHANGED`, which fires often and means nothing for a per-pixel-alpha layered window.
- The pill is **click-through except while it is showing buttons** — the hover bar, or the cancel/confirm a *click-started* session carries. A hotkey session shows neither and stays click-through. That flip is a bare read-modify-write of `WS_EX_TRANSPARENT` (`PillWindow::set_click_through`, through `write_ex_style`, which tells a failed `SetWindowLongPtrW` from a legitimate 0 — route every `GWL_EXSTYLE` write through it). `SetWindowPos(SWP_FRAMECHANGED)` and winit's `set_cursor_hittest` both break it; the latter ORs the layered bit away. Because a click-through window is not a mouse target, hover is found by *polling* `GetCursorPos` against the window rect (physical virtual-screen pixels, no scaling); only per-button hover, once expanded, comes from winit's `CursorMoved`.
- The window is fixed at its envelope (`geom::ENVELOPE_*`) and only its pixels animate. Resizing it per frame would reallocate three pixmaps and a DIB section; `ensure_size` exists for DPI changes, not for morphs.
- The envelope is **not** the pill: since #46 the surface is 260x80, and the pill is drawn in the `PILL_BAND_H` band anchored to its *bottom* edge, with the label above. Everything positional comes off `geom::pill_centre_y`; `pm.height() / 2.0` and the window rect's own centre are both bugs, in the renderer and in the hover-reach regions alike. Layout — widths, slabs, discs, button sizes — lives in `geom`; `core` is state only and asks `geom` when it hit-tests.
- A frame touches only its **damage**: what the renderer drew plus what the frame before drew (`surface.rs`). The bounds come from `render`'s `Canvas`, so every mark goes through `Canvas`'s `fill_path`/`stroke_path`/`fill_rect` — a draw straight on the `Pixmap` goes unreported, and its pixels are never cleared.
- An animated frame allocates nothing outside tiny-skia's rasterizer: the meter reuses its FFT scratch, the bar row is an array, the window overwrites its last frame in place. `a_recording_frame_allocates_nothing` holds that line through the test build's counting allocator (`alloc_count.rs`).
- This module warns on `clippy::undocumented_unsafe_blocks` and `missing_safety_doc`: each `unsafe` block gets a `// SAFETY:` comment, each `unsafe fn` a `# Safety` section. A helper whose only precondition is a valid `HWND` is a safe fn — a stale handle fails the call.
- The **home monitor** (`monitor.rs`) is where every number about the window comes from — its rect and the scale it renders at. `Home` is a pure core like `core`; the display *snapshot* is refreshed only on display/DPI change, never per poll, and only `focused` and `cursor` sample anything per loop. `PillWindow::set_home` is a raw `SetWindowPos`, not `Window::set_outer_position` — winit's mutator runs `apply_diff` and would clobber the ex-styles.
