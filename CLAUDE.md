# Draft

Windows push-to-talk speech-to-text: hold a global hotkey, speak, release, and the transcript is pasted at the cursor. Rust, tray-resident, no console window in release builds. Windows-only: `cpal`, `global-hotkey`, and the `windows` crate make it non-portable.

## Commands

There is no CI. `.githooks/pre-commit` runs `cargo fmt --check` and `cargo clippy -D warnings` on every commit (enable it per clone: `git config core.hooksPath .githooks`); run `cargo test` yourself before committing. Clippy stays warning-free (dead-code warnings were cleaned up deliberately) and the tree stays rustfmt-clean, so an ordinary change never drags a reformat of files it didn't touch.

`#[ignore]`d tests are tools that write files instead of asserting: `cargo test -- --ignored tray::tests::preview` draws the tray icon at every scale, on both taskbars, to `target/tray-preview/`. `cargo test -- --ignored mark::tests::write` regenerates `assets/draft.ico` (the exe and MSI icon, embedded by `build.rs`) after any change to `src/mark.rs` — a test fails until you do. `cargo test --bin draft -- --ignored --nocapture llm::tests::live` sends push-to-command's real request (`DRAFT_COMMAND` sets the instruction) with the stored key and prints the answer — no microphone needed. The pill's preview and frame bench are in `src/pill/CLAUDE.md`.

To look at the settings window, `cargo build` then `Start-Process .\target\debug\draft.exe -ArgumentList '--settings-preview' -Wait` (from the repo root): it opens the real window over your config, shows each pane, and saves them to `target/settings-preview/` as `01-recording.png`… before closing itself. It only looks — nothing is edited or saved.

`cargo test` and `cargo clippy` build the test harness, not `target/debug/draft.exe` — `cargo build` before launching. Launch the exe from PowerShell (`Start-Process .\draft.exe`); from Git Bash it exits 127 with no output. The single-instance gate means the user's running Draft must be closed first, or the new process exits at once.

## Branches and PRs

Work goes on an `area/topic` branch (`pill/settled-appearance`), never straight to `main`. PRs merge squash-only, and a single-commit PR lands its commit message as written — write it as the permanent record. PR titles and commit subjects carry no number — the squash merge appends `(#PR)` — and the PR body links the issue with `Closes #N`. Releases: `docs/agents/release.md`.

## Architecture

One library and two binaries. `lib.rs` holds the shared core — `paths`, `config`, `secrets`, `logging`, `history`, `audio`, `postprocess`, `transcribe`, `decode`, `transcription_run` — and nothing in it may reach back to the adapter (no `session`, `pill`, `tray`, `paste`, `hotkey`, `activation`, `settings_ui`, `llm`, `update`, `single_instance`). That closure is what makes a second binary possible; keep it.

- **`draft.exe`** (`main.rs`), a *window* program — no console. Two processes come out of it:
  - **Main process**: single-instance gate, tray icon, global hotkeys, winit event loop. It is the *adapter* — it performs the Commands that `session.rs` returns.
  - **Settings subprocess**: the same exe relaunched with `--settings`, running eframe/egui (`settings_ui/`). The main process polls for its exit and reloads `config.toml` afterward — settings never talk to the main process directly.
- **`draft-cli.exe`** (`cli/main.rs`), a *console* program: `draft-cli transcribe <file>` decodes a media file and prints the transcript. A window program can't print to a terminal or make a shell wait for it, hence the separate artifact — see `docs/adr/0001-console-subcommand-in-a-second-binary.md`.

Dictation flow: `hotkey.rs` (raw chord events) → `activation.rs` (FSM: hold/toggle/double-press-lock) → `session.rs` (pure core: events + `now` in, Commands out) → `audio/` (cpal capture, resample to 16 kHz mono) → worker thread → `transcribe/` → `postprocess/` (replacements, then voice commands) → `paste.rs` (clipboard+Ctrl+V or SendInput unicode). `pill/` is a peer core, not downstream of dictation.

Transcription run (`transcription_run.rs`): file → `decode.rs` (symphonia demux/decode, resampled to 16 kHz mono by the *same* `audio::resample`) → `transcribe::build` → **Replacements only**. No voice commands (a recording's speaker isn't addressing Draft) and no history (nothing is pasted, so there's nothing to recover). Both are absences by construction, not flags.

Push-to-command (`llm.rs`): a second hotkey routes the transcript to a Groq chat model as an instruction and pastes the answer; it skips the postprocess pipeline.

## Facts that bite

- Config lives at `%APPDATA%\Draft\config.toml`; data, logs, history, and models under `%LOCALAPPDATA%\Draft\`. API keys are in the Windows Credential Manager via the `keyring` crate (`secrets.rs`) — never in config or env.
- Config writes go through `paths::atomic_write`; corrupt configs are backed up as `.toml.bak`, not overwritten.
- An unrecognised `provider` in `config.toml` loads as local Parakeet (with a warning) rather than failing the parse, so removing a Provider costs nobody their settings.
- History (`history.rs`) records every transcript *before* the paste attempt — it is the recovery path for lost pastes. Keep that order.
- The settings UI is screenshot-reviewed for polish. `settings_ui/widgets.rs` documents layout invariants at the top of the file — read them before touching any settings layout, and keep the two-pane sidebar structure.
- Hotkey re-registration on config reload releases old bindings first (re-registering an unchanged chord collides with itself). See `reload_config` in `main.rs`.
- The event loop is **event-driven**: `ControlFlow::Wait` is the resting state, and `pill::ladder` is the only thing that may arm a timer. Anything that reports over a channel — hotkeys, the tray menu, a transcription worker, the settings watcher, the update check — must call `wake::Waker::wake` after sending, or the loop will not hear it. A deadline that isn't the pill's belongs on a thread that can wait on it (see `spawn_model_reaper`), not in `about_to_wait`.
- Anything under `src/pill/` has its own rules in `src/pill/CLAUDE.md`.

## Agent skills

- **Naming anything** — a type, a test, an issue title: `CONTEXT.md` is the glossary and binds the vocabulary. Read `docs/adr/` before working in an area it touches. Details: `docs/agents/domain.md`.
- **Issues and PRDs** live as GitHub issues (`Reuzehagel/draft-v2`) via `gh`. Conventions: `docs/agents/issue-tracker.md`; labels: `docs/agents/triage-labels.md`.
