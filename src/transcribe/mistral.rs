// Mistral Voxtral transcription client.
//
// POST multipart/form-data to https://api.mistral.ai/v1/audio/transcriptions
// with fields: file=<wav bytes>, model=<id>, language=<iso>. Bearer auth.
// Response JSON has a top-level "text" field with the transcript.

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

const ENDPOINT: &str = "https://api.mistral.ai/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "voxtral-mini-latest";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct MistralTranscriber {
    api_key: String,
    model: String,
    language: Option<String>,
    client: reqwest::blocking::Client,
}

impl MistralTranscriber {
    pub fn new(api_key: String) -> Result<Self> {
        let client = super::http_client(REQUEST_TIMEOUT)?;
        Ok(Self {
            api_key,
            model: DEFAULT_MODEL.into(),
            language: Some("en".into()),
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
        let mut form = reqwest::blocking::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);
        if let Some(lang) = &self.language {
            form = form.text("language", lang.clone());
        }

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
