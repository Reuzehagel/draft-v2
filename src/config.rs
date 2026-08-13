use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub activation: Activation,
    pub provider: Provider,
    pub append_trailing_space: bool,
    pub restore_clipboard: bool,
    pub double_press_lock: bool,
    pub paste_mode: PasteMode,
    /// Name of the input device to record from. `None` (the default) means the
    /// system default device, resolved fresh at each session start.
    pub input_device: Option<String>,
    /// Ordered find/replace rules applied to every transcript before paste.
    /// Provider-agnostic and instant — the first stage of the post-processing
    /// pipeline (see `src/postprocess`). Empty by default.
    pub replacements: Vec<Replacement>,
    /// Parse spoken formatting commands ("new line", "new paragraph",
    /// "scratch that", "all caps") out of the transcript before paste.
    pub voice_commands: bool,
    /// Custom vocabulary: proper nouns and jargon the recognizer should bias
    /// toward. Sent natively to providers that take a hint (OpenAI and Groq's
    /// `prompt` field); providers without biasing support ignore it — use a
    /// replacement rule there instead.
    pub vocabulary: Vec<String>,
    /// When the cloud provider errors out (network down, timeout, 5xx),
    /// transcribe locally with Parakeet instead of losing the dictation.
    /// Only takes effect when the local model is downloaded.
    pub fallback_to_local: bool,
    /// Push-to-command: a second hotkey where speech is an instruction and
    /// the LLM's answer is pasted instead of the words. Needs a Groq API key.
    pub push_to_command: bool,
    /// The hotkey that triggers push-to-command. Same syntax as `hotkey`.
    pub command_hotkey: String,
    /// The overlay's own settings. Last field on purpose: TOML puts every
    /// scalar before the first table, and a nested struct serialised ahead of
    /// one would emit a `[pill]` header with the remaining keys swallowed
    /// underneath it.
    #[serde(default)]
    pub pill: PillConfig,
}

/// The pill's settings. Its own table (`[pill]`) rather than flat keys, so the
/// residency toggle joins the home-monitor policy and the rest of the overlay's
/// settings as they land, instead of scattering `pill_*` keys across the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PillConfig {
    /// Keep the pill on screen with nothing happening — a nub that says Draft
    /// is on and armed. Off restores the session-only pill: nothing on screen
    /// between dictations, and no idle work at all.
    pub resident: bool,
}

impl Default for PillConfig {
    fn default() -> Self {
        Self { resident: true }
    }
}

/// A single find/replace rule. Rules run in order, each over the output of
/// the previous one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Replacement {
    /// Text to look for. Empty `from` rules are skipped.
    pub from: String,
    /// What to substitute in. May be empty (deletes the match).
    pub to: String,
    /// Only match when `from` is bounded by non-word characters, so "a row"
    /// won't fire inside "narrow". Word chars are alphanumerics and `_`.
    pub whole_word: bool,
    /// Match case exactly. When false, matching is case-insensitive (ASCII
    /// folding) but the replacement is inserted verbatim.
    pub case_sensitive: bool,
    /// Lets a rule be kept but turned off without deleting it.
    pub enabled: bool,
}

impl Default for Replacement {
    fn default() -> Self {
        Self {
            from: String::new(),
            to: String::new(),
            whole_word: false,
            case_sensitive: false,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PasteMode {
    /// Set clipboard + synthesize Ctrl+V (default; works in nearly every app).
    Clipboard,
    /// Bypass clipboard, type the transcript character-by-character via
    /// SendInput. Use only for hosts that swallow synthesized Ctrl+V.
    Unicode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    Hold,
    Toggle,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    LocalParakeet,
    Groq,
    Openai,
    Xai,
    Elevenlabs,
    Mistral,
    Reson8,
}

impl Provider {
    /// Human-readable name, shared by the settings UI and the tray tooltip so
    /// the two never disagree about what a provider is called.
    pub fn label(self) -> &'static str {
        match self {
            Provider::LocalParakeet => "Local (Parakeet)",
            Provider::Mistral => "Mistral (Voxtral)",
            Provider::Groq => "Groq",
            Provider::Openai => "OpenAI",
            Provider::Xai => "xAI",
            Provider::Elevenlabs => "ElevenLabs",
            Provider::Reson8 => "Reson8",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Backslash".into(),
            activation: Activation::Hold,
            provider: Provider::LocalParakeet,
            append_trailing_space: true,
            restore_clipboard: true,
            double_press_lock: true,
            paste_mode: PasteMode::Clipboard,
            input_device: None,
            replacements: Vec::new(),
            voice_commands: true,
            vocabulary: Vec::new(),
            fallback_to_local: true,
            push_to_command: false,
            command_hotkey: "Ctrl+Shift+Backslash".into(),
            pill: PillConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        Ok(Self::load_with_first_run()?.0)
    }

    /// Returns (config, first_run). `first_run` is true when the config
    /// file did not exist before this call.
    pub fn load_with_first_run() -> Result<(Self, bool)> {
        let path = crate::paths::config_file()?;
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok((cfg, true));
        }
        let text = std::fs::read_to_string(&path)?;
        let cfg: Self = match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                // Don't silently overwrite the user's settings on the next save:
                // preserve the unparseable file as a .bak so it can be recovered.
                let backup = path.with_extension("toml.bak");
                if let Err(be) = std::fs::rename(&path, &backup) {
                    tracing::warn!(error = %be, "could not back up corrupt config");
                }
                tracing::warn!(
                    error = %e,
                    backup = %backup.display(),
                    "config parse failed, using defaults (corrupt file backed up)"
                );
                Self::default()
            }
        };
        Ok((cfg, false))
    }

    pub fn save(&self) -> Result<()> {
        let path = crate::paths::config_file()?;
        let text = toml::to_string_pretty(self)?;
        // Atomic replace so a crash mid-write can't leave a truncated config.
        crate::paths::atomic_write(&path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every user upgrading into residency has a config.toml written before
    /// `[pill]` existed. It has to parse, pick the defaults up, and survive a
    /// save/load round trip unchanged — not be backed up as corrupt.
    #[test]
    fn a_config_written_before_the_pill_table_existed_still_parses() {
        let old = "hotkey = \"Ctrl+Backslash\"\n\
                   activation = \"hold\"\n\
                   provider = \"groq\"\n";
        let cfg: Config = toml::from_str(old).expect("old config parses");
        assert_eq!(cfg.hotkey, "Ctrl+Backslash");
        assert_eq!(cfg.provider, Provider::Groq);
        // Residency is on by default — the nub is what this ticket is for.
        assert!(cfg.pill.resident);
        assert_eq!(cfg.pill, PillConfig::default());
    }

    /// The table itself is `#[serde(default)]` too, so a `[pill]` section that
    /// exists but is empty (or gains a key this build doesn't know) is not a
    /// parse error either.
    #[test]
    fn an_empty_pill_table_takes_the_defaults() {
        let cfg: Config = toml::from_str("[pill]\n").expect("empty table parses");
        assert_eq!(cfg.pill, PillConfig::default());
    }

    #[test]
    fn the_config_round_trips_through_toml() {
        for resident in [true, false] {
            let mut cfg = Config::default();
            cfg.pill.resident = resident;
            let text = toml::to_string_pretty(&cfg).expect("serialise");
            let back: Config = toml::from_str(&text).expect("deserialise");
            assert_eq!(back, cfg, "{text}");
        }
    }

    /// TOML has no way back once a table header is emitted: every scalar has to
    /// come before `[pill]`. Serialising with the field anywhere but last emits
    /// a file whose later keys land *inside* the table — which round-trips into
    /// a parse error rather than silently wrong values, but is a landmine for
    /// the next field added to `Config` all the same.
    #[test]
    fn the_pill_table_is_serialised_after_every_scalar() {
        let text = toml::to_string_pretty(&Config::default()).expect("serialise");
        let header = text.find("[pill]").expect("the table is written");
        assert!(
            !text[header..].contains("hotkey"),
            "a scalar was emitted after the table header:\n{text}"
        );
    }
}
