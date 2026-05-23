// API keys live in the OS keyring (Windows Credential Manager), not in
// config.toml. Service name is "Draft"; the "username" slot is the
// provider tag — e.g. ("Draft", "mistral_api_key"). Environment variables
// (e.g. MISTRAL_API_KEY) are checked first as a dev-friendly override.

use crate::config::Provider;

const SERVICE: &str = "Draft";

pub fn slot_name(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Mistral => Some("mistral_api_key"),
        Provider::Groq => Some("groq_api_key"),
        Provider::Openai => Some("openai_api_key"),
        Provider::Xai => Some("xai_api_key"),
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
        Provider::Xai => Some("XAI_API_KEY"),
        Provider::Elevenlabs => Some("ELEVENLABS_API_KEY"),
        Provider::Reson8 => Some("RESON8_API_KEY"),
        Provider::LocalParakeet => None,
    }
}

pub fn load_key(provider: Provider) -> Option<String> {
    if let Some(env) = env_var(provider) {
        if let Ok(v) = std::env::var(env) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    let slot = slot_name(provider)?;
    let entry = keyring::Entry::new(SERVICE, slot).ok()?;
    let v = entry.get_password().ok()?;
    if v.trim().is_empty() { None } else { Some(v) }
}

pub fn save_key(provider: Provider, value: &str) -> anyhow::Result<()> {
    let Some(slot) = slot_name(provider) else {
        return Ok(());
    };
    let entry = keyring::Entry::new(SERVICE, slot)?;
    if value.trim().is_empty() {
        let _ = entry.delete_credential();
    } else {
        entry.set_password(value)?;
    }
    Ok(())
}
