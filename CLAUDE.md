# Draft

Windows push-to-talk speech-to-text: hold a global hotkey, speak, release, and the transcript is pasted at the cursor. Rust, tray-resident, no console window in release builds.

## Commands

`cargo test` covers the activation FSM, voice commands, replacements, and WAV encoding. There is no CI — run tests and `cargo clippy` locally before committing; clippy stays warning-free (dead-code warnings were cleaned up deliberately). Windows-only: `cpal`, `global-hotkey`, and the `windows` crate make this non-portable.

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

## Agent skills

- **Naming anything** — a type, a test, an issue title: `CONTEXT.md` is the glossary and binds the vocabulary. Read `docs/adr/` before working in an area it touches. Details: `docs/agents/domain.md`.
- **Issues and PRDs** live as GitHub issues (`Reuzehagel/draft-v2`) via `gh`; external PRs are not a triage surface. Conventions: `docs/agents/issue-tracker.md`.
- **Triage labels** are the default names (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`).
