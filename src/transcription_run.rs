// A **Transcription run**: one execution of the `transcribe` subcommand — a
// media file in, text out.
//
// This is not a **Session**. There is no activation, no capture, no pill and
// no paste, so there is also:
//
//   - **no history.** History exists because a paste can land in the wrong
//     window and the text would be lost. Nothing is pasted here, so there is
//     nothing to recover — and the tray's "Copy last" must never offer text
//     the user did not speak. This module does not reach `history`, by
//     construction rather than by a flag.
//   - **no voice commands.** A recorded file's speaker is not addressing
//     Draft, so "new paragraph" is words. `run` takes `Replacements` alone; it
//     could not apply voice commands if it wanted to.
//
// What it *does* share with a session is the two things worth sharing: the
// **Transcriber** (`transcribe::build`, which already owns provider selection,
// key loading, **Vocabulary hint** baking and **Fallback** wrapping) and the
// **Replacements**.

use std::path::Path;

use crate::config::Config;
use crate::decode::{self, DecodeError};
use crate::postprocess::Replacements;
use crate::transcribe::Transcriber;

/// The terminal result of a run — the **Outcome** vocabulary, as far as a
/// **Transcription run** can reach it.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Text was produced. Never empty or whitespace-only.
    Delivered(String),
    /// The provider heard nothing. Silence in, silence out — not an error.
    Empty,
}

#[derive(Debug)]
pub enum RunError {
    /// The file is longer than [`decode::MAX_DURATION`].
    TooLong(decode::TooLong),
    /// Anything else: unreadable file, no provider configured, provider error.
    Failed(anyhow::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::TooLong(e) => write!(f, "{e}"),
            RunError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl RunOutcome {
    /// The process exit code this outcome earns.
    ///
    /// `Empty` exits `0` alongside `Delivered`: silence is an honest answer,
    /// not a failure, and an agent that read it as one would keep retrying a
    /// file that will never produce words.
    pub fn exit_code(&self) -> u8 {
        match self {
            RunOutcome::Delivered(_) | RunOutcome::Empty => 0,
        }
    }
}

impl RunError {
    pub fn exit_code(&self) -> u8 {
        match self {
            RunError::Failed(_) => 1,
            RunError::TooLong(_) => 2,
        }
    }
}

/// Decode `path`, transcribe it with the configured **Provider**, and apply
/// the user's **Replacements**.
pub fn run(path: &Path, cfg: &Config) -> Result<RunOutcome, RunError> {
    let samples = decode::to_16k_mono(path).map_err(|e| match e {
        DecodeError::TooLong(e) => RunError::TooLong(e),
        // Wrapped rather than stringified, so the decoder's own error chain
        // survives into the log file.
        e @ DecodeError::Unreadable(_) => RunError::Failed(anyhow::Error::new(e)),
    })?;

    let transcriber = crate::transcribe::build(cfg).ok_or_else(|| {
        RunError::Failed(anyhow::anyhow!(
            "no transcriber available for provider {} — open Draft's settings \
             and check the provider and its API key",
            cfg.provider.label()
        ))
    })?;

    run_samples(
        &samples,
        transcriber.as_ref(),
        &Replacements::from_config(cfg),
    )
}

/// The half of a run that needs no file and no network of its own: samples in,
/// **Outcome** out. Split out so the post-transcription contract — replacements
/// applied, voice commands not, history untouched — is testable with a stub
/// **Transcriber**.
pub fn run_samples(
    samples: &[f32],
    transcriber: &dyn Transcriber,
    replacements: &Replacements,
) -> Result<RunOutcome, RunError> {
    let text = transcriber
        .transcribe(samples)
        .map_err(|e| RunError::Failed(e.context("transcription failed")))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(RunOutcome::Empty);
    }
    Ok(RunOutcome::Delivered(replacements.apply(trimmed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Replacement;

    struct StubTranscriber(&'static str);

    impl Transcriber for StubTranscriber {
        fn transcribe(&self, _samples: &[f32]) -> anyhow::Result<String> {
            Ok(self.0.to_owned())
        }
        fn name(&self) -> &'static str {
            "stub"
        }
    }

    fn cfg() -> Config {
        Config {
            // Voice commands on, to prove a Transcription run ignores them
            // even when a Session would apply them.
            voice_commands: true,
            replacements: vec![Replacement {
                from: "draft".into(),
                to: "Draft".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// The transcript gets **Replacements** and *not* **Voice commands**: a
    /// speaker who says "new paragraph" on tape meant those words.
    #[test]
    fn applies_replacements_but_not_voice_commands() {
        let out = run_samples(
            &[],
            &StubTranscriber("draft new paragraph please"),
            &Replacements::from_config(&cfg()),
        )
        .unwrap();
        assert_eq!(
            out,
            RunOutcome::Delivered("Draft new paragraph please".into())
        );
    }

    /// Silence is `Empty`, and `Empty` exits 0 — an agent must not retry a
    /// file that will never produce words.
    #[test]
    fn silence_is_empty_and_succeeds() {
        let out = run_samples(
            &[],
            &StubTranscriber("   \n "),
            &Replacements::from_config(&cfg()),
        )
        .unwrap();
        assert_eq!(out, RunOutcome::Empty);
        assert_eq!(out.exit_code(), 0);
    }

    /// A **Transcription run** never appends to history: it does not paste, so
    /// there is nothing to recover, and the tray's "Copy last" must not offer
    /// text the user never spoke.
    ///
    /// This reads the real history file, because `history` resolves its path
    /// from the Windows known folders and there is no seam to point it
    /// elsewhere. It still fails on the regression it is for — an append would
    /// move `last()` from `None` to `Some`, or change it — but note that it
    /// only ever reads, and never writes, the user's own history.
    #[test]
    fn does_not_touch_history() {
        let before = crate::history::last().map(|e| (e.ts, e.text));
        let out = run_samples(
            &[],
            &StubTranscriber("words that must not be recorded"),
            &Replacements::from_config(&cfg()),
        )
        .unwrap();
        assert!(matches!(out, RunOutcome::Delivered(_)));
        let after = crate::history::last().map(|e| (e.ts, e.text));
        assert_eq!(before, after, "a transcription run appended to history");
    }

    #[test]
    fn over_length_exits_2_and_failure_exits_1() {
        let too_long = RunError::TooLong(decode::TooLong { seconds: 1200.0 });
        assert_eq!(too_long.exit_code(), 2);
        assert_eq!(RunError::Failed(anyhow::anyhow!("boom")).exit_code(), 1);
    }
}
