# Draft

Windows push-to-talk speech-to-text: hold a global hotkey, speak, release, and the transcript is pasted at the cursor. Rust, tray-resident, no console window in release builds.

## Commands

`cargo test` covers the activation FSM, voice commands, replacements, WAV encoding, and the pill's core, motion model and rendering. There is no CI — run `cargo test`, `cargo clippy` and `cargo fmt --check` locally before committing; clippy stays warning-free (dead-code warnings were cleaned up deliberately) and the tree stays rustfmt-clean, so an ordinary change never drags a reformat of files it didn't touch. Windows-only: `cpal`, `global-hotkey`, and the `windows` crate make this non-portable.

To *look* at the pill without launching anything:

```
cargo test -- --ignored pill::preview
```

writes every mode (over a light and a dark desktop) and every transition (as a filmstrip) to `target/pill-preview/`, through the same `Geom`s and renderer the real window uses. `#[ignore]`d because it asserts nothing and writes files. Reach for it whenever you touch `pill/geom.rs` or `pill/render.rs` — it catches what unit tests don't, and has already caught a conceal that left its bar row behind. It cannot show timing, so how a transition *feels* is still a question for the running app.

## Architecture

Two processes from one binary:

- **Main process** (`main.rs`): single-instance gate, tray icon, global hotkeys, winit event loop. It is the *adapter* — it performs the Commands that `session.rs` returns.
- **Settings subprocess**: the same exe relaunched with `--settings`, running eframe/egui (`settings_ui/`). The main process polls for its exit and reloads `config.toml` afterward — settings never talk to the main process directly.

Dictation flow: `hotkey.rs` (raw chord events) → `activation.rs` (FSM: hold/toggle/double-press-lock) → `session.rs` (pure core: events + `now` in, Commands out) → `audio/` (cpal capture, resample to 16 kHz mono) → worker thread → `transcribe/` → `postprocess/` (replacements, then voice commands) → `paste.rs` (clipboard+Ctrl+V or SendInput unicode). `pill/` is a peer core, not downstream of dictation.

Push-to-command (`llm.rs`): a second hotkey routes the transcript to a Groq chat model as an instruction and pastes the answer; it skips the postprocess pipeline.

The `Xai` and `Elevenlabs` config variants are not implemented.

## Facts that bite

- Config lives at `%APPDATA%\Draft\config.toml`; data, logs, history, and models under `%LOCALAPPDATA%\Draft\`. API keys are in the Windows Credential Manager via the `keyring` crate (`secrets.rs`) — never in config or env.
- Config writes go through `paths::atomic_write`; corrupt configs are backed up as `.toml.bak`, not overwritten.
- History (`history.rs`) records every transcript *before* the paste attempt — it is the recovery path for lost pastes. Don't reorder that.
- The settings UI is screenshot-reviewed for polish. `settings_ui/widgets.rs` documents layout invariants at the top of the file — read them before touching any settings layout, and keep the two-pane sidebar structure.
- Hotkey re-registration on config reload releases old bindings first (re-registering an unchanged chord collides with itself). See `reload_config` in `main.rs`.
- The resident pill is on screen doing nothing most of the time, so it must cost nothing: a settled pill asks for no frames, and the system maintains the layered surface. Don't add a `RedrawRequested → redraw()` path or any per-frame push — `PillAdapter::wants_frame` is the one gate, and `PillWindow::repush` is only for the events that can invalidate the surface (display topology, DPI, lock/RDP/wake, and a home-monitor move). Never `WM_DWMCOMPOSITIONCHANGED`.
- The pill is **click-through except while it is showing buttons**. That flip is a bare read-modify-write `SetWindowLongPtrW` on `WS_EX_TRANSPARENT` (`PillWindow::set_click_through`) — never `SetWindowPos(SWP_FRAMECHANGED)`, and never winit's `set_cursor_hittest`, which ORs the layered bit away. Because a click-through window is not a mouse target, hover is found by *polling* `GetCursorPos` against the window rect (physical virtual-screen pixels, no scaling); only per-button hover, once expanded, comes from winit's `CursorMoved`.
- The pill window is fixed at its envelope (`pill::geom::ENVELOPE_*`) and only its pixels animate. Resizing it per frame would reallocate three pixmaps and a DIB section; `ensure_size` exists for DPI changes, not for morphs.
- The pill's **home monitor** (`pill/monitor.rs`) is where every number about the window comes from — its rect and the scale it renders at. `Home` is a pure core like `pill::core`; the display *snapshot* is refreshed only on display/DPI change, never per poll, and only `focused` and `cursor` sample anything per loop. `PillWindow::set_home` is a raw `SetWindowPos`, not `Window::set_outer_position` — winit's mutator runs `apply_diff` and would clobber the ex-styles.

## Agent skills

- **Naming anything** — a type, a test, an issue title: `CONTEXT.md` is the glossary and binds the vocabulary. Read `docs/adr/` before working in an area it touches. Details: `docs/agents/domain.md`.
- **Issues and PRDs** live as GitHub issues (`Reuzehagel/draft-v2`) via `gh`; external PRs are not a triage surface. Conventions: `docs/agents/issue-tracker.md`.
- **Triage labels** are the default names (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`).
