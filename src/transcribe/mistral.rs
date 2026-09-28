// Mistral Voxtral transcription client.
//
// POST multipart/form-data to https://api.mistral.ai/v1/audio/transcriptions
// with fields: file=<wav bytes>, model=<id>, and one `context_bias` field per
// vocabulary term when there are any. Bearer auth. Response JSON has a
// top-level "text" field with the transcript.

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

const ENDPOINT: &str = "https://api.mistral.ai/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "voxtral-mini-latest";

pub struct MistralTranscriber {
    api_key: String,
    model: String,
    /// The vocabulary hint, sent whole: Mistral takes up to 100 terms, which
    /// is Draft's own cap.
    context_bias: Vec<String>,
    client: reqwest::blocking::Client,
}

impl MistralTranscriber {
    /// `terms` is the normalised vocabulary hint (`vocabulary::hint_terms`).
    pub fn new(api_key: String, timeout: Duration, terms: &[String]) -> Result<Self> {
        let client = super::http_client(timeout)?;
        Ok(Self {
            api_key,
            model: DEFAULT_MODEL.into(),
            context_bias: terms.to_vec(),
            client,
        })
    }

    /// Every text field of the upload, beside the audio itself.
    ///
    /// No `language` field on purpose: omitted means auto-detect per clip,
    /// which is what a bilingual user needs.
    fn text_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![("model", self.model.clone())];
        fields.extend(
            self.context_bias
                .iter()
                .map(|t| ("context_bias", t.clone())),
        );
        fields
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
        // `model`, then the audio, then the rest — the order this adapter has
        // always sent.
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
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .context("POST to Mistral")?;

        super::parse_text_response("Mistral", resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mistral(vocab: &[&str]) -> MistralTranscriber {
        let terms: Vec<String> = vocab.iter().map(|t| t.to_string()).collect();
        MistralTranscriber::new("key".into(), Duration::from_secs(5), &terms).unwrap()
    }

    #[test]
    fn sends_the_vocabulary_as_context_bias() {
        assert_eq!(
            mistral(&["Draft", "Parakeet"]).text_fields(),
            [
                ("model", "voxtral-mini-latest".to_string()),
                ("context_bias", "Draft".to_string()),
                ("context_bias", "Parakeet".to_string()),
            ]
        );
    }

    #[test]
    fn no_vocabulary_sends_only_the_model() {
        assert_eq!(
            mistral(&[]).text_fields(),
            [("model", "voxtral-mini-latest".to_string())]
        );
    }
}
