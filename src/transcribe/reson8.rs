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

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

const ENDPOINT: &str = "https://api.reson8.dev/v1/speech-to-text/prerecorded";

pub struct Reson8Transcriber {
    api_key: String,
    client: reqwest::blocking::Client,
}

impl Reson8Transcriber {
    pub fn new(api_key: String, timeout: Duration) -> Result<Self> {
        let client = super::http_client(timeout)?;
        Ok(Self { api_key, client })
    }
}

impl Transcriber for Reson8Transcriber {
    fn name(&self) -> &'static str {
        "reson8"
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let wav_bytes = super::samples_to_wav_bytes(samples, TARGET_SR)
            .context("encode WAV for Reson8 upload")?;

        // No `language` param on purpose: the API auto-detects per clip when
        // it's omitted, which is what a bilingual user needs. Pinning it to
        // "en" (an early copy-paste default) degraded every non-English clip.
        let resp = self
            .client
            .post(ENDPOINT)
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
