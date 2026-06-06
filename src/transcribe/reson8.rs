// Reson8 prerecorded transcription client.
//
// POST raw audio bytes (application/octet-stream) to
// https://api.reson8.dev/v1/speech-to-text/prerecorded with an
// `Authorization: ApiKey <key>` header. We send a 16 kHz mono WAV; the server
// auto-detects the container. Response JSON has a top-level "text" field with
// the transcript.
//
// Reson8 also supports exchanging the API key for a short-lived bearer token,
// but the prerecorded endpoint accepts the static ApiKey scheme directly, so
// we use that and keep this a one-request client like the other adapters.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

const ENDPOINT: &str = "https://api.reson8.dev/v1/speech-to-text/prerecorded";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Reson8Transcriber {
    api_key: String,
    language: Option<String>,
    client: reqwest::blocking::Client,
}

impl Reson8Transcriber {
    pub fn new(api_key: String) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            api_key,
            language: Some("en".into()),
            client,
        })
    }
}

#[derive(Debug, Deserialize)]
struct Reson8Response {
    text: String,
}

impl Transcriber for Reson8Transcriber {
    fn name(&self) -> &'static str {
        "reson8"
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let wav_bytes = super::samples_to_wav_bytes(samples, TARGET_SR)
            .context("encode WAV for Reson8 upload")?;

        let mut req = self
            .client
            .post(ENDPOINT)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("ApiKey {}", self.api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream");
        if let Some(lang) = &self.language {
            req = req.query(&[("language", lang.as_str())]);
        }

        let resp = req.body(wav_bytes).send().context("POST to Reson8")?;

        let status = resp.status();
        let body = resp.text().context("read response body")?;
        if !status.is_success() {
            return Err(anyhow!(
                "Reson8 returned {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            ));
        }
        let parsed: Reson8Response = serde_json::from_str(&body)
            .with_context(|| format!("parse Reson8 response: {}", &body[..body.len().min(200)]))?;
        Ok(parsed.text)
    }
}
