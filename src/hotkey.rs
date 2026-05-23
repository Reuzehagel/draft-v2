// Global hotkey: parse a string like "Ctrl+Backslash" into a registered
// global-hotkey, expose a receiver that emits Pressed/Released events
// translated into Instants.

use anyhow::{anyhow, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use std::time::Instant;

pub struct HotkeyHandle {
    _manager: GlobalHotKeyManager,
    _registered: HotKey,
}

#[derive(Debug)]
pub enum HotkeyEvent {
    Pressed(Instant),
    Released(Instant),
}

pub fn parse(spec: &str) -> Result<HotKey> {
    let mut mods = Modifiers::empty();
    let mut key: Option<Code> = None;
    for part in spec.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" => mods |= Modifiers::ALT,
            "super" | "win" | "meta" => mods |= Modifiers::SUPER,
            other => {
                if key.is_some() {
                    return Err(anyhow!("hotkey '{spec}': multiple non-modifier keys"));
                }
                key = Some(parse_code(other)?);
            }
        }
    }
    let code = key.ok_or_else(|| anyhow!("hotkey '{spec}': no key specified"))?;
    Ok(HotKey::new(Some(mods), code))
}

fn parse_code(s: &str) -> Result<Code> {
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
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_alphabetic() => {
            match s {
                "A" => Code::KeyA, "B" => Code::KeyB, "C" => Code::KeyC, "D" => Code::KeyD,
                "E" => Code::KeyE, "F" => Code::KeyF, "G" => Code::KeyG, "H" => Code::KeyH,
                "I" => Code::KeyI, "J" => Code::KeyJ, "K" => Code::KeyK, "L" => Code::KeyL,
                "M" => Code::KeyM, "N" => Code::KeyN, "O" => Code::KeyO, "P" => Code::KeyP,
                "Q" => Code::KeyQ, "R" => Code::KeyR, "S" => Code::KeyS, "T" => Code::KeyT,
                "U" => Code::KeyU, "V" => Code::KeyV, "W" => Code::KeyW, "X" => Code::KeyX,
                "Y" => Code::KeyY, "Z" => Code::KeyZ,
                _ => unreachable!(),
            }
        }
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_digit() => {
            match s {
                "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2,
                "3" => Code::Digit3, "4" => Code::Digit4, "5" => Code::Digit5,
                "6" => Code::Digit6, "7" => Code::Digit7, "8" => Code::Digit8,
                "9" => Code::Digit9,
                _ => unreachable!(),
            }
        }
        s if s.starts_with('F') && s[1..].chars().all(|c| c.is_ascii_digit()) => {
            match s {
                "F1" => Code::F1, "F2" => Code::F2, "F3" => Code::F3, "F4" => Code::F4,
                "F5" => Code::F5, "F6" => Code::F6, "F7" => Code::F7, "F8" => Code::F8,
                "F9" => Code::F9, "F10" => Code::F10, "F11" => Code::F11, "F12" => Code::F12,
                _ => return Err(anyhow!("unsupported function key: {s}")),
            }
        }
        _ => return Err(anyhow!("unknown key name: {s}")),
    };
    Ok(code)
}

pub fn register(spec: &str) -> Result<(HotkeyHandle, crossbeam_channel::Receiver<HotkeyEvent>)> {
    let hk = parse(spec)?;
    let manager = GlobalHotKeyManager::new()
        .map_err(|e| anyhow!("global-hotkey init failed: {e}"))?;
    manager
        .register(hk)
        .map_err(|e| anyhow!("RegisterHotKey failed for '{spec}': {e}. Another app may own this combo."))?;

    let (tx, rx) = crossbeam_channel::unbounded::<HotkeyEvent>();
    GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
        let now = Instant::now();
        let msg = match ev.state {
            HotKeyState::Pressed => HotkeyEvent::Pressed(now),
            HotKeyState::Released => HotkeyEvent::Released(now),
        };
        let _ = tx.send(msg);
    }));

    Ok((
        HotkeyHandle {
            _manager: manager,
            _registered: hk,
        },
        rx,
    ))
}
