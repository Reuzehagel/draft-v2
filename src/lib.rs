// Draft's shared core — everything both binaries need.
//
// `draft.exe` (src/main.rs) is the tray adapter; `draft-cli.exe`
// (src/cli/main.rs) is the console subcommand. The split is forced by
// docs/adr/0001-console-subcommand-in-a-second-binary.md: a window-subsystem
// program cannot print to a terminal, so `transcribe` had to ship as its own
// artifact, and every module it needs had to leave `main.rs`.
//
// The line is drawn at I/O-free-of-the-event-loop: these modules read config
// and secrets, decode and resample audio, reach a provider, and clean up a
// transcript. What stays in the binary is the adapter — the FSMs, the pill,
// the tray, the paste, the hotkeys, the settings UI. Nothing here may reach
// back across that line.

pub mod audio;
pub mod config;
pub mod decode;
pub mod history;
pub mod logging;
pub mod paths;
pub mod postprocess;
pub mod secrets;
pub mod transcribe;
pub mod transcription_run;
