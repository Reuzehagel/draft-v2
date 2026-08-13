// `draft-cli` — the console-subsystem sibling of `draft.exe`.
//
// Why a second binary at all: `draft.exe` is a window program, so `println!`
// goes nowhere and a shell returns the prompt without waiting for it. See
// docs/adr/0001-console-subcommand-in-a-second-binary.md.
//
// The contract this file exists to keep:
//
//   - **stdout carries the transcript and nothing else.** Every diagnostic,
//     every error, every log line goes to stderr, so `draft-cli transcribe x.mp4
//     > out.txt` yields the words and nothing to strip. An agent reads stdout
//     directly.
//   - **exit codes mirror Outcome:** 0 Delivered, 0 Empty (silence is not an
//     error), 1 Failed, 2 over the duration cap.
//   - **no single-instance gate.** The tray app is expected to be running.
//
// All the work is in the library; this file is argument parsing and the exit
// code. Keep it that way — anything with behaviour worth testing belongs in
// `draft::transcription_run`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use draft::transcription_run::{self, RunError, RunOutcome};

#[derive(Parser)]
#[command(
    name = "draft-cli",
    about = "Draft's command line — speech to text without the hotkey",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Transcribe a media file and print the transcript to stdout.
    Transcribe {
        /// The media file to transcribe (mp3, mp4, m4a, wav, flac, alac).
        file: PathBuf,
    },
}

fn main() -> ExitCode {
    // Logging goes to the same file as the tray app's, but the terminal only
    // gets warnings and errors — an agent parsing stderr shouldn't have to
    // wade through debug spans, and neither should a person.
    let _log_guard = match draft::logging::init_cli() {
        Ok(g) => Some(g),
        // Losing the log file is not a reason to refuse the transcript.
        Err(e) => {
            eprintln!("draft-cli: logging unavailable: {e:#}");
            None
        }
    };

    let cli = Cli::parse();
    match cli.command {
        Command::Transcribe { file } => transcribe(file),
    }
}

/// Everything that can go differently funnels into one `Result`, so the exit
/// code is decided in exactly one place — by the outcome itself, never spelled
/// out at a call site where it could drift from the contract.
fn transcribe(file: PathBuf) -> ExitCode {
    let result = draft::config::Config::load()
        .map_err(|e| RunError::Failed(e.context("could not read config")))
        .and_then(|cfg| transcription_run::run(&file, &cfg));

    match result {
        Ok(outcome) => {
            match &outcome {
                RunOutcome::Delivered(text) => println!("{text}"),
                // Nothing on stdout: a redirect must capture an empty file,
                // not the word "silence".
                RunOutcome::Empty => {
                    eprintln!("draft-cli: no speech found in {}", file.display())
                }
            }
            ExitCode::from(outcome.exit_code())
        }
        Err(e) => {
            eprintln!("draft-cli: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}
