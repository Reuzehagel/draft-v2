// Post-processing pipeline: the stages a raw transcript passes through
// between transcription and paste.
//
//   transcribe -> [find/replace] -> [voice commands] -> paste
//
// Find/replace (issue #2) and voice commands (#3) run in order, each taking
// the previous stage's text. Everything here is deterministic, instant, and
// provider-agnostic — LLM work deliberately lives outside this pipeline, in
// push-to-command (issue #4), so the dictation fast path never waits on a
// model.

mod commands;
mod replace;

use crate::config::Config;

/// The ordered transforms applied to a transcript before it's pasted. Built
/// once per dictation from the current config and moved onto the worker thread.
#[derive(Debug, Clone)]
pub struct Pipeline {
    replacements: Vec<crate::config::Replacement>,
    voice_commands: bool,
}

impl Pipeline {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            // Drop disabled and empty-`from` rules up front so `apply` is a
            // straight walk with no per-call filtering.
            replacements: cfg
                .replacements
                .iter()
                .filter(|r| r.enabled && !r.from.is_empty())
                .cloned()
                .collect(),
            voice_commands: cfg.voice_commands,
        }
    }

    /// True when the pipeline would do nothing — lets a caller skip work.
    /// Not yet wired into the event loop.
    #[allow(dead_code)]
    pub fn is_noop(&self) -> bool {
        self.replacements.is_empty() && !self.voice_commands
    }

    /// Run every stage in order and return the transformed text. Replacements
    /// run first so a rule can't accidentally assemble or break up a command
    /// phrase the user actually spoke.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for rule in &self.replacements {
            out = replace::apply_replacement(&out, rule);
        }
        if self.voice_commands {
            out = commands::apply_commands(&out);
        }
        out
    }
}
