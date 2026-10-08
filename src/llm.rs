// Push-to-command LLM call (issue #4): the spoken instruction goes to a fast
// chat model and the *answer* is what gets pasted. This is a second pipeline
// alongside dictation, not a stage in it — and unlike the dictation path,
// latency here is expected: the user explicitly asked the model to think.
//
// Groq + GPT-OSS 120B (#109): Llama 3.3 70B, the original pick, was retired
// for free and developer tiers on 2026-08-16. GPT-OSS is a reasoning model,
// so it runs at low effort and its reasoning is never returned — only the
// answer may reach the paste. Reuses the existing Groq API key from the
// keyring.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

const ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const MODEL: &str = "openai/gpt-oss-120b";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The contract that makes answers paste-safe: the model's output lands
/// verbatim at the user's cursor, so anything conversational is a defect.
const SYSTEM_PROMPT: &str = "You are the command mode of a desktop dictation app. \
The user held a hotkey and spoke an instruction; your entire reply is inserted \
verbatim at their cursor in whatever application they are using. Output only the \
requested text — no preamble, no explanation, no surrounding quotes, and no \
markdown fences unless the user explicitly asked for markdown or code formatting.";

/// Groq's error envelope. Only `code` is read: it is what tells a retired
/// model apart from every other failure.
#[derive(Deserialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    code: Option<String>,
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
            .context("POST to Groq chat")?;
        let status = resp.status();
        let text = resp.text().context("read Groq chat response")?;
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
    api_key: &str,
    instruction: &str,
) -> Result<String> {
    let (status, text) = transport.post(ENDPOINT, api_key, &request_body(instruction))?;
    read_response(status, &text)
}

fn request_body(instruction: &str) -> serde_json::Value {
    serde_json::json!({
        "model": MODEL,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": instruction },
        ],
        "temperature": 0.3,
        // Counts the reasoning tokens too; at low effort that leaves ample room.
        "max_tokens": 2048,
        "reasoning_effort": "low",
        "include_reasoning": false,
    })
}

/// The answer to paste, or why there isn't one. A retired model gets an error
/// naming it, so the next retirement is diagnosable from the log alone.
fn read_response(status: reqwest::StatusCode, text: &str) -> Result<String> {
    if !status.is_success() {
        let code = serde_json::from_str::<ErrorResponse>(text)
            .ok()
            .and_then(|e| e.error.code);
        if matches!(
            code.as_deref(),
            Some("model_not_found" | "model_decommissioned")
        ) {
            // Groq's own text rides along: `model_not_found` also covers a
            // key that lacks access, which only its message tells apart.
            return Err(anyhow!(
                "Groq chat returned {status}: the push-to-command model `{MODEL}` is no longer served ({})",
                crate::transcribe::clip_body(text, 500)
            ));
        }
        return Err(anyhow!(
            "Groq chat returned {status}: {}",
            crate::transcribe::clip_body(text, 500)
        ));
    }
    let parsed: ChatResponse = serde_json::from_str(text).with_context(|| {
        format!(
            "parse Groq chat response: {}",
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

    #[test]
    fn the_request_names_gpt_oss_with_low_effort_and_no_reasoning() {
        let body = request_body("make it shorter");
        assert_eq!(body["model"], "openai/gpt-oss-120b");
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["include_reasoning"], false);
        // Groq rejects `reasoning_format` alongside `include_reasoning`.
        assert!(body.get("reasoning_format").is_none());
        assert_eq!(body["messages"][1]["content"], "make it shorter");
    }

    #[test]
    fn only_the_content_is_pasted_never_the_reasoning() {
        let text = r#"{"choices":[{"message":{"role":"assistant",
            "content":"  Shorter text.\n","reasoning":"The user wants brevity."}}]}"#;
        assert_eq!(
            read_response(StatusCode::OK, text).unwrap(),
            "Shorter text."
        );
    }

    #[test]
    fn a_decommissioned_model_is_named_in_the_error() {
        let text = r#"{"error":{"message":"The model `openai/gpt-oss-120b` has been decommissioned and is no longer supported.","type":"invalid_request_error","code":"model_decommissioned"}}"#;
        let err = read_response(StatusCode::BAD_REQUEST, text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("openai/gpt-oss-120b"), "{msg}");
        assert!(msg.contains("no longer served"), "{msg}");
    }

    #[test]
    fn an_unknown_model_is_named_in_the_error() {
        let text = r#"{"error":{"message":"The model `openai/gpt-oss-120b` does not exist or you do not have access to it.","type":"invalid_request_error","code":"model_not_found"}}"#;
        let err = read_response(StatusCode::NOT_FOUND, text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("openai/gpt-oss-120b"), "{msg}");
        assert!(msg.contains("no longer served"), "{msg}");
    }

    #[test]
    fn any_other_failure_keeps_the_status_and_body() {
        let text = r#"{"error":{"message":"Rate limit reached","type":"tokens","code":"rate_limit_exceeded"}}"#;
        let err = read_response(StatusCode::TOO_MANY_REQUESTS, text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.starts_with("Groq chat returned 429"), "{msg}");
        assert!(msg.contains("Rate limit reached"), "{msg}");
        assert!(!msg.contains("no longer served"), "{msg}");
    }

    /// A tool, not a test: push-to-command's real request, without a
    /// microphone. Sends `DRAFT_COMMAND` (or a stock instruction) with the
    /// stored Groq key and prints the answer, so a model or backend change can
    /// be checked live from a terminal.
    #[test]
    #[ignore]
    fn live() {
        let instruction = std::env::var("DRAFT_COMMAND")
            .unwrap_or_else(|_| "Write one short sentence about coffee.".into());
        let key = draft::secrets::load_key(draft::config::Provider::Groq)
            .expect("no Groq key: set one in Settings or GROQ_API_KEY");
        println!(
            "{}",
            run_command(&HttpTransport, &key, &instruction).unwrap()
        );
    }
}
