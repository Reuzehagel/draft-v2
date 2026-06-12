// Push-to-command LLM call (issue #4): the spoken instruction goes to a fast
// chat model and the *answer* is what gets pasted. This is a second pipeline
// alongside dictation, not a stage in it — and unlike the dictation path,
// latency here is expected: the user explicitly asked the model to think.
//
// Groq + Llama 3.3 70B per the umbrella issue's research: TTFT-bound,
// ~0.3s for a sentence, ~1s for a paragraph. Reuses the existing Groq API
// key from the keyring.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

const ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODEL: &str = "llama-3.3-70b-versatile";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The contract that makes answers paste-safe: the model's output lands
/// verbatim at the user's cursor, so anything conversational is a defect.
const SYSTEM_PROMPT: &str = "You are the command mode of a desktop dictation app. \
The user held a hotkey and spoke an instruction; your entire reply is inserted \
verbatim at their cursor in whatever application they are using. Output only the \
requested text — no preamble, no explanation, no surrounding quotes, and no \
markdown fences unless the user explicitly asked for markdown or code formatting.";

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

pub fn run_command(api_key: &str, instruction: &str) -> Result<String> {
    let client = client()?;
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": instruction },
        ],
        "temperature": 0.3,
        "max_tokens": 2048,
    });
    let resp = client
        .post(ENDPOINT)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .context("POST to Groq chat")?;

    let status = resp.status();
    let text = resp.text().context("read Groq chat response")?;
    if !status.is_success() {
        return Err(anyhow!(
            "Groq chat returned {status}: {}",
            crate::transcribe::clip_body(&text, 500)
        ));
    }
    let parsed: ChatResponse = serde_json::from_str(&text).with_context(|| {
        format!(
            "parse Groq chat response: {}",
            crate::transcribe::clip_body(&text, 200)
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
