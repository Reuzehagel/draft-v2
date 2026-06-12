// OpenAI-compatible transcription client, serving both OpenAI and Groq —
// Groq's speech endpoint is a drop-in clone of OpenAI's, so one adapter
// covers both.
//
// POST multipart/form-data to <endpoint> with fields: file=<wav bytes>,
// model=<id>, and optionally prompt=<vocab hint>. Bearer auth. Response
// JSON has a top-level "text" field with the transcript.
//
// The `prompt` field is the weak tier of custom-vocabulary biasing (issue
// #2): both providers condition the Whisper decoder on up to ~224 tokens of
// free text, so feeding it the user's vocabulary nudges spelling toward
// those terms. Strong keyterm biasing (Reson8) is separate, per-provider
// work.

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

pub struct OpenAiCompatTranscriber {
    name: &'static str,
    endpoint: &'static str,
    model: &'static str,
    api_key: String,
    /// Free-text decoder hint; built from the custom vocabulary when set.
    prompt: Option<String>,
    client: reqwest::blocking::Client,
}

impl OpenAiCompatTranscriber {
    pub fn openai(api_key: String, timeout: Duration, prompt: Option<String>) -> Result<Self> {
        Self::build(
            "openai",
            "https://api.openai.com/v1/audio/transcriptions",
            "gpt-4o-mini-transcribe",
            api_key,
            timeout,
            prompt,
        )
    }

    pub fn groq(api_key: String, timeout: Duration, prompt: Option<String>) -> Result<Self> {
        Self::build(
            "groq",
            "https://api.groq.com/openai/v1/audio/transcriptions",
            "whisper-large-v3-turbo",
            api_key,
            timeout,
            prompt,
        )
    }

    fn build(
        name: &'static str,
        endpoint: &'static str,
        model: &'static str,
        api_key: String,
        timeout: Duration,
        prompt: Option<String>,
    ) -> Result<Self> {
        let client = super::http_client(timeout)?;
        Ok(Self {
            name,
            endpoint,
            model,
            api_key,
            prompt,
            client,
        })
    }
}

impl Transcriber for OpenAiCompatTranscriber {
    fn name(&self) -> &'static str {
        self.name
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let wav_bytes = super::samples_to_wav_bytes(samples, TARGET_SR)
            .with_context(|| format!("encode WAV for {} upload", self.name))?;
        let part = reqwest::blocking::multipart::Part::bytes(wav_bytes)
            .file_name("clip.wav")
            .mime_str("audio/wav")
            .context("set wav mime")?;
        // No `language` field on purpose: omitted means auto-detect per clip,
        // which is what a bilingual user needs.
        let mut form = reqwest::blocking::multipart::Form::new()
            .text("model", self.model)
            .part("file", part);
        if let Some(prompt) = &self.prompt {
            form = form.text("prompt", prompt.clone());
        }

        let resp = self
            .client
            .post(self.endpoint)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .with_context(|| format!("POST to {}", self.name))?;

        super::parse_text_response(self.name, resp)
    }
}
