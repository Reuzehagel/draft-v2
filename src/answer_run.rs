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

use crate::llm::{self, ChatTransport};
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

/// What to ask, and how the answer is pasted.
pub struct Ask<'a> {
    /// The Chat backend's key; `None` when none is stored.
    pub key: Option<&'a str>,
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
    let key = ask.key.ok_or_else(|| {
        anyhow!(
            "push-to-command needs a Groq API key — add one under \
             Settings > Transcription with Groq selected"
        )
    })?;
    let answer = llm::run_command(transport, key, ask.instruction)?;

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
        Asked(serde_json::Value),
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
            _endpoint: &str,
            _key: &str,
            body: &serde_json::Value,
        ) -> Result<(StatusCode, String)> {
            self.log.borrow_mut().push(Event::Asked(body.clone()));
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

    fn ask(append_space: bool) -> Ask<'static> {
        Ask {
            key: Some("gsk_test"),
            instruction: "say hi",
            append_space,
        }
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
            .filter(|e| !matches!(e, Event::Asked(_)))
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
        let log = Log::default();
        go(&ask(false), &log, &answering(&log, "Hi."), false);
        let Event::Asked(body) = &log.borrow()[0] else {
            panic!("the chat backend was not asked first: {:?}", log.borrow());
        };
        assert_eq!(body["messages"][1]["content"], "say hi");
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
        assert!(msg.contains(llm::MODEL), "{msg}");
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
            key: None,
            ..ask(false)
        };
        let (outcome, _) = go(&no_key, &log, &answering(&log, "Hi."), false);
        assert_eq!(outcome, Outcome::Failed);
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
