// Post-processing pipeline: the stages a raw transcript passes through
// between transcription and paste.
//
//   transcribe -> [find/replace] -> [voice commands] -> [optional LLM] -> paste
//
// Only the find/replace stage exists today (issue #2). Commands (#3) and the
// gated LLM transform (#1) are meant to slot in here as further stages, each
// taking the previous stage's text. Stages run in order; everything here is
// deterministic, instant, and provider-agnostic.

mod replace;

use crate::config::Config;

/// The ordered transforms applied to a transcript before it's pasted. Built
/// once per dictation from the current config and moved onto the worker thread.
#[derive(Debug, Clone)]
pub struct Pipeline {
    replacements: Vec<crate::config::Replacement>,
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
        }
    }

    /// True when the pipeline would do nothing — lets the caller skip work
    /// (and, later, avoid spinning up an LLM call for a no-op). Not yet wired
    /// into the event loop; kept for the gated LLM stage (issue #1).
    #[allow(dead_code)]
    pub fn is_noop(&self) -> bool {
        self.replacements.is_empty()
    }

    /// Run every stage in order and return the transformed text.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for rule in &self.replacements {
            out = replace::apply_replacement(&out, rule);
        }
        out
    }
}
