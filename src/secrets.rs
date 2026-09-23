// API keys live in the OS keyring (Windows Credential Manager), not in
// config.toml. Service name is "Draft"; the "username" slot is the
// provider tag — e.g. ("Draft", "mistral_api_key"). Environment variables
// (e.g. MISTRAL_API_KEY) are checked first as a dev-friendly override, and
// are never written back.

use crate::config::Provider;

const SERVICE: &str = "Draft";

pub fn slot_name(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Mistral => Some("mistral_api_key"),
        Provider::Groq => Some("groq_api_key"),
        Provider::Openai => Some("openai_api_key"),
        Provider::Elevenlabs => Some("elevenlabs_api_key"),
        Provider::Reson8 => Some("reson8_api_key"),
        Provider::LocalParakeet => None,
    }
}

pub fn env_var(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Mistral => Some("MISTRAL_API_KEY"),
        Provider::Groq => Some("GROQ_API_KEY"),
        Provider::Openai => Some("OPENAI_API_KEY"),
        Provider::Elevenlabs => Some("ELEVENLABS_API_KEY"),
        Provider::Reson8 => Some("RESON8_API_KEY"),
        Provider::LocalParakeet => None,
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

pub fn load_key(provider: Provider) -> Option<String> {
    load_key_with_source(provider).0
}

pub fn load_key_with_source(provider: Provider) -> (Option<String>, KeySource) {
    if let Some(env) = env_var(provider) {
        if let Ok(v) = std::env::var(env) {
            if !v.trim().is_empty() {
                return (Some(v), KeySource::Environment);
            }
        }
    }
    let Some(slot) = slot_name(provider) else {
        return (None, KeySource::Absent);
    };
    let Ok(entry) = keyring::Entry::new(SERVICE, slot) else {
        return (None, KeySource::Unreadable);
    };
    match entry.get_password() {
        Ok(v) if v.trim().is_empty() => (None, KeySource::Absent),
        Ok(v) => (Some(v), KeySource::Keyring),
        Err(keyring::Error::NoEntry) => (None, KeySource::Absent),
        Err(_) => (None, KeySource::Unreadable),
    }
}

/// What a save does to one Provider's keyring slot.
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

pub fn apply_write(provider: Provider, write: &KeyWrite) -> anyhow::Result<()> {
    let Some(slot) = slot_name(provider) else {
        return Ok(());
    };
    let entry = keyring::Entry::new(SERVICE, slot)?;
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
    fn an_environment_key_is_not_written_by_an_unrelated_save() {
        assert_eq!(
            key_write("env-key", "env-key", KeySource::Environment),
            None
        );
    }

    #[test]
    fn a_key_that_failed_to_load_is_not_deleted_by_an_unrelated_save() {
        assert_eq!(key_write("", "", KeySource::Unreadable), None);
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
