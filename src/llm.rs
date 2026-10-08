// Push-to-command LLM call (issue #4): the spoken instruction goes to a fast
// chat model and the *answer* is what gets pasted. This is a second pipeline
// alongside dictation, not a stage in it — and unlike the dictation path,
// latency here is expected: the user explicitly asked the model to think.
//
// The Chat backend is Groq or Cerebras, both OpenAI-compatible, and the user
// chooses the model on it (#111, ADR 0004): vendors retire chat models under
// us, as Groq did Llama 3.3 70B on 2026-08-16 (#109). Draft lists the models
// it knows how to drive, each with the request fields that keep its reasoning
// out of the answer; any other id is sent as-is, with none. A retired model is
// an error naming it, never a quiet swap to another.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::ChatBackend;
use crate::secrets;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The contract that makes answers paste-safe: the model's output lands
/// verbatim at the user's cursor, so anything conversational is a defect.
const SYSTEM_PROMPT: &str = "You are the command mode of a desktop dictation app. \
The user held a hotkey and spoke an instruction; your entire reply is inserted \
verbatim at their cursor in whatever application they are using. Output only the \
requested text — no preamble, no explanation, no surrounding quotes, and no \
markdown fences unless the user explicitly asked for markdown or code formatting.";

pub const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const CEREBRAS_ENDPOINT: &str = "https://api.cerebras.ai/v1/chat/completions";

fn endpoint(backend: ChatBackend) -> &'static str {
    match backend {
        ChatBackend::Groq => GROQ_ENDPOINT,
        ChatBackend::Cerebras => CEREBRAS_ENDPOINT,
    }
}

// The listed models' ids, for the tests that name one.
pub const QWEN_3_8_27B: &str = "qwen-3.8-27b";
pub const CEREBRAS_GPT_OSS_120B: &str = "gpt-oss-120b";
pub const GROQ_GPT_OSS_120B: &str = "openai/gpt-oss-120b";

/// A value for one of a model's reasoning fields.
#[derive(Clone, Copy, Debug)]
enum Field {
    Str(&'static str),
    Bool(bool),
}

/// A model Draft lists for a backend, and how to drive it.
#[derive(Debug)]
pub struct Model {
    pub backend: ChatBackend,
    pub id: &'static str,
    /// Request fields that keep reasoning out of the answer. Checked on
    /// 2026-10-08 against https://console.groq.com/docs/reasoning and
    /// https://inference-docs.cerebras.ai/capabilities/reasoning.
    reasoning: &'static [(&'static str, Field)],
}

/// Each backend's models, its default first.
const MODELS: &[Model] = &[
    // Qwen's reasoning switches off outright; it doesn't support
    // `reasoning_format: hidden`.
    Model {
        backend: ChatBackend::Cerebras,
        id: QWEN_3_8_27B,
        reasoning: &[("reasoning_effort", Field::Str("none"))],
    },
    // GPT-OSS always reasons: at low effort, and kept out of the response.
    Model {
        backend: ChatBackend::Cerebras,
        id: CEREBRAS_GPT_OSS_120B,
        reasoning: &[
            ("reasoning_effort", Field::Str("low")),
            ("reasoning_format", Field::Str("hidden")),
        ],
    },
    // Groq says the same with `include_reasoning`, and rejects
    // `reasoning_format` alongside it.
    Model {
        backend: ChatBackend::Groq,
        id: GROQ_GPT_OSS_120B,
        reasoning: &[
            ("reasoning_effort", Field::Str("low")),
            ("include_reasoning", Field::Bool(false)),
        ],
    },
];

/// The models Draft lists for `backend`, its default first.
pub fn models(backend: ChatBackend) -> impl Iterator<Item = &'static Model> {
    MODELS.iter().filter(move |m| m.backend == backend)
}

/// The model Draft asks on `backend` until the user chooses one.
pub fn default_model(backend: ChatBackend) -> &'static str {
    models(backend)
        .next()
        .expect("every Chat backend lists a model")
        .id
}

/// The listed model `id` on `backend`, if Draft lists it.
pub fn listed(backend: ChatBackend, id: &str) -> Option<&'static Model> {
    models(backend).find(|m| m.id == id)
}

/// The backend and model one answer run asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    pub backend: ChatBackend,
    pub model: String,
}

impl Chat {
    /// What the config's choice comes to, given which backends have a key.
    /// With no backend chosen, Draft picks one by the keys and asks its
    /// default model; a chosen backend with no model asks its default too. A
    /// model is read only alongside a chosen backend — on its own it may name
    /// a model the picked backend doesn't serve.
    pub fn resolve(
        backend: Option<ChatBackend>,
        model: Option<&str>,
        has_key: impl Fn(ChatBackend) -> bool,
    ) -> Chat {
        let (backend, model) = match backend {
            Some(b) => (b, model),
            None => (
                ChatBackend::default_for(
                    has_key(ChatBackend::Groq),
                    has_key(ChatBackend::Cerebras),
                ),
                None,
            ),
        };
        Chat {
            backend,
            model: model.map_or_else(|| default_model(backend).to_string(), str::to_string),
        }
    }
}

/// Each Chat backend's key as stored.
#[derive(Clone, Debug, Default)]
pub struct ChatKeys {
    pub groq: Option<String>,
    pub cerebras: Option<String>,
}

impl ChatKeys {
    pub fn load() -> Self {
        ChatKeys {
            groq: secrets::load_key(ChatBackend::Groq),
            cerebras: secrets::load_key(ChatBackend::Cerebras),
        }
    }

    pub fn get(&self, backend: ChatBackend) -> Option<&str> {
        match backend {
            ChatBackend::Groq => self.groq.as_deref(),
            ChatBackend::Cerebras => self.cerebras.as_deref(),
        }
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

/// The answer run's other seam: POST a JSON body to an endpoint with a bearer
/// key, and hand back the status and body for [`read_response`] to judge.
pub trait ChatTransport {
    fn post(
        &self,
        endpoint: &str,
        key: &str,
        body: &serde_json::Value,
    ) -> Result<(reqwest::StatusCode, String)>;
}

/// The real transport: the shared blocking HTTP client.
pub struct HttpTransport;

impl ChatTransport for HttpTransport {
    fn post(
        &self,
        endpoint: &str,
        key: &str,
        body: &serde_json::Value,
    ) -> Result<(reqwest::StatusCode, String)> {
        let resp = client()?
            .post(endpoint)
            .bearer_auth(key)
            .json(body)
            .send()
            .with_context(|| format!("POST to {endpoint}"))?;
        let status = resp.status();
        let text = resp.text().context("read chat response")?;
        Ok((status, text))
    }
}

/// One client for the process lifetime — rebuilding it per call would pay
/// TLS/pool setup on a path whose whole point is low latency.
fn client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let built = crate::transcribe::http_client(REQUEST_TIMEOUT)?;
    Ok(CLIENT.get_or_init(|| built))
}

pub fn run_command(
    transport: &impl ChatTransport,
    chat: &Chat,
    api_key: &str,
    instruction: &str,
) -> Result<String> {
    let (status, text) = transport.post(
        endpoint(chat.backend),
        api_key,
        &request_body(chat, instruction),
    )?;
    read_response(chat, status, &text)
}

fn request_body(chat: &Chat, instruction: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": chat.model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": instruction },
        ],
        "temperature": 0.3,
        // Counts the reasoning tokens too; at low effort that leaves ample room.
        "max_completion_tokens": 2048,
    });
    // An id Draft doesn't list carries no extra fields: Draft can't know
    // which ones that model accepts.
    if let Some(model) = listed(chat.backend, &chat.model) {
        for &(name, value) in model.reasoning {
            body[name] = match value {
                Field::Str(s) => s.into(),
                Field::Bool(b) => b.into(),
            };
        }
    }
    body
}

/// The answer to paste, or why there isn't one. A retired model gets an error
/// naming it and where to choose another, so the next retirement is
/// diagnosable — and fixable — from the log alone.
fn read_response(chat: &Chat, status: reqwest::StatusCode, text: &str) -> Result<String> {
    let backend = chat.backend.label();
    if !status.is_success() {
        // Groq nests the code under `error`; Cerebras puts it at the top.
        let envelope = serde_json::from_str::<serde_json::Value>(text).unwrap_or_default();
        let code = envelope["error"]["code"]
            .as_str()
            .or_else(|| envelope["code"].as_str());
        if matches!(code, Some("model_not_found" | "model_decommissioned")) {
            // The vendor's own text rides along: `model_not_found` also covers
            // a key that lacks access, which only its message tells apart.
            return Err(anyhow!(
                "{backend} chat returned {status}: the push-to-command model `{}` is no \
                 longer served — choose another under Settings > Commands ({})",
                chat.model,
                crate::transcribe::clip_body(text, 500)
            ));
        }
        return Err(anyhow!(
            "{backend} chat returned {status}: {}",
            crate::transcribe::clip_body(text, 500)
        ));
    }
    let parsed: ChatResponse = serde_json::from_str(text).with_context(|| {
        format!(
            "parse {backend} chat response: {}",
            crate::transcribe::clip_body(text, 200)
        )
    })?;
    let answer = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();
    Ok(answer.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    fn chat(backend: ChatBackend, model: &str) -> Chat {
        Chat {
            backend,
            model: model.into(),
        }
    }

    fn groq() -> Chat {
        chat(ChatBackend::Groq, GROQ_GPT_OSS_120B)
    }

    #[test]
    fn every_backend_lists_its_default_model_first() {
        assert_eq!(default_model(ChatBackend::Cerebras), QWEN_3_8_27B);
        assert_eq!(default_model(ChatBackend::Groq), GROQ_GPT_OSS_120B);
    }

    #[test]
    fn with_nothing_chosen_draft_picks_by_the_keys_stored() {
        let resolve = |groq: bool, cerebras: bool| {
            Chat::resolve(None, None, |b| match b {
                ChatBackend::Groq => groq,
                ChatBackend::Cerebras => cerebras,
            })
        };
        let qwen = chat(ChatBackend::Cerebras, QWEN_3_8_27B);
        assert_eq!(resolve(false, false), qwen);
        assert_eq!(resolve(false, true), qwen);
        assert_eq!(resolve(true, true), qwen);
        assert_eq!(resolve(true, false), groq());
    }

    #[test]
    fn a_chosen_backend_and_model_win_over_the_keys() {
        assert_eq!(
            Chat::resolve(
                Some(ChatBackend::Cerebras),
                Some(CEREBRAS_GPT_OSS_120B),
                |_| false
            ),
            chat(ChatBackend::Cerebras, CEREBRAS_GPT_OSS_120B)
        );
        assert_eq!(
            Chat::resolve(Some(ChatBackend::Groq), None, |b| b
                == ChatBackend::Cerebras),
            groq()
        );
        assert_eq!(
            Chat::resolve(None, Some(CEREBRAS_GPT_OSS_120B), |_| false),
            chat(ChatBackend::Cerebras, QWEN_3_8_27B)
        );
    }

    #[test]
    fn a_request_names_the_model_and_carries_only_its_own_reasoning_fields() {
        let body = request_body(&groq(), "make it shorter");
        assert_eq!(body["model"], GROQ_GPT_OSS_120B);
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["include_reasoning"], false);
        // Groq rejects `reasoning_format` alongside `include_reasoning`.
        assert!(body.get("reasoning_format").is_none());
        assert_eq!(body["messages"][1]["content"], "make it shorter");

        let other = request_body(&chat(ChatBackend::Groq, "vendor/next-model"), "x");
        assert!(other.get("reasoning_effort").is_none());
        assert!(other.get("include_reasoning").is_none());
    }

    #[test]
    fn only_the_content_is_pasted_never_the_reasoning() {
        let text = r#"{"choices":[{"message":{"role":"assistant",
            "content":"  Shorter text.\n","reasoning":"The user wants brevity."}}]}"#;
        assert_eq!(
            read_response(&groq(), StatusCode::OK, text).unwrap(),
            "Shorter text."
        );
    }

    #[test]
    fn a_decommissioned_model_is_named_in_the_error() {
        let text = r#"{"error":{"message":"The model `openai/gpt-oss-120b` has been decommissioned and is no longer supported.","type":"invalid_request_error","code":"model_decommissioned"}}"#;
        let err = read_response(&groq(), StatusCode::BAD_REQUEST, text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&format!("model `{GROQ_GPT_OSS_120B}`")),
            "{msg}"
        );
        assert!(msg.contains("no longer served"), "{msg}");
        assert!(msg.contains("Settings > Commands"), "{msg}");
    }

    /// Cerebras's envelope is flat: the code is at the top, not under `error`.
    #[test]
    fn a_model_cerebras_does_not_serve_is_named_in_the_error() {
        let text = r#"{"message":"Model qwen-3.8-27b does not exist or you do not have access to it.","type":"not_found_error","param":"model","code":"model_not_found"}"#;
        let qwen = chat(ChatBackend::Cerebras, QWEN_3_8_27B);
        let err = read_response(&qwen, StatusCode::NOT_FOUND, text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.starts_with("Cerebras chat returned 404"), "{msg}");
        assert!(msg.contains(&format!("model `{QWEN_3_8_27B}`")), "{msg}");
        assert!(msg.contains("Settings > Commands"), "{msg}");
    }

    #[test]
    fn any_other_failure_keeps_the_status_and_body() {
        let text = r#"{"error":{"message":"Rate limit reached","type":"tokens","code":"rate_limit_exceeded"}}"#;
        let err = read_response(&groq(), StatusCode::TOO_MANY_REQUESTS, text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.starts_with("Groq chat returned 429"), "{msg}");
        assert!(msg.contains("Rate limit reached"), "{msg}");
        assert!(!msg.contains("no longer served"), "{msg}");
    }

    /// A tool, not a test: push-to-command's real request, without a
    /// microphone. Sends `DRAFT_COMMAND` (or a stock instruction) to the Chat
    /// backend and model the config names, with the stored key, and prints
    /// the answer — so a model or backend change can be checked live from a
    /// terminal.
    #[test]
    #[ignore]
    fn live() {
        let instruction = std::env::var("DRAFT_COMMAND")
            .unwrap_or_else(|_| "Write one short sentence about coffee.".into());
        let cfg = draft::config::Config::load().expect("config loads");
        let keys = ChatKeys::load();
        let chat = Chat::resolve(cfg.chat_backend, cfg.chat_model.as_deref(), |b| {
            keys.get(b).is_some()
        });
        let key = keys.get(chat.backend).unwrap_or_else(|| {
            panic!(
                "no {} key: set one in Settings or {}",
                chat.backend.label(),
                secrets::env_var(chat.backend).unwrap_or_default()
            )
        });
        println!("{} / {}", chat.backend.label(), chat.model);
        println!(
            "{}",
            run_command(&HttpTransport, &chat, key, &instruction).unwrap()
        );
    }
}
