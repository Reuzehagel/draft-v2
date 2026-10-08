// The answer run: once Draft knows what to ask a chat model, ask the Chat
// backend, record the answer in history, and paste it. Today only a `Command`
// session reaches it (after transcription); the Selection and Quick fix are
// added here next, which is why it sits behind two seams — the Desk and the
// Chat transport — and is tested against fakes of both.
//
// History is recorded *before* the paste, as on the dictation path: if the
// paste is swallowed or lands in the wrong window, that record is the only
// surviving copy. Do not reorder. History itself comes in as a `record`
// closure rather than a third trait: it is the shared core's file, not the
// focused app, and a test must watch the write without touching the user's.

use anyhow::{anyhow, Result};

use crate::config::ChatBackend;
use crate::llm::{self, Chat, ChatKeys, ChatTransport};
use crate::paste;
use crate::session::Outcome;

/// The label an answer is recorded under in history.
const HISTORY_LABEL: &str = "command";

/// The answer run's view of the focused app. For now it only pastes.
pub trait Desk {
    /// Paste `text` at the cursor. `on_sent` fires once the paste has been
    /// handed to the OS, before any housekeeping — exactly once on `Ok`,
    /// never on `Err`.
    fn paste(&mut self, text: &str, on_sent: impl FnOnce()) -> Result<()>;
}

/// The real Desk: `paste`'s clipboard or unicode path, under its lock.
pub struct SystemDesk {
    pub mode: paste::PasteMode,
    pub restore_clipboard: bool,
}

impl Desk for SystemDesk {
    fn paste(&mut self, text: &str, on_sent: impl FnOnce()) -> Result<()> {
        paste::deliver_text(text, self.mode, self.restore_clipboard, on_sent)
    }
}

/// What to ask, who to ask it of, and how the answer is pasted.
pub struct Ask<'a> {
    /// The Chat backend and model the config names; `None` leaves it to
    /// Draft — see [`Chat::resolve`].
    pub backend: Option<ChatBackend>,
    pub model: Option<&'a str>,
    /// Every Chat backend's key, as stored: the default backend follows them.
    pub keys: &'a ChatKeys,
    pub instruction: &'a str,
    /// The cosmetic trailing space after the paste — never recorded.
    pub append_space: bool,
}

/// Run `ask` to its [`Outcome`]. `record` writes history (text, label);
/// `on_sent` fires the moment the paste is sent, so `Delivered` can be
/// reported before the Desk finishes its clipboard housekeeping — the return
/// value is the same verdict, once that housekeeping is done.
pub fn run(
    ask: &Ask,
    desk: &mut impl Desk,
    transport: &impl ChatTransport,
    record: impl FnOnce(&str, &str) -> Result<()>,
    on_sent: impl FnOnce(),
) -> Outcome {
    answer(ask, desk, transport, record, on_sent).unwrap_or_else(|e| {
        tracing::error!(error = %format!("{e:#}"), "command failed");
        Outcome::Failed
    })
}

fn answer(
    ask: &Ask,
    desk: &mut impl Desk,
    transport: &impl ChatTransport,
    record: impl FnOnce(&str, &str) -> Result<()>,
    on_sent: impl FnOnce(),
) -> Result<Outcome> {
    let chat = Chat::resolve(ask.backend, ask.model, |b| ask.keys.get(b).is_some());
    // The chosen backend is asked or nothing is: a key stored for the other
    // one is no reason to swap.
    let key = ask.keys.get(chat.backend).ok_or_else(|| {
        anyhow!(
            "push-to-command needs a {} API key — add one under Settings > Commands",
            chat.backend.label()
        )
    })?;
    tracing::info!(backend = chat.backend.label(), model = %chat.model, "asking");
    let answer = llm::run_command(transport, &chat, key, ask.instruction)?;

    // A refusing or silent model is not a failure — there is just nothing to
    // paste, and an empty entry is not worth recording.
    if answer.trim().is_empty() {
        tracing::info!("command answer empty; nothing to paste");
        return Ok(Outcome::Empty);
    }
    if let Err(e) = record(&answer, HISTORY_LABEL) {
        tracing::warn!(error = %e, "failed to record answer in history");
    }

    let mut out = answer;
    if ask.append_space {
        out.push(' ');
    }
    tracing::info!(chars = out.len(), "command answered");
    desk.paste(&out, on_sent)
        .map_err(|e| e.context("paste failed"))?;
    Ok(Outcome::Delivered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use std::cell::RefCell;

    /// Everything the run did to the outside world, in order.
    #[derive(Debug, PartialEq)]
    enum Event {
        Asked {
            endpoint: String,
            key: String,
            body: serde_json::Value,
        },
        Recorded(String, String),
        Pasted(String),
    }

    type Log = RefCell<Vec<Event>>;

    struct FakeDesk<'a> {
        log: &'a Log,
        fail: bool,
    }

    impl Desk for FakeDesk<'_> {
        fn paste(&mut self, text: &str, on_sent: impl FnOnce()) -> Result<()> {
            if self.fail {
                return Err(anyhow!("the window went away"));
            }
            self.log.borrow_mut().push(Event::Pasted(text.into()));
            on_sent();
            Ok(())
        }
    }

    struct FakeTransport<'a> {
        log: &'a Log,
        status: StatusCode,
        body: String,
    }

    impl ChatTransport for FakeTransport<'_> {
        fn post(
            &self,
            endpoint: &str,
            key: &str,
            body: &serde_json::Value,
        ) -> Result<(StatusCode, String)> {
            self.log.borrow_mut().push(Event::Asked {
                endpoint: endpoint.into(),
                key: key.into(),
                body: body.clone(),
            });
            Ok((self.status, self.body.clone()))
        }
    }

    fn answering<'a>(log: &'a Log, content: &str) -> FakeTransport<'a> {
        let body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        });
        FakeTransport {
            log,
            status: StatusCode::OK,
            body: body.to_string(),
        }
    }

    static GROQ_ONLY: ChatKeys = ChatKeys {
        groq: Some(String::new()),
        cerebras: None,
    };

    static NO_KEYS: ChatKeys = ChatKeys {
        groq: None,
        cerebras: None,
    };

    /// Asks Groq: with only a Groq key stored, that is Draft's default.
    fn ask(append_space: bool) -> Ask<'static> {
        Ask {
            backend: None,
            model: None,
            keys: &GROQ_ONLY,
            instruction: "say hi",
            append_space,
        }
    }

    fn both_keys() -> ChatKeys {
        ChatKeys {
            groq: Some("gsk_test".into()),
            cerebras: Some("csk_test".into()),
        }
    }

    /// The one request `ask` sent: endpoint, key, body.
    fn asked(ask: &Ask) -> (String, String, serde_json::Value) {
        let log = Log::default();
        go(ask, &log, &answering(&log, "Hi."), false);
        let first = log.into_inner().into_iter().next();
        let Some(Event::Asked {
            endpoint,
            key,
            body,
        }) = first
        else {
            panic!("the chat backend was not asked first: {first:?}");
        };
        (endpoint, key, body)
    }

    /// Runs `ask` with history recorded into `log`; returns the outcome and
    /// whether `on_sent` fired.
    fn go(ask: &Ask, log: &Log, transport: &FakeTransport, fail_paste: bool) -> (Outcome, bool) {
        let mut desk = FakeDesk {
            log,
            fail: fail_paste,
        };
        let sent = RefCell::new(false);
        let outcome = run(
            ask,
            &mut desk,
            transport,
            |text, label| {
                log.borrow_mut()
                    .push(Event::Recorded(text.into(), label.into()));
                Ok(())
            },
            || *sent.borrow_mut() = true,
        );
        (outcome, sent.into_inner())
    }

    /// The log without the request, which most tests don't care about.
    fn effects(log: Log) -> Vec<Event> {
        log.into_inner()
            .into_iter()
            .filter(|e| !matches!(e, Event::Asked { .. }))
            .collect()
    }

    #[test]
    fn an_answer_is_recorded_before_it_is_pasted() {
        let log = Log::default();
        let (outcome, sent) = go(&ask(true), &log, &answering(&log, " Hi there.\n"), false);
        assert_eq!(outcome, Outcome::Delivered);
        assert!(sent);
        assert_eq!(
            effects(log),
            [
                Event::Recorded("Hi there.".into(), "command".into()),
                Event::Pasted("Hi there. ".into()),
            ]
        );
    }

    #[test]
    fn the_instruction_is_what_is_asked() {
        let (_, _, body) = asked(&ask(false));
        assert_eq!(body["messages"][1]["content"], "say hi");
    }

    #[test]
    fn each_backend_is_asked_at_its_endpoint_with_its_own_key() {
        let keys = both_keys();
        for (backend, endpoint, key) in [
            (ChatBackend::Groq, llm::GROQ_ENDPOINT, "gsk_test"),
            (ChatBackend::Cerebras, llm::CEREBRAS_ENDPOINT, "csk_test"),
        ] {
            let chosen = Ask {
                backend: Some(backend),
                keys: &keys,
                ..ask(false)
            };
            let (e, k, body) = asked(&chosen);
            assert_eq!((e.as_str(), k.as_str()), (endpoint, key), "{backend:?}");
            assert_eq!(body["model"], llm::default_model(backend), "{backend:?}");
        }
    }

    #[test]
    fn with_nothing_chosen_cerebras_is_asked_unless_only_groq_has_a_key() {
        let keys = both_keys();
        let (endpoint, key, body) = asked(&Ask {
            keys: &keys,
            ..ask(false)
        });
        assert_eq!(endpoint, llm::CEREBRAS_ENDPOINT);
        assert_eq!(key, "csk_test");
        assert_eq!(body["model"], llm::QWEN_3_8_27B);

        let (endpoint, _, body) = asked(&ask(false));
        assert_eq!(endpoint, llm::GROQ_ENDPOINT);
        assert_eq!(body["model"], llm::GROQ_GPT_OSS_120B);
    }

    /// Each listed model carries the fields that keep its reasoning out of the
    /// answer, and none of the others' fields.
    #[test]
    fn each_listed_model_carries_its_reasoning_fields() {
        let keys = both_keys();
        let fields = ["reasoning_effort", "reasoning_format", "include_reasoning"];
        for (backend, model, want) in [
            (
                ChatBackend::Cerebras,
                llm::QWEN_3_8_27B,
                serde_json::json!({ "reasoning_effort": "none" }),
            ),
            (
                ChatBackend::Cerebras,
                llm::CEREBRAS_GPT_OSS_120B,
                serde_json::json!({ "reasoning_effort": "low", "reasoning_format": "hidden" }),
            ),
            (
                ChatBackend::Groq,
                llm::GROQ_GPT_OSS_120B,
                serde_json::json!({ "reasoning_effort": "low", "include_reasoning": false }),
            ),
        ] {
            let chosen = Ask {
                backend: Some(backend),
                model: Some(model),
                keys: &keys,
                ..ask(false)
            };
            let (_, _, body) = asked(&chosen);
            assert_eq!(body["model"], model);
            let got: serde_json::Map<_, _> = fields
                .iter()
                .filter_map(|&f| body.get(f).map(|v| (f.to_string(), v.clone())))
                .collect();
            assert_eq!(serde_json::Value::Object(got), want, "{model}");
        }
    }

    #[test]
    fn a_model_draft_does_not_list_is_sent_as_is_with_no_extra_fields() {
        let keys = both_keys();
        for backend in [ChatBackend::Groq, ChatBackend::Cerebras] {
            let chosen = Ask {
                backend: Some(backend),
                model: Some("vendor/next-model"),
                keys: &keys,
                ..ask(false)
            };
            let (_, _, body) = asked(&chosen);
            let mut fields: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
            fields.sort();
            assert_eq!(
                fields,
                ["max_completion_tokens", "messages", "model", "temperature"],
                "{backend:?}"
            );
            assert_eq!(body["model"], "vendor/next-model");
        }
    }

    #[test]
    fn an_answer_that_trims_to_empty_is_neither_recorded_nor_pasted() {
        let log = Log::default();
        let (outcome, sent) = go(&ask(true), &log, &answering(&log, "  \n "), false);
        assert_eq!(outcome, Outcome::Empty);
        assert!(!sent);
        assert_eq!(effects(log), []);
    }

    #[test]
    fn a_non_2xx_response_fails_without_recording_or_pasting() {
        let log = Log::default();
        let transport = FakeTransport {
            log: &log,
            status: StatusCode::TOO_MANY_REQUESTS,
            body: r#"{"error":{"message":"Rate limit reached","code":"rate_limit_exceeded"}}"#
                .into(),
        };
        let (outcome, sent) = go(&ask(false), &log, &transport, false);
        assert_eq!(outcome, Outcome::Failed);
        assert!(!sent);
        assert_eq!(effects(log), []);
    }

    #[test]
    fn a_model_not_found_response_fails_naming_the_model() {
        let log = Log::default();
        let transport = FakeTransport {
            log: &log,
            status: StatusCode::NOT_FOUND,
            body: r#"{"error":{"message":"The model does not exist.","code":"model_not_found"}}"#
                .into(),
        };
        let mut desk = FakeDesk {
            log: &log,
            fail: false,
        };
        assert_eq!(
            run(&ask(false), &mut desk, &transport, |_, _| Ok(()), || {}),
            Outcome::Failed
        );
        let err = answer(&ask(false), &mut desk, &transport, |_, _| Ok(()), || {}).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&format!("`{}`", llm::GROQ_GPT_OSS_120B)),
            "{msg}"
        );
        assert!(msg.contains("Settings > Commands"), "{msg}");
        assert_eq!(effects(log), []);
    }

    #[test]
    fn a_failed_paste_fails_but_keeps_the_history_entry() {
        let log = Log::default();
        let (outcome, sent) = go(&ask(false), &log, &answering(&log, "Hi."), true);
        assert_eq!(outcome, Outcome::Failed);
        assert!(!sent);
        assert_eq!(
            effects(log),
            [Event::Recorded("Hi.".into(), "command".into())]
        );
    }

    #[test]
    fn without_a_key_nothing_is_asked() {
        let log = Log::default();
        let no_key = Ask {
            keys: &NO_KEYS,
            ..ask(false)
        };
        let (outcome, _) = go(&no_key, &log, &answering(&log, "Hi."), false);
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(log.into_inner(), []);
    }

    /// A chosen backend is asked or nothing is: a key on the other one is no
    /// reason to swap.
    #[test]
    fn a_chosen_backend_without_a_key_fails_naming_its_key() {
        let log = Log::default();
        let cerebras = Ask {
            backend: Some(ChatBackend::Cerebras),
            ..ask(false)
        };
        let mut desk = FakeDesk {
            log: &log,
            fail: false,
        };
        let transport = answering(&log, "Hi.");
        let err = answer(&cerebras, &mut desk, &transport, |_, _| Ok(()), || {}).unwrap_err();
        assert!(format!("{err}").contains("Cerebras API key"), "{err}");
        assert_eq!(log.into_inner(), []);
    }

    #[test]
    fn a_history_write_that_fails_still_pastes() {
        let log = Log::default();
        let mut desk = FakeDesk {
            log: &log,
            fail: false,
        };
        let outcome = run(
            &ask(false),
            &mut desk,
            &answering(&log, "Hi."),
            |_, _| Err(anyhow!("disk full")),
            || {},
        );
        assert_eq!(outcome, Outcome::Delivered);
        assert_eq!(effects(log), [Event::Pasted("Hi.".into())]);
    }
}
