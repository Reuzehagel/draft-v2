// Paste pipeline (PasteMode::Clipboard):
//   1. Take the app-wide clipboard lock — two quick dictations must not
//      interleave their set/restore steps.
//   2. Snapshot existing clipboard text — unless it's residue from our own
//      previous paste (restoring that would re-paste an old transcript).
//   3. Put the transcript on the clipboard.
//   4. Wait for physically-held modifier keys to clear, then synthesize
//      Ctrl+V into the foreground window.
//   5. After RESTORE_DELAY, restore the snapshot — but only if the clipboard
//      still holds our transcript.
//
// Fallback path (PasteMode::Unicode): bypass clipboard entirely and
// synthesize each character via SendInput with KEYEVENTF_UNICODE. Useful
// for hosts that swallow Ctrl+V (some RDP/Citrix sessions). Slower and
// triggers per-key handlers, so it's opt-in via config.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RETURN,
    VK_RWIN, VK_SHIFT, VK_V,
};

/// How long the target app gets to consume the Ctrl+V before the original
/// clipboard is put back. SendInput only queues the keystroke — the app reads
/// the clipboard whenever its message loop processes the paste, and Electron
/// hosts under load have been seen taking >150 ms. Lose that race and the app
/// pastes the restored (stale) text instead of the fresh transcript.
const RESTORE_DELAY: Duration = Duration::from_millis(500);

/// Upper bound on waiting for the user to let go of modifier keys. Beyond
/// this we inject anyway and log, so a stuck key can't wedge a paste forever.
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_millis(1000);

/// App-wide clipboard gate. Holding the lock serializes every clipboard touch
/// in this process. The value is the last transcript we left on the clipboard,
/// used to avoid "restoring" our own residue on the next paste.
static CLIPBOARD_STATE: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Clone, Copy)]
pub enum PasteMode {
    Clipboard,
    Unicode,
}

/// Deliver `text` to the foreground window. `on_sent` fires as soon as the
/// synthetic input has been handed to the OS — before any clipboard-restore
/// housekeeping — so the caller can flash success without waiting out
/// RESTORE_DELAY. Called exactly once on the `Ok` path, never on `Err`.
pub fn deliver_text(
    text: &str,
    mode: PasteMode,
    restore_clipboard: bool,
    on_sent: impl FnOnce(),
) -> Result<()> {
    match mode {
        PasteMode::Clipboard => paste_via_clipboard(text, restore_clipboard, on_sent),
        PasteMode::Unicode => type_unicode(text, on_sent),
    }
}

/// Put `text` on the clipboard without pasting. Recovery actions (e.g. the
/// tray "Copy last transcription") go through here so every clipboard write
/// stays inside this module — and behind the same lock as the paste pipeline.
pub fn set_clipboard(text: &str) -> Result<()> {
    let _state = CLIPBOARD_STATE.lock();
    let mut cb = arboard::Clipboard::new().context("open clipboard")?;
    cb.set_text(text).context("write to clipboard")
}

fn paste_via_clipboard(text: &str, restore: bool, on_sent: impl FnOnce()) -> Result<()> {
    let mut last_pasted = CLIPBOARD_STATE.lock();

    let mut cb = arboard::Clipboard::new().context("open clipboard")?;
    // Snapshot for restore — but never our own previous transcript. Whenever a
    // restore is skipped (snapshot empty or non-text), the pasted transcript
    // stays on the clipboard; treating that residue as user content and
    // restoring it mid-race is exactly how an old dictation reappears.
    let saved = if restore {
        cb.get_text()
            .ok()
            .filter(|prev| last_pasted.as_deref() != Some(prev.as_str()))
    } else {
        None
    };

    cb.set_text(text).context("write to clipboard")?;
    drop(cb);
    *last_pasted = Some(text.to_owned());

    // Clipboard owner change has to propagate before the foreground app
    // reads it on Ctrl+V.
    std::thread::sleep(Duration::from_millis(30));
    wait_for_modifier_release();
    send_ctrl_v()?;
    on_sent();

    if let Some(prev) = saved {
        std::thread::sleep(RESTORE_DELAY);
        // Best-effort restore, and only if the clipboard still holds our
        // transcript — if anything else wrote in the meantime (another
        // dictation, a user copy), stomping it would lose data.
        if let Ok(mut cb2) = arboard::Clipboard::new() {
            if cb2.get_text().ok().as_deref() == Some(text) && cb2.set_text(prev).is_ok() {
                *last_pasted = None;
            }
        }
    }
    Ok(())
}

/// Block until no modifier key is physically held (or the timeout passes).
/// Transcription often finishes ~100–250 ms after the hotkey is released, so
/// the user may still be holding part of the combo (Ctrl on the default
/// binding, Shift/Alt on custom ones). A V injected then arrives as
/// Ctrl+Shift+V etc. — a different action in many apps — and in Unicode mode
/// every character would become a shortcut chord.
fn wait_for_modifier_release() {
    const MODIFIERS: [VIRTUAL_KEY; 5] = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN];
    let deadline = Instant::now() + MODIFIER_RELEASE_TIMEOUT;
    loop {
        let held = MODIFIERS
            .iter()
            .any(|&vk| (unsafe { GetAsyncKeyState(vk.0 as i32) } as u16) & 0x8000 != 0);
        if !held {
            return;
        }
        if Instant::now() >= deadline {
            tracing::warn!("modifier key still held at paste time; injecting anyway");
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn type_unicode(text: &str, on_sent: impl FnOnce()) -> Result<()> {
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
        on_sent();
        return Ok(());
    }
    wait_for_modifier_release();
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize != inputs.len() {
        anyhow::bail!("SendInput sent {} of {} unicode events", n, inputs.len());
    }
    on_sent();
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
    let flags = if key_up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
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
