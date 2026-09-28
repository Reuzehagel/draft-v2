// OpenAI-compatible transcription client, serving both OpenAI and Groq —
// Groq's speech endpoint is a drop-in clone of OpenAI's, so one adapter
// covers both.
//
// POST multipart/form-data to <endpoint> with fields: file=<wav bytes>,
// model=<id>, and the vocabulary hint when there is one. Bearer auth.
// Response JSON has a top-level "text" field with the transcript.
//
// The two render the vocabulary hint differently. OpenAI's `gpt-transcribe`
// takes a term list: one `keywords[]` field per term, skipping a term with
// `<`, `>` or a line break in it. Groq's Whisper takes only a free-text
// `prompt` (~224 tokens), which `vocab_prompt` fills from the top of the list
// down to a character budget.

use anyhow::{Context, Result};
use std::time::Duration;

use super::Transcriber;
use crate::audio::TARGET_SR;

pub struct OpenAiCompatTranscriber {
    name: &'static str,
    endpoint: &'static str,
    model: &'static str,
    api_key: String,
    /// The vocabulary hint, rendered into this service's fields at
    /// construction; empty when there's no vocabulary.
    vocab_fields: Vec<(&'static str, String)>,
    client: reqwest::blocking::Client,
}

impl OpenAiCompatTranscriber {
    /// `terms` is the normalised vocabulary hint (`vocabulary::hint_terms`).
    pub fn openai(api_key: String, timeout: Duration, terms: &[String]) -> Result<Self> {
        let keywords = super::vocabulary::carried("openai", terms, |term| {
            term.contains(['<', '>', '\r', '\n'])
                .then_some("contains <, > or a line break")
        });
        Self::build(
            "openai",
            "https://api.openai.com/v1/audio/transcriptions",
            "gpt-transcribe",
            api_key,
            timeout,
            keywords.into_iter().map(|k| ("keywords[]", k)).collect(),
        )
    }

    /// `terms` is the normalised vocabulary hint (`vocabulary::hint_terms`).
    pub fn groq(api_key: String, timeout: Duration, terms: &[String]) -> Result<Self> {
        Self::build(
            "groq",
            "https://api.groq.com/openai/v1/audio/transcriptions",
            "whisper-large-v3-turbo",
            api_key,
            timeout,
            super::vocab_prompt(terms)
                .map(|prompt| ("prompt", prompt))
                .into_iter()
                .collect(),
        )
    }

    fn build(
        name: &'static str,
        endpoint: &'static str,
        model: &'static str,
        api_key: String,
        timeout: Duration,
        vocab_fields: Vec<(&'static str, String)>,
    ) -> Result<Self> {
        let client = super::http_client(timeout)?;
        Ok(Self {
            name,
            endpoint,
            model,
            api_key,
            vocab_fields,
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
        fields.extend(self.vocab_fields.iter().cloned());
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

    fn terms(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|t| t.to_string()).collect()
    }

    fn openai(vocab: &[&str]) -> OpenAiCompatTranscriber {
        let timeout = Duration::from_secs(5);
        OpenAiCompatTranscriber::openai("key".into(), timeout, &terms(vocab)).unwrap()
    }

    fn groq(vocab: &[&str]) -> OpenAiCompatTranscriber {
        let timeout = Duration::from_secs(5);
        OpenAiCompatTranscriber::groq("key".into(), timeout, &terms(vocab)).unwrap()
    }

    #[test]
    fn openai_requests_gpt_transcribe() {
        let t = openai(&[]);
        assert_eq!(t.endpoint, "https://api.openai.com/v1/audio/transcriptions");
        assert_eq!(t.text_fields(), [("model", "gpt-transcribe".to_string())]);
    }

    #[test]
    fn openai_sends_one_keyword_per_term_and_no_prompt() {
        assert_eq!(
            openai(&["Draft", "Parakeet"]).text_fields(),
            [
                ("model", "gpt-transcribe".to_string()),
                ("keywords[]", "Draft".to_string()),
                ("keywords[]", "Parakeet".to_string()),
            ]
        );
    }

    #[test]
    fn openai_skips_a_keyword_it_cannot_carry() {
        let t = openai(&["<b>", "a>b", "two\nlines", "cr\r", "Draft"]);
        assert_eq!(
            t.text_fields(),
            [
                ("model", "gpt-transcribe".to_string()),
                ("keywords[]", "Draft".to_string()),
            ]
        );
    }

    #[test]
    fn groq_requests_whisper_large_v3_turbo() {
        let t = groq(&["Draft"]);
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
    fn groq_joins_the_terms_into_one_prompt() {
        assert_eq!(
            groq(&["Draft", "Parakeet"]).text_fields(),
            [
                ("model", "whisper-large-v3-turbo".to_string()),
                ("prompt", "Draft, Parakeet".to_string()),
            ]
        );
    }

    #[test]
    fn no_vocabulary_sends_no_vocabulary_field() {
        assert_eq!(openai(&[]).text_fields().len(), 1);
        assert_eq!(groq(&[]).text_fields().len(), 1);
    }

    #[test]
    fn no_language_is_ever_sent() {
        for t in [openai(&["Draft"]), groq(&["Draft"])] {
            assert!(t
                .text_fields()
                .iter()
                .all(|(field, _)| !field.starts_with("language")));
        }
    }
}
