// Mistral Voxtral transcription client.
//
// POST multipart/form-data to https://api.mistral.ai/v1/audio/transcriptions
// with fields: file=<wav bytes>, model=<id>. Bearer auth. Response JSON has
// a top-level "text" field with the transcript.

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

const ENDPOINT: &str = "https://api.mistral.ai/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "voxtral-mini-latest";

pub struct MistralTranscriber {
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl MistralTranscriber {
    pub fn new(api_key: String, timeout: Duration) -> Result<Self> {
        let client = super::http_client(timeout)?;
        Ok(Self {
            api_key,
            model: DEFAULT_MODEL.into(),
            client,
        })
    }
}

impl Transcriber for MistralTranscriber {
    fn name(&self) -> &'static str {
        "mistral"
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let wav_bytes = super::samples_to_wav_bytes(samples, TARGET_SR)
            .context("encode WAV for Mistral upload")?;
        let part = reqwest::blocking::multipart::Part::bytes(wav_bytes)
            .file_name("clip.wav")
            .mime_str("audio/wav")
            .context("set wav mime")?;
        // No `language` field on purpose: omitted means auto-detect per clip,
        // which is what a bilingual user needs.
        let form = reqwest::blocking::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);

        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .context("POST to Mistral")?;

        super::parse_text_response("Mistral", resp)
    }
}
