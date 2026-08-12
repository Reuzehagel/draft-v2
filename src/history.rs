// Transcript history — a safety net for the case where a paste lands in the
// wrong place (or nowhere) and the text would otherwise be lost.
//
// Every transcript is appended here *before* the paste is attempted, so even a
// silently-swallowed Ctrl+V leaves a recoverable copy. Storage is a small
// newline-delimited JSON file (`history.jsonl`) in the app data dir, capped to
// the most recent MAX_ENTRIES. Writes are serialized through a process-wide
// lock and committed atomically, so racing paste threads can't corrupt it.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Rolling window size. Old entries past this are dropped on the next append.
const MAX_ENTRIES: usize = 100;

/// Serializes the read-modify-write in `append` so two paste threads finishing
/// at once can't clobber each other's writes.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Unix time (seconds) the transcript was recorded.
    pub ts: i64,
    /// The post-processed transcript text, exactly as it was handed to paste
    /// (minus any cosmetic trailing space).
    pub text: String,
    /// Which transcriber produced it, for context in the list.
    pub provider: String,
}

fn history_path() -> Result<std::path::PathBuf> {
    Ok(crate::paths::data_dir()?.join("history.jsonl"))
}

/// Seconds since the Unix epoch. Shared so recorded timestamps and the UI's
/// "x ago" rendering agree on the same clock convention.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read every entry, oldest first. Unparseable lines are skipped rather than
/// failing the whole load, so one bad record can't hide the rest.
pub fn load() -> Vec<Entry> {
    let Ok(path) = history_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect()
}

/// The most recent transcript, if any. Used by the tray "Copy last" action.
/// Parses only the final line rather than the whole file.
pub fn last() -> Option<Entry> {
    let path = history_path().ok()?;
    let text = std::fs::read_to_string(&path).ok()?;
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str(l).ok())
}

/// Whether any transcript has been recorded. A metadata check, not a read —
/// the tray asks this after every dictation just to decide whether to offer
/// "Copy last transcription", and that runs on the UI thread.
pub fn is_empty() -> bool {
    let Ok(path) = history_path() else {
        return true;
    };
    std::fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true)
}

/// Wipe the history file. Best-effort — a missing file is already "clear".
pub fn clear() -> Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = history_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Record a transcript. Appends it as the newest entry and trims the file back
/// to MAX_ENTRIES, committing atomically so a crash mid-write can't truncate
/// the history.
pub fn append(text: &str, provider: &str) -> Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = history_path()?;

    let mut entries = load();
    entries.push(Entry {
        ts: now_unix(),
        text: text.to_owned(),
        provider: provider.to_owned(),
    });
    // Keep only the newest MAX_ENTRIES.
    let start = entries.len().saturating_sub(MAX_ENTRIES);
    let kept = &entries[start..];

    let mut out = String::with_capacity(kept.len() * 64);
    for e in kept {
        out.push_str(&serde_json::to_string(e)?);
        out.push('\n');
    }
    crate::paths::atomic_write(&path, out)
}
