// ElevenLabs Scribe transcription client.
//
// POST multipart/form-data to https://api.elevenlabs.io/v1/speech-to-text
// with fields: model_id=<id>, file=<wav bytes>, tag_audio_events=false, and
// one `keyterms` field per vocabulary term when there are any. Auth is the
// `xi-api-key` header, not a bearer token. Response JSON has a top-level
// "text" field with the transcript.
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

/// Scribe refuses a keyterm of this many characters or more.
const KEYTERM_MAX_CHARS: usize = 50;
/// Scribe refuses a keyterm of more words than this.
const KEYTERM_MAX_WORDS: usize = 5;

pub struct ElevenLabsTranscriber {
    api_key: String,
    /// The vocabulary hint, less any term Scribe would refuse.
    keyterms: Vec<String>,
    client: reqwest::blocking::Client,
}

impl ElevenLabsTranscriber {
    /// `terms` is the normalised vocabulary hint (`vocabulary::hint_terms`).
    pub fn new(api_key: String, timeout: Duration, terms: &[String]) -> Result<Self> {
        let client = super::http_client(timeout)?;
        let mut keyterms = super::vocabulary::carried("elevenlabs", terms, |term| {
            if term.chars().count() >= KEYTERM_MAX_CHARS {
                Some("50 characters or more")
            } else if term.split_whitespace().count() > KEYTERM_MAX_WORDS {
                Some("more than 5 words")
            } else {
                None
            }
        });
        // Past 100 keyterms Scribe bills every request as at least 20 s.
        keyterms.truncate(super::vocabulary::MAX_TERMS);
        Ok(Self {
            api_key,
            keyterms,
            client,
        })
    }

    /// Every text field of the upload, beside the audio itself.
    ///
    /// Asked for what was **said**: `tag_audio_events` defaults to true and
    /// would put "(laughter)" at someone's cursor, so it is pinned off. No
    /// `no_verbatim` — filler removal is cleanup, and cleanup belongs to
    /// Replacements. No `language_code`: omitted means auto-detect per clip,
    /// which is what a bilingual user needs. `keyterms` carry a surcharge, so
    /// they go only when the user has a vocabulary.
    fn text_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![
            ("model_id", MODEL.to_string()),
            ("tag_audio_events", "false".to_string()),
        ];
        fields.extend(self.keyterms.iter().map(|t| ("keyterms", t.clone())));
        fields
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
        for (field, value) in self.text_fields() {
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

    fn scribe(vocab: &[&str]) -> ElevenLabsTranscriber {
        let terms: Vec<String> = vocab.iter().map(|t| t.to_string()).collect();
        ElevenLabsTranscriber::new("key".into(), Duration::from_secs(5), &terms).unwrap()
    }

    #[test]
    fn requests_scribe_v2_with_audio_events_untagged() {
        assert_eq!(ENDPOINT, "https://api.elevenlabs.io/v1/speech-to-text");
        assert_eq!(
            scribe(&[]).text_fields(),
            [
                ("model_id", "scribe_v2".to_string()),
                ("tag_audio_events", "false".to_string()),
            ]
        );
    }

    #[test]
    fn nothing_that_tidies_or_pins_the_words_is_sent() {
        for (field, _) in scribe(&["Draft"]).text_fields() {
            assert!(!field.starts_with("language"));
            assert_ne!(field, "no_verbatim");
        }
    }

    #[test]
    fn sends_one_keyterm_per_term() {
        assert_eq!(
            scribe(&["Draft", "Parakeet"]).text_fields()[2..],
            [
                ("keyterms", "Draft".to_string()),
                ("keyterms", "Parakeet".to_string()),
            ]
        );
    }

    #[test]
    fn skips_a_keyterm_scribe_would_refuse() {
        let long = "x".repeat(50);
        let fits = "x".repeat(49);
        let six_words = "one two three four five six";
        let five_words = "one two three four five";
        let t = scribe(&[&long, &fits, six_words, five_words]);
        assert_eq!(t.keyterms, [fits.as_str(), five_words]);
    }

    #[test]
    fn never_sends_more_than_a_hundred_keyterms() {
        let terms: Vec<String> = (0..150).map(|i| format!("term{i}")).collect();
        let t = ElevenLabsTranscriber::new("key".into(), Duration::from_secs(5), &terms).unwrap();
        assert_eq!(t.keyterms.len(), 100);
    }

    #[test]
    fn attributes_to_elevenlabs() {
        assert_eq!(scribe(&[]).name(), "elevenlabs");
    }
}
