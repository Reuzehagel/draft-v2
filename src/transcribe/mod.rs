// Transcriber trait + WAV helpers for providers that need an encoded
// payload (cloud uploaders). Local providers can take the raw f32 slice
// directly via the trait's `transcribe` entry point.

pub mod mistral;
pub mod openai_compat;
pub mod parakeet;
pub mod parakeet_download;
pub mod reson8;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

/// Request timeout for cloud transcribers when they're the only option:
/// generous, since waiting beats losing the dictation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Tighter timeout used when a local fallback is standing by — no point
/// hanging on a dead network for a minute when Parakeet can serve the
/// transcript in a couple of seconds.
pub const FALLBACK_PRIMARY_TIMEOUT: Duration = Duration::from_secs(15);

pub trait Transcriber: Send + Sync + 'static {
    /// 16 kHz mono f32 PCM in [-1.0, 1.0].
    fn transcribe(&self, samples: &[f32]) -> Result<String>;
    fn name(&self) -> &'static str;

    /// Transcribe and report which provider actually produced the text.
    /// Trivially `name()` for plain providers; wrappers that route between
    /// providers override it so attribution travels with the result instead
    /// of through shared state (which races between concurrent dictations).
    fn transcribe_attributed(&self, samples: &[f32]) -> Result<(String, &'static str)> {
        self.transcribe(samples).map(|text| (text, self.name()))
    }

    /// Release any heavy resident state (e.g. an on-device model held in RAM)
    /// if it has gone unused for at least `timeout`. Called periodically from
    /// the main loop. Default: no-op — cloud providers hold nothing resident.
    fn unload_if_idle(&self, _timeout: std::time::Duration) {}
}

/// Reliability wrapper (issue #7): try the configured cloud provider, and on
/// any error — network down, timeout, 5xx — transcribe locally with Parakeet
/// instead, so a connectivity blip degrades quality instead of losing words.
pub struct FallbackTranscriber {
    primary: Arc<dyn Transcriber>,
    fallback: Arc<dyn Transcriber>,
}

impl FallbackTranscriber {
    pub fn new(primary: Arc<dyn Transcriber>, fallback: Arc<dyn Transcriber>) -> Self {
        Self { primary, fallback }
    }
}

impl Transcriber for FallbackTranscriber {
    fn name(&self) -> &'static str {
        self.primary.name()
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        self.transcribe_attributed(samples).map(|(text, _)| text)
    }

    /// Attribution rides with the result, so history records the provider
    /// that actually produced each transcript — a degraded fallback result
    /// shouldn't be mistaken for the primary provider's quality, and two
    /// concurrent dictations can't mislabel each other.
    fn transcribe_attributed(&self, samples: &[f32]) -> Result<(String, &'static str)> {
        match self.primary.transcribe(samples) {
            Ok(text) => Ok((text, self.primary.name())),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    primary = self.primary.name(),
                    fallback = self.fallback.name(),
                    "primary transcription failed; falling back to local"
                );
                self.fallback
                    .transcribe(samples)
                    .map(|text| (text, self.fallback.name()))
            }
        }
    }

    fn unload_if_idle(&self, timeout: Duration) {
        self.primary.unload_if_idle(timeout);
        self.fallback.unload_if_idle(timeout);
    }
}

/// Single seam for transcriber construction: owns provider selection,
/// API-key loading, vocabulary-hint baking, and fallback-wrapping. The main
/// process asks here for a ready-to-use `Transcriber` and never deals with
/// providers, keys, or the vocabulary hint directly.
pub fn build(cfg: &crate::config::Config) -> Option<Arc<dyn Transcriber>> {
    // With a local fallback standing by, give the cloud call a tighter
    // timeout — failing over beats hanging on a dead network for a minute.
    let fallback_ready = cfg.fallback_to_local
        && cfg.provider != crate::config::Provider::LocalParakeet
        && parakeet_download::is_present();
    let timeout = if fallback_ready {
        FALLBACK_PRIMARY_TIMEOUT
    } else {
        DEFAULT_TIMEOUT
    };
    let primary = build_primary(cfg, timeout)?;
    if !fallback_ready {
        return Some(primary);
    }
    match local_parakeet() {
        Some(local) => Some(Arc::new(FallbackTranscriber::new(primary, local))),
        // Model present but dir unresolvable — degraded but functional:
        // run the cloud provider unwrapped rather than not at all.
        None => Some(primary),
    }
}

fn local_parakeet() -> Option<Arc<dyn Transcriber>> {
    if !parakeet_download::is_present() {
        tracing::warn!(
            "Parakeet model files missing — open Settings and click \
             'Download model' to fetch them"
        );
        return None;
    }
    let dir = match parakeet_download::model_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "resolve model dir failed");
            return None;
        }
    };
    // Lazy: the ~700MB model is pulled into RAM on first dictation
    // and released again after MODEL_IDLE_TIMEOUT of inactivity.
    let t = parakeet::ParakeetTranscriber::new(&dir);
    Some(Arc::new(t) as Arc<dyn Transcriber>)
}

fn build_primary(cfg: &crate::config::Config, timeout: Duration) -> Option<Arc<dyn Transcriber>> {
    fn arc<T: Transcriber>(t: Result<T>, what: &str) -> Option<Arc<dyn Transcriber>> {
        match t {
            Ok(t) => Some(Arc::new(t) as Arc<dyn Transcriber>),
            Err(e) => {
                tracing::error!(error = %e, "failed to build {what} transcriber");
                None
            }
        }
    }
    use crate::config::Provider;
    use openai_compat::OpenAiCompatTranscriber;
    // Baked in at construction so no downstream caller can forget to pass it
    // and silently disable biasing on prompt-capable providers.
    let vocab = || vocab_prompt(&cfg.vocabulary);
    match cfg.provider {
        Provider::LocalParakeet => local_parakeet(),
        Provider::Mistral => {
            let key = crate::secrets::load_key(Provider::Mistral)?;
            arc(mistral::MistralTranscriber::new(key, timeout), "Mistral")
        }
        Provider::Reson8 => {
            let key = crate::secrets::load_key(Provider::Reson8)?;
            arc(reson8::Reson8Transcriber::new(key, timeout), "Reson8")
        }
        Provider::Groq => {
            let key = crate::secrets::load_key(Provider::Groq)?;
            arc(OpenAiCompatTranscriber::groq(key, timeout, vocab()), "Groq")
        }
        Provider::Openai => {
            let key = crate::secrets::load_key(Provider::Openai)?;
            arc(
                OpenAiCompatTranscriber::openai(key, timeout, vocab()),
                "OpenAI",
            )
        }
        other => {
            tracing::warn!(?other, "provider not yet implemented; no transcriber");
            None
        }
    }
}

/// Build the free-text vocabulary hint for prompt-based providers (OpenAI,
/// Groq) from the user's term list. Whisper's prompt window is ~224 tokens;
/// stay well under it by skipping terms past a character budget — and say so,
/// rather than silently truncating mid-term.
pub fn vocab_prompt(terms: &[String]) -> Option<String> {
    const MAX_CHARS: usize = 600;
    let mut out = String::new();
    let mut dropped = 0usize;
    for term in terms {
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        let sep = if out.is_empty() { 0 } else { 2 };
        if out.len() + sep + term.len() > MAX_CHARS {
            dropped += 1;
            continue;
        }
        if sep > 0 {
            out.push_str(", ");
        }
        out.push_str(term);
    }
    if dropped > 0 {
        tracing::warn!(
            dropped,
            "custom vocabulary exceeds the provider prompt budget; \
             later terms were not sent"
        );
    }
    (!out.is_empty()).then_some(out)
}

/// Shared blocking HTTP client for the cloud transcribers — keeps the
/// timeout/TLS policy in one place instead of re-spelling the builder per
/// backend.
pub fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .context("build reqwest client")
}

/// Clip a response body for inclusion in an error or log message, so a huge
/// HTML error page can't flood the diagnostics.
pub fn clip_body(body: &str, max_chars: usize) -> String {
    body.chars().take(max_chars).collect()
}

#[derive(Debug, Deserialize)]
struct TextResponse {
    text: String,
}

/// Consume a cloud transcriber's HTTP response: error out on a non-2xx status
/// (with a truncated body for diagnostics), otherwise parse the shared
/// `{ "text": ... }` shape every provider returns.
pub fn parse_text_response(provider: &str, resp: reqwest::blocking::Response) -> Result<String> {
    let status = resp.status();
    let body = resp.text().context("read response body")?;
    if !status.is_success() {
        return Err(anyhow!(
            "{provider} returned {status}: {}",
            clip_body(&body, 500)
        ));
    }
    let parsed: TextResponse = serde_json::from_str(&body).with_context(|| {
        format!("parse {provider} response: {}", clip_body(&body, 200))
    })?;
    Ok(parsed.text)
}

pub fn samples_to_wav_bytes(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(samples.len() * 2 + 44);
    {
        let cursor = Cursor::new(&mut buf);
        let mut w = hound::WavWriter::new(cursor, spec)?;
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            let v = (clamped * i16::MAX as f32) as i16;
            w.write_sample(v)?;
        }
        w.finalize()?;
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Scripted stub Transcriber: pops results off a queue in call order and
    /// reports a fixed name, so `FallbackTranscriber`'s routing and
    /// attribution can be driven deterministically without a network or a
    /// real model.
    struct StubTranscriber {
        name: &'static str,
        results: Mutex<std::collections::VecDeque<Result<String>>>,
    }

    impl StubTranscriber {
        fn new(name: &'static str, results: Vec<Result<String>>) -> Self {
            Self {
                name,
                results: Mutex::new(results.into_iter().collect()),
            }
        }
    }

    impl Transcriber for StubTranscriber {
        fn transcribe(&self, _samples: &[f32]) -> Result<String> {
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("stub exhausted")))
        }

        fn name(&self) -> &'static str {
            self.name
        }
    }

    #[test]
    fn fallback_uses_primary_on_success() {
        let primary = Arc::new(StubTranscriber::new("primary", vec![Ok("hello".into())]));
        // Fallback would return different text; it must not be consulted.
        let fallback = Arc::new(StubTranscriber::new("fallback", vec![Ok("degraded".into())]));
        let t = FallbackTranscriber::new(primary, fallback);
        let (text, provider) = t.transcribe_attributed(&[]).unwrap();
        assert_eq!(text, "hello");
        assert_eq!(provider, "primary");
    }

    #[test]
    fn fallback_uses_fallback_on_primary_error() {
        let primary = Arc::new(StubTranscriber::new(
            "primary",
            vec![Err(anyhow!("network down"))],
        ));
        let fallback = Arc::new(StubTranscriber::new("fallback", vec![Ok("local text".into())]));
        let t = FallbackTranscriber::new(primary, fallback);
        let (text, provider) = t.transcribe_attributed(&[]).unwrap();
        assert_eq!(text, "local text");
        assert_eq!(provider, "fallback");
    }

    #[test]
    fn vocab_prompt_joins_and_skips_blanks() {
        let terms: Vec<String> = vec!["Reson8".into(), "   ".into(), "egui".into()];
        assert_eq!(vocab_prompt(&terms).as_deref(), Some("Reson8, egui"));
    }

    #[test]
    fn vocab_prompt_empty_is_none() {
        assert_eq!(vocab_prompt(&[]), None);
        assert_eq!(vocab_prompt(&["  ".into()]), None);
    }

    #[test]
    fn vocab_prompt_stays_under_budget() {
        let terms: Vec<String> = (0..200).map(|i| format!("term{i:03}xxxxxxxxxx")).collect();
        let p = vocab_prompt(&terms).unwrap();
        assert!(p.len() <= 600);
        // The first terms made it in untruncated.
        assert!(p.starts_with("term000xxxxxxxxxx, term001xxxxxxxxxx"));
        assert!(!p.ends_with(','));
    }
}
