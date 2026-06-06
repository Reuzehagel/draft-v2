// Paste pipeline:
//   1. Snapshot existing clipboard text (if any).
//   2. Put the transcript on the clipboard.
//   3. Synthesize Ctrl+V into the foreground window.
//   4. After a short delay, restore the original clipboard.
//
// Fallback path (PasteMode::Unicode): bypass clipboard entirely and
// synthesize each character via SendInput with KEYEVENTF_UNICODE. Useful
// for hosts that swallow Ctrl+V (some RDP/Citrix sessions). Slower and
// triggers per-key handlers, so it's opt-in via config.

use anyhow::{Context, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_RETURN, VK_V,
};

#[derive(Debug, Clone, Copy)]
pub enum PasteMode {
    Clipboard,
    Unicode,
}

pub fn deliver_text(text: &str, mode: PasteMode, restore_clipboard: bool) -> Result<()> {
    match mode {
        PasteMode::Clipboard => paste_via_clipboard(text, restore_clipboard),
        PasteMode::Unicode => type_unicode(text),
    }
}

fn paste_via_clipboard(text: &str, restore: bool) -> Result<()> {
    let mut cb = arboard::Clipboard::new().context("open clipboard")?;
    let saved = if restore { cb.get_text().ok() } else { None };

    cb.set_text(text.to_owned()).context("write to clipboard")?;
    // Clipboard owner change has to propagate before the foreground app
    // reads it on Ctrl+V.
    std::thread::sleep(std::time::Duration::from_millis(30));
    send_ctrl_v()?;

    if let Some(prev) = saved {
        // Give the target app time to actually read the clipboard before
        // we stomp it. 150 ms is enough for native + Electron hosts in
        // local testing; longer is safer but more visible.
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Best-effort restore — open a fresh clipboard handle in case
        // some app between us and now invalidated the previous one.
        if let Ok(mut cb2) = arboard::Clipboard::new() {
            let _ = cb2.set_text(prev);
        }
    }
    Ok(())
}

fn type_unicode(text: &str) -> Result<()> {
    // Per character: one keydown + one keyup with KEYEVENTF_UNICODE.
    // Surrogate pairs need to be emitted as two separate inputs.
    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.encode_utf16().count() * 2);
    let mut utf16 = [0u16; 2];
    for ch in text.chars() {
        match ch {
            // A literal LF (wScan 0x0A) is a control char most edit controls
            // ignore — emit a real Return keystroke instead. Skip CR so CRLF
            // collapses to a single newline.
            '\r' => {}
            '\n' => {
                inputs.push(key_event(VK_RETURN, false));
                inputs.push(key_event(VK_RETURN, true));
            }
            _ => {
                for unit in ch.encode_utf16(&mut utf16) {
                    inputs.push(unicode_event(*unit, false));
                    inputs.push(unicode_event(*unit, true));
                }
            }
        }
    }
    if inputs.is_empty() {
        return Ok(());
    }
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize != inputs.len() {
        anyhow::bail!("SendInput sent {} of {} unicode events", n, inputs.len());
    }
    Ok(())
}

fn send_ctrl_v() -> Result<()> {
    let inputs = [
        key_event(VK_CONTROL, false),
        key_event(VK_V, false),
        key_event(VK_V, true),
        key_event(VK_CONTROL, true),
    ];
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize != inputs.len() {
        anyhow::bail!("SendInput sent {} of {} events", n, inputs.len());
    }
    Ok(())
}

fn key_event(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    let flags = if key_up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn unicode_event(unit: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
