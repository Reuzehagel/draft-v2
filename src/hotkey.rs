// Global hotkeys: parse strings like "Ctrl+Backslash" into registered
// global-hotkeys, expose a receiver that emits Pressed/Released events
// translated into Instants, each tagged with which chord fired.

use anyhow::{anyhow, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

pub struct HotkeyHandle {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
}

impl Drop for HotkeyHandle {
    /// The crate's manager `Drop` only destroys its message window — Windows
    /// keeps the RegisterHotKey registrations alive, so without an explicit
    /// release, re-registering the same combo (e.g. after a settings change
    /// that only toggled the command chord) fails against our own orphaned
    /// registration.
    fn drop(&mut self) {
        if let Err(e) = self.manager.unregister_all(&self.registered) {
            tracing::warn!(error = %e, "failed to unregister hotkeys");
        }
    }
}

/// Which registered hotkey an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    /// The main hotkey: speech is transcribed and pasted as-is.
    Dictate,
    /// Push-to-command (issue #4): speech is an instruction, and the LLM's
    /// answer is pasted instead of the words.
    Command,
}

#[derive(Debug)]
pub enum HotkeyEvent {
    Pressed(Chord, Instant),
    Released(Chord, Instant),
}

/// Why a hotkey spec didn't parse. The messages are written for the settings
/// window, which shows them beneath the field the spec was typed into — so
/// they name the offending part, never the whole spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Nothing but whitespace and stray `+`s.
    Empty,
    /// Modifiers with no key to go with them.
    NoKey,
    /// More than one non-modifier key, as typed.
    TwoKeys(String, String),
    /// A key name we don't map, as typed.
    UnknownKey(String),
    /// F13 and up: real keys, but not ones we map.
    UnsupportedFunctionKey(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Empty => write!(f, "Enter a key combination, like Ctrl+Backslash."),
            ParseError::NoKey => write!(f, "Add a key to go with the modifiers."),
            ParseError::TwoKeys(a, b) => {
                write!(
                    f,
                    "Use one key besides the modifiers, not both {a} and {b}."
                )
            }
            ParseError::UnknownKey(k) => write!(f, "“{k}” isn't a key name Draft knows."),
            ParseError::UnsupportedFunctionKey(k) => {
                write!(f, "{k} isn't supported — use F1 to F12.")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a spec like "Ctrl+Shift+Backslash". Parts are `+`-separated, trimmed
/// and case-insensitive; empty parts are skipped. The settings window validates
/// with this same function, so a spec it lets through is one this accepts.
pub fn parse(spec: &str) -> Result<HotKey, ParseError> {
    let mut mods = Modifiers::empty();
    let mut key: Option<(Code, &str)> = None;
    for part in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" => mods |= Modifiers::ALT,
            "super" | "win" | "meta" => mods |= Modifiers::SUPER,
            _ => {
                if let Some((_, first)) = key {
                    return Err(ParseError::TwoKeys(first.into(), part.into()));
                }
                key = Some((parse_code(part)?, part));
            }
        }
    }
    match key {
        Some((code, _)) => Ok(HotKey::new(Some(mods), code)),
        None if mods.is_empty() => Err(ParseError::Empty),
        None => Err(ParseError::NoKey),
    }
}

fn parse_code(s: &str) -> Result<Code, ParseError> {
    // Map a small set of human-friendly names. Anything not listed falls back
    // to letter/digit detection so "A", "1", "F5" etc. just work.
    let upper = s.to_ascii_uppercase();
    let code = match upper.as_str() {
        "BACKSLASH" | "\\" => Code::Backslash,
        "SLASH" | "/" => Code::Slash,
        "BACKQUOTE" | "`" | "GRAVE" => Code::Backquote,
        "SPACE" => Code::Space,
        "TAB" => Code::Tab,
        "ENTER" | "RETURN" => Code::Enter,
        "ESCAPE" | "ESC" => Code::Escape,
        "MINUS" | "-" => Code::Minus,
        "EQUAL" | "=" => Code::Equal,
        "SEMICOLON" | ";" => Code::Semicolon,
        "QUOTE" | "'" => Code::Quote,
        "COMMA" | "," => Code::Comma,
        "PERIOD" | "." => Code::Period,
        "LEFT" => Code::ArrowLeft,
        "RIGHT" => Code::ArrowRight,
        "UP" => Code::ArrowUp,
        "DOWN" => Code::ArrowDown,
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_alphabetic() => match s {
            "A" => Code::KeyA,
            "B" => Code::KeyB,
            "C" => Code::KeyC,
            "D" => Code::KeyD,
            "E" => Code::KeyE,
            "F" => Code::KeyF,
            "G" => Code::KeyG,
            "H" => Code::KeyH,
            "I" => Code::KeyI,
            "J" => Code::KeyJ,
            "K" => Code::KeyK,
            "L" => Code::KeyL,
            "M" => Code::KeyM,
            "N" => Code::KeyN,
            "O" => Code::KeyO,
            "P" => Code::KeyP,
            "Q" => Code::KeyQ,
            "R" => Code::KeyR,
            "S" => Code::KeyS,
            "T" => Code::KeyT,
            "U" => Code::KeyU,
            "V" => Code::KeyV,
            "W" => Code::KeyW,
            "X" => Code::KeyX,
            "Y" => Code::KeyY,
            "Z" => Code::KeyZ,
            _ => unreachable!(),
        },
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_digit() => match s {
            "0" => Code::Digit0,
            "1" => Code::Digit1,
            "2" => Code::Digit2,
            "3" => Code::Digit3,
            "4" => Code::Digit4,
            "5" => Code::Digit5,
            "6" => Code::Digit6,
            "7" => Code::Digit7,
            "8" => Code::Digit8,
            "9" => Code::Digit9,
            _ => unreachable!(),
        },
        s if s.starts_with('F') && s[1..].chars().all(|c| c.is_ascii_digit()) => match s {
            "F1" => Code::F1,
            "F2" => Code::F2,
            "F3" => Code::F3,
            "F4" => Code::F4,
            "F5" => Code::F5,
            "F6" => Code::F6,
            "F7" => Code::F7,
            "F8" => Code::F8,
            "F9" => Code::F9,
            "F10" => Code::F10,
            "F11" => Code::F11,
            "F12" => Code::F12,
            _ => return Err(ParseError::UnsupportedFunctionKey(s.into())),
        },
        _ => return Err(ParseError::UnknownKey(s.into())),
    };
    Ok(code)
}

/// Which registered ids currently map to which chord. Refreshed on every
/// (re-)registration; read by the process-global event handler.
struct ChordIds {
    dictate: Option<u32>,
    command: Option<u32>,
}

static CHORD_IDS: RwLock<ChordIds> = RwLock::new(ChordIds {
    dictate: None,
    command: None,
});

/// One channel for the process lifetime. The crate's event handler slot is a
/// write-once OnceCell — a second `set_event_handler` call is silently
/// ignored — so the handler must be installed exactly once, over state that
/// outlives every registration. Re-registrations only swap CHORD_IDS.
static EVENT_CHANNEL: OnceLock<(
    crossbeam_channel::Sender<HotkeyEvent>,
    crossbeam_channel::Receiver<HotkeyEvent>,
)> = OnceLock::new();

fn install_handler_once(waker: &crate::wake::Waker) -> crossbeam_channel::Receiver<HotkeyEvent> {
    let (tx, rx) = EVENT_CHANNEL.get_or_init(crossbeam_channel::unbounded);
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let tx = tx.clone();
        // The chord is what a dictation starts from, so this is the wake that
        // matters most for latency: the loop is asleep with no timer armed
        // until this posts to it. Cloned into the handler, which outlives
        // every re-registration — the waker is the loop's, not a binding's.
        let waker = waker.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
            let now = Instant::now();
            let chord = {
                let ids = CHORD_IDS.read().unwrap_or_else(|e| e.into_inner());
                if ids.dictate == Some(ev.id) {
                    Chord::Dictate
                } else if ids.command == Some(ev.id) {
                    Chord::Command
                } else {
                    return;
                }
            };
            let msg = match ev.state {
                HotKeyState::Pressed => HotkeyEvent::Pressed(chord, now),
                HotKeyState::Released => HotkeyEvent::Released(chord, now),
            };
            let _ = tx.send(msg);
            waker.wake();
        }));
    });
    rx.clone()
}

/// `parse`, with the spec put back into the message: the log has no field
/// beside it to say what was typed.
fn parse_for_register(spec: &str) -> Result<HotKey> {
    parse(spec).map_err(|e| anyhow!("hotkey '{spec}': {e}"))
}

/// Register the dictation hotkey (required) and, when given, the
/// push-to-command hotkey. A command chord that fails to parse or register
/// (e.g. another app owns the combo) is logged and skipped rather than
/// failing the whole app — dictation must survive.
///
/// Safe to call again after dropping the previous handle (the drop
/// unregisters); the returned receiver is always the same process-wide
/// channel.
pub fn register(
    dictate_spec: &str,
    command_spec: Option<&str>,
    waker: &crate::wake::Waker,
) -> Result<(HotkeyHandle, crossbeam_channel::Receiver<HotkeyEvent>)> {
    let dictate = parse_for_register(dictate_spec)?;
    let manager =
        GlobalHotKeyManager::new().map_err(|e| anyhow!("global-hotkey init failed: {e}"))?;
    manager.register(dictate).map_err(|e| {
        anyhow!("RegisterHotKey failed for '{dictate_spec}': {e}. Another app may own this combo.")
    })?;
    let mut registered = vec![dictate];

    let mut command_id = None;
    if let Some(spec) = command_spec {
        let attempt = parse_for_register(spec).and_then(|hk| {
            manager.register(hk).map_err(|e| {
                anyhow!("RegisterHotKey failed for '{spec}': {e}. Another app may own this combo.")
            })?;
            Ok(hk)
        });
        match attempt {
            Ok(hk) => {
                command_id = Some(hk.id());
                registered.push(hk);
            }
            Err(e) => {
                tracing::warn!(error = %e, "command hotkey unavailable; push-to-command disabled");
            }
        }
    }

    *CHORD_IDS.write().unwrap_or_else(|e| e.into_inner()) = ChordIds {
        dictate: Some(dictate.id()),
        command: command_id,
    };

    let rx = install_handler_once(waker);
    Ok((
        HotkeyHandle {
            manager,
            registered,
        },
        rx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_chord_parses() {
        let hk = parse("Ctrl+Backslash").unwrap();
        assert_eq!(hk, HotKey::new(Some(Modifiers::CONTROL), Code::Backslash));
        let hk = parse(" shift + alt + f5 ").unwrap();
        assert_eq!(
            hk,
            HotKey::new(Some(Modifiers::SHIFT | Modifiers::ALT), Code::F5)
        );
    }

    #[test]
    fn a_blank_spec_asks_for_a_combination() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert_eq!(parse("   "), Err(ParseError::Empty));
    }

    #[test]
    fn modifiers_alone_ask_for_a_key() {
        assert_eq!(parse("Ctrl+Shift"), Err(ParseError::NoKey));
        assert_eq!(parse("Ctrl+"), Err(ParseError::NoKey));
    }

    #[test]
    fn an_unknown_key_is_named_as_typed() {
        assert_eq!(
            parse("Ctrl+Bakslash"),
            Err(ParseError::UnknownKey("Bakslash".into()))
        );
    }

    #[test]
    fn two_keys_are_both_named() {
        assert_eq!(
            parse("Ctrl+A+b"),
            Err(ParseError::TwoKeys("A".into(), "b".into()))
        );
    }

    #[test]
    fn a_function_key_past_f12_is_refused() {
        assert_eq!(
            parse("Ctrl+F13"),
            Err(ParseError::UnsupportedFunctionKey("F13".into()))
        );
    }

    /// The message is shown beneath the field, which already says what was
    /// typed — so it names the offending part, and never echoes the whole spec.
    #[test]
    fn a_message_names_the_problem_not_the_spec() {
        let msg = parse("Ctrl+Bakslash").unwrap_err().to_string();
        assert!(msg.contains("Bakslash"), "{msg}");
        assert!(!msg.contains("Ctrl+"), "{msg}");
    }
}
