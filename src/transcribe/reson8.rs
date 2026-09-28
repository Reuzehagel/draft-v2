// Reson8 prerecorded transcription client.
//
// POST raw audio bytes (application/octet-stream) to
// https://api.reson8.dev/v1/speech-to-text/prerecorded with an
// `Authorization: ApiKey <key>` header. We send a 16 kHz mono WAV; the server
// auto-detects the container. Response JSON has a top-level "text" field with
// the transcript. The vocabulary hint rides in the `phrases` query parameter,
// comma-separated, so a term with a comma in it is skipped; `bias_strength` is
// left at Reson8's default, and no `custom_model_id` is used.
//
// Reson8 also supports exchanging the API key for a short-lived bearer token,
// but the prerecorded endpoint accepts the static ApiKey scheme directly, so
// we use that and keep this a one-request client like the other adapters.

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

const ENDPOINT: &str = "https://api.reson8.dev/v1/speech-to-text/prerecorded";

pub struct Reson8Transcriber {
    api_key: String,
    /// The vocabulary hint, less any term with a comma in it.
    phrases: Vec<String>,
    client: reqwest::blocking::Client,
}

impl Reson8Transcriber {
    /// `terms` is the normalised vocabulary hint (`vocabulary::hint_terms`).
    pub fn new(api_key: String, timeout: Duration, terms: &[String]) -> Result<Self> {
        let client = super::http_client(timeout)?;
        let phrases = super::vocabulary::carried("reson8", terms, |term| {
            term.contains(',').then_some("contains a comma")
        });
        Ok(Self {
            api_key,
            phrases,
            client,
        })
    }

    /// The request's query parameters.
    ///
    /// No `language` on purpose: the API auto-detects per clip when it's
    /// omitted, which is what a bilingual user needs. Pinning it to "en" (an
    /// early copy-paste default) degraded every non-English clip.
    fn query(&self) -> Vec<(&'static str, String)> {
        if self.phrases.is_empty() {
            return Vec::new();
        }
        vec![("phrases", self.phrases.join(","))]
    }
}

impl Transcriber for Reson8Transcriber {
    fn name(&self) -> &'static str {
        "reson8"
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let wav_bytes = super::samples_to_wav_bytes(samples, TARGET_SR)
            .context("encode WAV for Reson8 upload")?;

        let resp = self
            .client
            .post(ENDPOINT)
            .query(&self.query())
            .header(
                reqwest::header::AUTHORIZATION,
                format!("ApiKey {}", self.api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(wav_bytes)
            .send()
            .context("POST to Reson8")?;

        super::parse_text_response("Reson8", resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reson8(vocab: &[&str]) -> Reson8Transcriber {
        let terms: Vec<String> = vocab.iter().map(|t| t.to_string()).collect();
        Reson8Transcriber::new("key".into(), Duration::from_secs(5), &terms).unwrap()
    }

    #[test]
    fn sends_the_vocabulary_as_comma_joined_phrases() {
        assert_eq!(
            reson8(&["Draft", "Parakeet v3"]).query(),
            [("phrases", "Draft,Parakeet v3".to_string())]
        );
    }

    #[test]
    fn skips_a_phrase_with_a_comma() {
        assert_eq!(
            reson8(&["Smith, John", "Draft"]).query(),
            [("phrases", "Draft".to_string())]
        );
    }

    #[test]
    fn no_vocabulary_sends_no_query() {
        assert!(reson8(&[]).query().is_empty());
    }

    #[test]
    fn never_sends_a_language_or_bias_strength() {
        for (param, _) in reson8(&["Draft"]).query() {
            assert_ne!(param, "language");
            assert_ne!(param, "bias_strength");
            assert_ne!(param, "custom_model_id");
        }
    }
}
