use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub activation: Activation,
    pub provider: Provider,
    pub autostart: bool,
    pub append_trailing_space: bool,
    pub restore_clipboard: bool,
    pub double_press_lock: bool,
    pub paste_mode: PasteMode,
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
            autostart: false,
            append_trailing_space: true,
            restore_clipboard: true,
            double_press_lock: true,
            paste_mode: PasteMode::Clipboard,
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
        let cfg: Self = toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "config parse failed, using defaults");
            Self::default()
        });
        Ok((cfg, false))
    }

    pub fn save(&self) -> Result<()> {
        let path = crate::paths::config_file()?;
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)?;
        Ok(())
    }
}
