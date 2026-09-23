// The Parakeet model download as the Transcription pane sees it: the shared
// state the pane draws from, and the worker body that drives it. The pane
// shows its progress state (and keeps repainting) exactly while
// `running` is set, so the one thing the worker must guarantee is that
// `running` clears however the download ends — returned, failed, or panicked.

use crate::transcribe::parakeet_download::Progress;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard};

pub(super) struct DownloadState {
    pub model_present: bool,
    pub running: bool,
    pub progress: Option<Progress>,
    pub finished: Option<Result<(), String>>,
}

impl DownloadState {
    pub fn new(model_present: bool) -> Self {
        Self {
            model_present,
            running: false,
            progress: None,
            finished: None,
        }
    }

    /// Enters the progress state, forgetting any previous attempt.
    pub fn start(&mut self) {
        self.running = true;
        self.finished = None;
        self.progress = None;
    }
}

/// Locks the state, recovering from poison: a worker that panicked while
/// holding the lock must not take the Settings window down with it, and the
/// state it left is still the best we have.
pub(super) fn lock(state: &Mutex<DownloadState>) -> MutexGuard<'_, DownloadState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// The worker body. `download` is handed a reporter to call with each
/// progress update; `is_present` is asked afterwards whether the model is now
/// on disk. A panic in `download` ends the attempt with an error, like any
/// other failure.
pub(super) fn run(
    state: &Mutex<DownloadState>,
    download: impl FnOnce(&dyn Fn(Progress)) -> anyhow::Result<()>,
    is_present: impl FnOnce() -> bool,
) {
    let report = |p: Progress| lock(state).progress = Some(p);
    let result = match panic::catch_unwind(AssertUnwindSafe(|| download(&report))) {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(payload) => Err(format!("Download crashed: {}", panic_message(&*payload))),
    };
    let model_present = is_present();
    let mut s = lock(state);
    s.running = false;
    s.model_present = model_present;
    s.finished = Some(result);
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown error")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> Mutex<DownloadState> {
        let mut s = DownloadState::new(false);
        s.start();
        Mutex::new(s)
    }

    fn progress(bytes_done: u64) -> Progress {
        Progress {
            file_index: 0,
            file_count: 1,
            bytes_done,
            bytes_total: Some(100),
        }
    }

    #[test]
    fn a_finished_download_leaves_the_progress_state_with_the_model_present() {
        let state = started();
        run(&state, |_| Ok(()), || true);
        let s = lock(&state);
        assert!(!s.running);
        assert!(s.model_present);
        assert_eq!(s.finished, Some(Ok(())));
    }

    #[test]
    fn a_failed_download_leaves_the_progress_state_with_its_error() {
        let state = started();
        run(&state, |_| Err(anyhow::anyhow!("HTTP 503")), || false);
        let s = lock(&state);
        assert!(!s.running);
        assert!(!s.model_present);
        assert_eq!(s.finished, Some(Err("HTTP 503".into())));
    }

    #[test]
    fn a_panicking_download_leaves_the_progress_state_with_an_error() {
        let state = started();
        run(&state, |_| panic!("disk on fire"), || false);
        let s = lock(&state);
        assert!(!s.running);
        assert_eq!(
            s.finished,
            Some(Err("Download crashed: disk on fire".into()))
        );
    }

    #[test]
    fn a_panic_with_a_formatted_message_keeps_its_message() {
        let state = started();
        run(&state, |_| panic!("file {} of {}", 2, 3), || false);
        assert_eq!(
            lock(&state).finished,
            Some(Err("Download crashed: file 2 of 3".into()))
        );
    }

    #[test]
    fn progress_reported_during_the_download_reaches_the_state() {
        let state = started();
        // Asserting inside `download` would be swallowed by the catch_unwind;
        // note what was seen and assert once `run` is back.
        let seen = std::cell::Cell::new(None);
        run(
            &state,
            |report| {
                report(progress(40));
                seen.set(lock(&state).progress.as_ref().map(|p| p.bytes_done));
                Ok(())
            },
            || true,
        );
        assert_eq!(seen.get(), Some(40));
    }

    #[test]
    fn a_poisoned_lock_still_locks_and_the_download_still_ends() {
        let state = started();
        let _ = std::thread::scope(|s| {
            s.spawn(|| {
                let _guard = state.lock().unwrap();
                panic!("poison");
            })
            .join()
        });
        assert!(state.is_poisoned());
        assert!(lock(&state).running);
        run(&state, |_| Ok(()), || true);
        assert!(!lock(&state).running);
    }

    #[test]
    fn starting_again_forgets_the_previous_attempt() {
        let state = started();
        run(
            &state,
            |report| {
                report(progress(10));
                Err(anyhow::anyhow!("nope"))
            },
            || false,
        );
        let mut s = lock(&state);
        s.start();
        assert!(s.running);
        assert!(s.finished.is_none());
        assert!(s.progress.is_none());
    }
}
