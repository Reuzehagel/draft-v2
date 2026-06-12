# Draft

Windows push-to-talk speech-to-text: hold a global hotkey, speak, release, and the transcript is pasted at the cursor. Rust, tray-resident, no console window in release builds.

## Commands

```
cargo build            # debug build
cargo test             # unit tests (activation FSM, voice commands, replacements, WAV encoding)
cargo clippy           # keep warning-free; dead-code warnings have been cleaned up deliberately
cargo build --release  # LTO + stripped; the shipped binary
```

Windows-only — `cpal`, `global-hotkey`, and the `windows` crate make this non-portable. There is no CI; run tests and clippy locally before committing.

## Architecture

Two processes from one binary:

- **Main process** (`main.rs`): single-instance gate, tray icon, global hotkeys, winit event loop. Owns the dictation state machine.
- **Settings subprocess**: the same exe relaunched with `--settings`, running eframe/egui (`settings_ui/`). The main process polls for its exit and reloads `config.toml` afterward — settings never talk to the main process directly.

Dictation flow: `hotkey.rs` (raw chord events) → `activation.rs` (FSM: hold/toggle/double-press-lock) → `audio/` (cpal capture, resample to 16 kHz mono) → worker thread → `transcribe/` → `postprocess/` (voice commands, then replacements) → `paste.rs` (clipboard+Ctrl+V or SendInput unicode). The pill overlay (`pill/`) renders mic bars during capture, a breathing border while the worker runs, then a green/red flash for the real outcome — workers report back over a channel with a session id so a stale worker can't repaint a newer pill.

`transcribe/` providers: local Parakeet (onnx via `transcribe-rs`, lazily loaded, unloaded after 5 min idle), Mistral, Reson8, and OpenAI/Groq through `openai_compat.rs`. `FallbackTranscriber` wraps cloud providers with the local model when enabled. The `Xai` and `Elevenlabs` enum variants exist in config but are not implemented.

Push-to-command (`llm.rs`): a second hotkey routes the transcript to a Groq chat model as an instruction and pastes the answer; it skips the postprocess pipeline.

## Facts that bite

- Config lives at `%APPDATA%\Draft\config.toml`; data, logs, history, and models under `%LOCALAPPDATA%\Draft\`. API keys are in the Windows Credential Manager via the `keyring` crate (`secrets.rs`) — never in config or env.
- Config writes go through `paths::atomic_write`; corrupt configs are backed up as `.toml.bak`, not overwritten.
- History (`history.rs`) records every transcript *before* the paste attempt — it is the recovery path for lost pastes. Don't reorder that.
- The settings UI is screenshot-reviewed for polish. `settings_ui/widgets.rs` documents layout invariants at the top of the file — read them before touching any settings layout, and keep the two-pane sidebar structure.
- Hotkey re-registration on config reload releases old bindings first (re-registering an unchanged chord collides with itself). See `reload_config` in `main.rs`.
