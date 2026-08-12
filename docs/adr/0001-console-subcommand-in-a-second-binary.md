# The console subcommand ships as a second binary

Draft's release build is a window program (`windows_subsystem = "windows"`), so it has no console: `println!` goes nowhere and `cmd`/PowerShell return the prompt without waiting for it to exit. A **Transcription run** must be usable by a person at a terminal, not only by an agent reading a pipe, so `transcribe` ships as a separate console-subsystem binary (`draft-cli.exe`) alongside `draft.exe`.

This contradicts the "two processes from one binary" property the tray app and the settings subprocess share, which is why it is written down.

## Considered options

- **Write to stdout and do nothing else.** A window program still inherits a stdout handle when its parent supplies a pipe, so this works today for an agent — and only for an agent. A person at a terminal gets silence and an early prompt.
- **`AttachConsole(ATTACH_PARENT_PROCESS)`.** Fixes the printing, not the waiting: the shell has already moved on, so output lands after the next prompt.
- **One console-subsystem binary that calls `FreeConsole()` on the window path.** Flashes a black console window on every launch of the tray app.
- **A second binary.** Correct for every caller. Costs an artifact to build, ship and update.

The agent is the primary consumer, but interactive use is a stated requirement, and only the last option delivers it.

## Consequences

Every module the new binary needs must leave `main.rs`. `paths`, `config`, `secrets`, `logging`, `history`, `audio`, `postprocess` and `transcribe` form a closed set — none of them reference `session`, `pill`, `tray`, `paste` or the event loop — so they move to a `lib.rs` and `main.rs` keeps the adapter. The library extraction is a consequence of this decision, not an independent one.

## Decoding: symphonia, not Media Foundation

Media Foundation would decode mp3/mp4/m4a/wav from the OS with no growth in binary size, and Draft is Windows-only anyway. It was rejected because binary size stopped being a constraint — a pure-Rust decoder is deterministic across Windows installs and testable without COM. **Do not re-propose Media Foundation on size grounds**; size is not the deciding factor.
