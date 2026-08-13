// Post-processing: the stages a raw transcript passes through between
// transcription and paste.
//
//   transcribe -> [find/replace] -> [voice commands] -> paste
//
// Find/replace (issue #2) and voice commands (#3) run in order, each taking
// the previous stage's text. Everything here is deterministic, instant, and
// provider-agnostic — LLM work deliberately lives outside this pipeline, in
// push-to-command (issue #4), so the dictation fast path never waits on a
// model.
//
// The two stages are separate types because they belong to different things.
// **Replacements** correct vocabulary, so they are true of a **Session** and a
// **Transcription run** alike. **Voice commands** are spoken instructions to
// Draft, so they belong to a **Session** only — a recorded file's speaker is
// not addressing Draft, and applying them there would corrupt text that merely
// contains the phrase. A caller composes what it is entitled to: the worker
// thread builds a whole `Pipeline`, `transcription_run` takes `Replacements`
// alone and could not apply voice commands if it tried.

mod commands;
mod replace;

use crate::config::Config;

/// The user's find/replace rules, in order. Applied to any transcript whatever
/// produced it.
#[derive(Debug, Clone)]
pub struct Replacements {
    rules: Vec<crate::config::Replacement>,
}

impl Replacements {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            // Drop disabled and empty-`from` rules up front so `apply` is a
            // straight walk with no per-call filtering.
            rules: cfg
                .replacements
                .iter()
                .filter(|r| r.enabled && !r.from.is_empty())
                .cloned()
                .collect(),
        }
    }

    /// True when there is nothing to substitute — lets a caller skip work.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for rule in &self.rules {
            out = replace::apply_replacement(&out, rule);
        }
        out
    }
}

/// Spoken instructions to Draft ("new paragraph") turned into their effect.
/// A **Session** stage only — construct it from config, where the user can
/// turn it off, or not at all for a **Transcription run**.
#[derive(Debug, Clone, Copy)]
pub struct VoiceCommands;

impl VoiceCommands {
    /// `None` when the user has voice commands turned off.
    pub fn from_config(cfg: &Config) -> Option<Self> {
        cfg.voice_commands.then_some(Self)
    }

    pub fn apply(&self, text: &str) -> String {
        commands::apply_commands(text)
    }
}

/// The ordered transforms applied to a **Session**'s transcript before it's
/// pasted. Built once per dictation and moved onto the worker thread.
#[derive(Debug, Clone)]
pub struct Pipeline {
    replacements: Replacements,
    voice_commands: Option<VoiceCommands>,
}

impl Pipeline {
    /// Everything a **Session** is entitled to, read off the current config.
    /// The only way to build one — a **Transcription run** composes
    /// `Replacements` directly instead.
    pub fn for_session(cfg: &Config) -> Self {
        Self {
            replacements: Replacements::from_config(cfg),
            voice_commands: VoiceCommands::from_config(cfg),
        }
    }

    /// True when the pipeline would do nothing — lets a caller skip work.
    /// Not yet wired into the event loop.
    #[allow(dead_code)]
    pub fn is_noop(&self) -> bool {
        self.replacements.is_empty() && self.voice_commands.is_none()
    }

    /// Run every stage in order and return the transformed text. Replacements
    /// run first so a rule can't accidentally assemble or break up a command
    /// phrase the user actually spoke.
    pub fn apply(&self, text: &str) -> String {
        let out = self.replacements.apply(text);
        match &self.voice_commands {
            Some(vc) => vc.apply(&out),
            None => out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Replacement;

    fn cfg_with(voice_commands: bool) -> Config {
        Config {
            voice_commands,
            replacements: vec![Replacement {
                from: "draft".into(),
                to: "Draft".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// A **Session** gets both stages, in order.
    #[test]
    fn session_pipeline_applies_replacements_then_voice_commands() {
        let out = Pipeline::for_session(&cfg_with(true)).apply("draft new paragraph done");
        assert_eq!(out, "Draft\n\nDone");
    }

    /// A **Transcription run** composes replacements alone: a recorded speaker
    /// saying "new paragraph" is describing, not instructing.
    #[test]
    fn replacements_alone_leave_command_phrases_as_text() {
        let out = Replacements::from_config(&cfg_with(true)).apply("draft new paragraph done");
        assert_eq!(out, "Draft new paragraph done");
    }

    #[test]
    fn voice_commands_are_none_when_the_user_turns_them_off() {
        assert!(VoiceCommands::from_config(&cfg_with(false)).is_none());
        let out = Pipeline::for_session(&cfg_with(false)).apply("draft new paragraph done");
        assert_eq!(out, "Draft new paragraph done");
    }
}
