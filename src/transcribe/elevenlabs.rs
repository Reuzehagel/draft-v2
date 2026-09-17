// ElevenLabs Scribe transcription client.
//
// POST multipart/form-data to https://api.elevenlabs.io/v1/speech-to-text
// with fields: model_id=<id>, file=<wav bytes>, tag_audio_events=false. Auth
// is the `xi-api-key` header, not a bearer token. Response JSON has a
// top-level "text" field with the transcript.
//
// Scribe rejects audio shorter than 100 ms; that surfaces as the non-2xx
// error every adapter reports, and the fallback (when there is one) serves
// the clip instead.

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

const ENDPOINT: &str = "https://api.elevenlabs.io/v1/speech-to-text";
const MODEL: &str = "scribe_v2";

pub struct ElevenLabsTranscriber {
    api_key: String,
    client: reqwest::blocking::Client,
}

impl ElevenLabsTranscriber {
    pub fn new(api_key: String, timeout: Duration) -> Result<Self> {
        let client = super::http_client(timeout)?;
        Ok(Self { api_key, client })
    }

    /// Every text field of the upload, beside the audio itself.
    ///
    /// Asked for what was **said**: `tag_audio_events` defaults to true and
    /// would put "(laughter)" at someone's cursor, so it is pinned off. No
    /// `no_verbatim` — filler removal is cleanup, and cleanup belongs to
    /// Replacements. No `language_code`: omitted means auto-detect per clip,
    /// which is what a bilingual user needs. No `keyterms` either; they carry
    /// a surcharge and are the vocabulary work's to send.
    fn text_fields() -> Vec<(&'static str, String)> {
        vec![
            ("model_id", MODEL.to_string()),
            ("tag_audio_events", "false".to_string()),
        ]
    }
}

impl Transcriber for ElevenLabsTranscriber {
    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let wav_bytes = super::samples_to_wav_bytes(samples, TARGET_SR)
            .context("encode WAV for ElevenLabs upload")?;
        let part = reqwest::blocking::multipart::Part::bytes(wav_bytes)
            .file_name("clip.wav")
            .mime_str("audio/wav")
            .context("set wav mime")?;
        let mut form = reqwest::blocking::multipart::Form::new();
        for (field, value) in Self::text_fields() {
            form = form.text(field, value);
        }
        form = form.part("file", part);

        let resp = self
            .client
            .post(ENDPOINT)
            .header("xi-api-key", &self.api_key)
            .multipart(form)
            .send()
            .context("POST to ElevenLabs")?;

        super::parse_text_response("ElevenLabs", resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_scribe_v2_with_audio_events_untagged() {
        assert_eq!(ENDPOINT, "https://api.elevenlabs.io/v1/speech-to-text");
        assert_eq!(
            ElevenLabsTranscriber::text_fields(),
            [
                ("model_id", "scribe_v2".to_string()),
                ("tag_audio_events", "false".to_string()),
            ]
        );
    }

    #[test]
    fn nothing_that_tidies_or_pins_the_words_is_sent() {
        for (field, _) in ElevenLabsTranscriber::text_fields() {
            assert!(!field.starts_with("language"));
            assert_ne!(field, "no_verbatim");
            assert_ne!(field, "keyterms");
        }
    }

    #[test]
    fn attributes_to_elevenlabs() {
        let t = ElevenLabsTranscriber::new("key".into(), Duration::from_secs(5)).unwrap();
        assert_eq!(t.name(), "elevenlabs");
    }
}
