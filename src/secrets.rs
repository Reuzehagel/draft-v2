// API keys live in the OS keyring (Windows Credential Manager), not in
// config.toml. Service name is "Draft"; the "username" slot is the key's
// tag — e.g. ("Draft", "mistral_api_key"). Environment variables (e.g.
// MISTRAL_API_KEY) are checked first as a dev-friendly override, and are
// never written back.

use crate::config::{ChatBackend, Provider};

const SERVICE: &str = "Draft";

/// Whose key a keyring slot holds: a **Provider**'s, or the Cerebras **Chat
/// backend**'s. The Groq Chat backend has no slot of its own — it uses the
/// Groq Provider's — and Cerebras is not a Provider, so it gets this variant
/// rather than a place in the Provider list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySlot {
    Provider(Provider),
    Cerebras,
}

impl From<Provider> for KeySlot {
    fn from(p: Provider) -> Self {
        KeySlot::Provider(p)
    }
}

impl From<ChatBackend> for KeySlot {
    fn from(b: ChatBackend) -> Self {
        match b {
            ChatBackend::Groq => KeySlot::Provider(Provider::Groq),
            ChatBackend::Cerebras => KeySlot::Cerebras,
        }
    }
}

impl KeySlot {
    /// What the key is called where a user sees it: "<label> API key".
    pub fn label(self) -> &'static str {
        match self {
            KeySlot::Provider(p) => p.label(),
            KeySlot::Cerebras => ChatBackend::Cerebras.label(),
        }
    }
}

pub fn slot_name(slot: impl Into<KeySlot>) -> Option<&'static str> {
    match slot.into() {
        KeySlot::Provider(Provider::Mistral) => Some("mistral_api_key"),
        KeySlot::Provider(Provider::Groq) => Some("groq_api_key"),
        KeySlot::Provider(Provider::Openai) => Some("openai_api_key"),
        KeySlot::Provider(Provider::Elevenlabs) => Some("elevenlabs_api_key"),
        KeySlot::Provider(Provider::Reson8) => Some("reson8_api_key"),
        KeySlot::Provider(Provider::LocalParakeet) => None,
        KeySlot::Cerebras => Some("cerebras_api_key"),
    }
}

pub fn env_var(slot: impl Into<KeySlot>) -> Option<&'static str> {
    match slot.into() {
        KeySlot::Provider(Provider::Mistral) => Some("MISTRAL_API_KEY"),
        KeySlot::Provider(Provider::Groq) => Some("GROQ_API_KEY"),
        KeySlot::Provider(Provider::Openai) => Some("OPENAI_API_KEY"),
        KeySlot::Provider(Provider::Elevenlabs) => Some("ELEVENLABS_API_KEY"),
        KeySlot::Provider(Provider::Reson8) => Some("RESON8_API_KEY"),
        KeySlot::Provider(Provider::LocalParakeet) => None,
        KeySlot::Cerebras => Some("CEREBRAS_API_KEY"),
    }
}

/// Where a loaded key came from. The Settings window needs this to know
/// which keys are the user's to write back: a key from the environment is a
/// dev override, and one the keyring failed to hand over is not "no key".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Environment,
    Keyring,
    Absent,
    Unreadable,
}

pub fn load_key(slot: impl Into<KeySlot>) -> Option<String> {
    load_key_with_source(slot).0
}

pub fn load_key_with_source(slot: impl Into<KeySlot>) -> (Option<String>, KeySource) {
    let slot = slot.into();
    if let Some(env) = env_var(slot) {
        if let Ok(v) = std::env::var(env) {
            if !v.trim().is_empty() {
                return (Some(v), KeySource::Environment);
            }
        }
    }
    let Some(name) = slot_name(slot) else {
        return (None, KeySource::Absent);
    };
    let Ok(entry) = keyring::Entry::new(SERVICE, name) else {
        return (None, KeySource::Unreadable);
    };
    match entry.get_password() {
        Ok(v) if v.trim().is_empty() => (None, KeySource::Absent),
        Ok(v) => (Some(v), KeySource::Keyring),
        Err(keyring::Error::NoEntry) => (None, KeySource::Absent),
        Err(_) => (None, KeySource::Unreadable),
    }
}

/// What a save does to one keyring slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyWrite {
    Set(String),
    Delete,
}

impl KeyWrite {
    /// Where the key stands once this write has landed.
    pub fn leaves(&self) -> KeySource {
        match self {
            KeyWrite::Set(_) => KeySource::Keyring,
            KeyWrite::Delete => KeySource::Absent,
        }
    }
}

/// Which write, if any, a save owes one key: `baseline` is what the window
/// loaded (or last saved), `current` what it holds now. Only an edit is
/// written, and only a key that came from the keyring is ever deleted —
/// clearing an environment key, or one that failed to load, must not reach
/// whatever credential sits behind it.
pub fn key_write(baseline: &str, current: &str, source: KeySource) -> Option<KeyWrite> {
    if current == baseline {
        return None;
    }
    if !current.trim().is_empty() {
        return Some(KeyWrite::Set(current.to_string()));
    }
    (source == KeySource::Keyring).then_some(KeyWrite::Delete)
}

pub fn apply_write(slot: impl Into<KeySlot>, write: &KeyWrite) -> anyhow::Result<()> {
    let Some(name) = slot_name(slot) else {
        return Ok(());
    };
    let entry = keyring::Entry::new(SERVICE, name)?;
    match write {
        KeyWrite::Set(value) => entry.set_password(value)?,
        KeyWrite::Delete => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(e.into()),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_groq_chat_backend_shares_the_groq_providers_slot() {
        assert_eq!(
            KeySlot::from(ChatBackend::Groq),
            KeySlot::from(Provider::Groq)
        );
        assert_eq!(slot_name(ChatBackend::Groq), Some("groq_api_key"));
    }

    #[test]
    fn cerebras_has_its_own_slot_and_environment_override() {
        assert_eq!(KeySlot::from(ChatBackend::Cerebras), KeySlot::Cerebras);
        assert_eq!(slot_name(KeySlot::Cerebras), Some("cerebras_api_key"));
        assert_eq!(env_var(KeySlot::Cerebras), Some("CEREBRAS_API_KEY"));
        assert_eq!(KeySlot::Cerebras.label(), "Cerebras");
    }

    #[test]
    fn an_unedited_key_is_never_written() {
        for source in [
            KeySource::Environment,
            KeySource::Keyring,
            KeySource::Absent,
            KeySource::Unreadable,
        ] {
            assert_eq!(key_write("k", "k", source), None, "{source:?}");
            assert_eq!(key_write("", "", source), None, "{source:?}");
        }
    }

    #[test]
    fn clearing_a_stored_key_deletes_it() {
        assert_eq!(
            key_write("stored", "", KeySource::Keyring),
            Some(KeyWrite::Delete)
        );
    }

    #[test]
    fn clearing_an_environment_key_leaves_the_keyring_alone() {
        // What was shown came from the environment; the keyring slot behind it
        // was never shown, so it is not the user's to clear from here.
        assert_eq!(key_write("env-key", "", KeySource::Environment), None);
    }

    #[test]
    fn a_typed_key_is_stored_whatever_was_there() {
        for source in [
            KeySource::Environment,
            KeySource::Keyring,
            KeySource::Absent,
            KeySource::Unreadable,
        ] {
            let base = if source == KeySource::Absent || source == KeySource::Unreadable {
                ""
            } else {
                "old"
            };
            assert_eq!(
                key_write(base, "new", source),
                Some(KeyWrite::Set("new".into())),
                "{source:?}"
            );
        }
    }

    #[test]
    fn a_whitespace_only_edit_is_a_clear() {
        assert_eq!(
            key_write("stored", "  ", KeySource::Keyring),
            Some(KeyWrite::Delete)
        );
        assert_eq!(key_write("", "  ", KeySource::Absent), None);
    }
}
