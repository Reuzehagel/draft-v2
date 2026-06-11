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
