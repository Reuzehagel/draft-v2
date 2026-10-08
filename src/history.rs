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
use std::path::Path;
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

#[cfg(not(test))]
fn history_path() -> Result<std::path::PathBuf> {
    Ok(crate::paths::data_dir()?.join("history.jsonl"))
}

/// The library's own tests get a history file of their own per process, wiped
/// on first use, so a test can watch what is written without reading — or
/// writing — the user's. Only the library's unit tests see this: the `draft`
/// binary's tests link the ordinary library, so a test there takes history as
/// a seam instead (the answer run's `record` closure).
#[cfg(test)]
fn history_path() -> Result<std::path::PathBuf> {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    Ok(PATH
        .get_or_init(|| {
            let dir = std::env::temp_dir()
                .join("draft-history-tests")
                .join(format!("process-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            dir.join("history.jsonl")
        })
        .clone())
}

/// Seconds since the Unix epoch. Shared so recorded timestamps and the UI's
/// "x ago" rendering agree on the same clock convention.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read every entry, oldest first. Unparseable lines are skipped rather than
/// failing the whole load, so one bad record can't hide the rest.
pub fn load() -> Vec<Entry> {
    history_path().map(|p| load_at(&p)).unwrap_or_default()
}

fn load_at(path: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
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
    last_at(&history_path().ok()?)
}

fn last_at(path: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str(l).ok())
}

/// Whether any transcript has been recorded. A metadata check, not a read —
/// the tray asks this after every dictation just to decide whether to offer
/// "Copy last transcription", and that runs on the UI thread.
pub fn is_empty() -> bool {
    history_path().map(|p| is_empty_at(&p)).unwrap_or(true)
}

fn is_empty_at(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() == 0)
        .unwrap_or(true)
}

/// Wipe the history file. Best-effort — a missing file is already "clear".
pub fn clear() -> Result<()> {
    clear_at(&history_path()?)
}

fn clear_at(path: &Path) -> Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Record a transcript. Appends it as the newest entry and trims the file back
/// to MAX_ENTRIES, committing atomically so a crash mid-write can't truncate
/// the history.
pub fn append(text: &str, provider: &str) -> Result<()> {
    append_at(&history_path()?, text, provider)
}

fn append_at(path: &Path, text: &str, provider: &str) -> Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut entries = load_at(path);
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
    crate::paths::atomic_write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh history file of its own per test, never the user's real one.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("draft-history-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("history.jsonl")
    }

    fn texts(path: &Path) -> Vec<String> {
        load_at(path).into_iter().map(|e| e.text).collect()
    }

    /// The window rolls: past the cap it is the *oldest* transcripts that go,
    /// so the one just dictated — the one a lost paste needs — is always kept.
    #[test]
    fn past_the_cap_the_oldest_transcripts_are_dropped() {
        let path = scratch("cap");
        for i in 0..MAX_ENTRIES + 3 {
            append_at(&path, &format!("t{i}"), "groq").unwrap();
        }
        let kept = texts(&path);
        assert_eq!(kept.len(), MAX_ENTRIES);
        assert_eq!(kept.first().map(String::as_str), Some("t3"));
        assert_eq!(kept.last().map(String::as_str), Some("t102"));
        assert_eq!(last_at(&path).unwrap().text, "t102");
    }

    /// One damaged line — a hand edit, a write from an older build — costs
    /// that record only. The rest still load, and the next append still lands.
    #[test]
    fn a_damaged_line_hides_nothing_else() {
        let path = scratch("damaged");
        append_at(&path, "before", "groq").unwrap();
        append_at(&path, "after", "openai").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let (first, rest) = text.split_once('\n').unwrap();
        std::fs::write(&path, format!("{first}\n{{not json\n{rest}")).unwrap();
        assert_eq!(texts(&path), ["before", "after"]);

        append_at(&path, "next", "groq").unwrap();
        assert_eq!(texts(&path), ["before", "after", "next"]);
        let last = last_at(&path).unwrap();
        assert_eq!(
            (last.text.as_str(), last.provider.as_str()),
            ("next", "groq")
        );
    }

    /// The tray offers "Copy last transcription" off `is_empty`, so it has to
    /// track the file through a first append and a clear — and clearing a
    /// history that was never written is not an error.
    #[test]
    fn emptiness_follows_appends_and_clears() {
        let path = scratch("empty");
        assert!(is_empty_at(&path));
        clear_at(&path).expect("clearing a missing history is fine");

        append_at(&path, "hello", "groq").unwrap();
        assert!(!is_empty_at(&path));

        clear_at(&path).unwrap();
        assert!(is_empty_at(&path));
        assert!(last_at(&path).is_none());
    }
}
