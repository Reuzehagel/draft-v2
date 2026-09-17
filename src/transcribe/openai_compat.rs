// OpenAI-compatible transcription client, serving both OpenAI and Groq —
// Groq's speech endpoint is a drop-in clone of OpenAI's, so one adapter
// covers both.
//
// POST multipart/form-data to <endpoint> with fields: file=<wav bytes>,
// model=<id>, and optionally prompt=<vocab hint>. Bearer auth. Response
// JSON has a top-level "text" field with the transcript.
//
// The `prompt` field is the weak tier of custom-vocabulary biasing (issue
// #2): both providers condition the decoder on a short run of free text
// (~224 tokens on Whisper), so feeding it the user's vocabulary nudges
// spelling toward those terms. OpenAI's `gpt-transcribe` also takes a
// `keywords[]` list; the hint stays in `prompt` until the vocabulary work
// moves it. Strong keyterm biasing (Reson8) is separate, per-provider work.

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
            "gpt-transcribe",
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

    /// Every text field of the upload, beside the audio itself.
    ///
    /// No `language` (nor OpenAI's newer `languages[]`) on purpose: omitted
    /// means auto-detect per clip, which is what a bilingual user needs. No
    /// `response_format` either — both services answer JSON by default, and
    /// JSON is the only output OpenAI documents for `gpt-transcribe`.
    fn text_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![("model", self.model.to_string())];
        if let Some(prompt) = &self.prompt {
            fields.push(("prompt", prompt.clone()));
        }
        fields
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
        // `model`, then the audio, then the rest — the order this adapter has
        // always sent, kept so neither service sees a different request.
        let mut fields = self.text_fields().into_iter();
        let mut form = reqwest::blocking::multipart::Form::new();
        if let Some((field, value)) = fields.next() {
            form = form.text(field, value);
        }
        form = form.part("file", part);
        for (field, value) in fields {
            form = form.text(field, value);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn openai(prompt: Option<&str>) -> OpenAiCompatTranscriber {
        let timeout = Duration::from_secs(5);
        OpenAiCompatTranscriber::openai("key".into(), timeout, prompt.map(str::to_string)).unwrap()
    }

    fn groq(prompt: Option<&str>) -> OpenAiCompatTranscriber {
        let timeout = Duration::from_secs(5);
        OpenAiCompatTranscriber::groq("key".into(), timeout, prompt.map(str::to_string)).unwrap()
    }

    #[test]
    fn openai_requests_gpt_transcribe() {
        let t = openai(None);
        assert_eq!(t.endpoint, "https://api.openai.com/v1/audio/transcriptions");
        assert_eq!(t.text_fields(), [("model", "gpt-transcribe".to_string())]);
    }

    #[test]
    fn openai_carries_the_vocabulary_hint_in_prompt() {
        assert_eq!(
            openai(Some("Draft, Parakeet")).text_fields(),
            [
                ("model", "gpt-transcribe".to_string()),
                ("prompt", "Draft, Parakeet".to_string()),
            ]
        );
    }

    #[test]
    fn groq_requests_whisper_large_v3_turbo() {
        let t = groq(Some("Draft"));
        assert_eq!(
            t.endpoint,
            "https://api.groq.com/openai/v1/audio/transcriptions"
        );
        assert_eq!(
            t.text_fields(),
            [
                ("model", "whisper-large-v3-turbo".to_string()),
                ("prompt", "Draft".to_string()),
            ]
        );
    }

    #[test]
    fn no_language_is_ever_sent() {
        for t in [openai(Some("Draft")), groq(Some("Draft"))] {
            assert!(t
                .text_fields()
                .iter()
                .all(|(field, _)| !field.starts_with("language")));
        }
    }
}
