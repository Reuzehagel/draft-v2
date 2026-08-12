// Session core — the dictation lifecycle as a pure state machine.
//
// Every method takes an input plus the current instant and returns a list of
// `Command`s for the caller to perform; the core touches no I/O, no clock, and
// no winit. That's what makes the lifecycle rules that used to hide inline in
// `main.rs` — a stale worker outcome repainting a newer pill, a second press
// while the previous transcription is still running, capture failing to start
// after the FSM already flipped to recording — assertable in a unit test.
//
// The core owns both activation FSMs (dictate and push-to-command), the capture
// handle, the monotonic session id, and the session kind. The winit event loop,
// the pill, the worker thread, the transcriber, and the clipboard all live on
// the far side of the `Command` boundary as adapters. The two FSMs are separate
// instances, so holding one chord can never corrupt the other's press/release
// state.
//
// A session ends the moment its outcome is known: the terminal flash outlives
// it and belongs to the Pill core, which is a peer rather than a downstream
// (ADR-0003). So this core reports what it is doing — `SessionActivity` — and
// never says what the pill should look like or how long anything stays up.
//
// Clock is a parameter, never ambient — no `Instant::now()` in here, matching
// the activation FSM one level down, whose events already carry an `Instant`.

use std::time::Instant;

use crate::activation::{self, InEvent, OutEvent};
use crate::pill::core::{Origin, SessionActivity};

/// Shortest capture worth transcribing; anything briefer is a fumbled tap.
const MIN_CAPTURE_MS: u64 = 150;

/// Which pipeline a capture feeds: dictation pastes the (post-processed)
/// transcript; command sends it to the LLM and pastes the answer (issue #4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionKind {
    Dictate,
    Command,
}

/// What a worker thread reports back once it finishes, so the pill can show an
/// honest result instead of a premature "success".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Text was produced and the paste call succeeded.
    Delivered,
    /// Transcription returned nothing usable — disappear quietly.
    Empty,
    /// Transcription or paste errored — the transcript is recoverable from
    /// History but never reached the cursor. Flash the pill red.
    Failed,
}

/// An effect for the adapter to perform. The core returns these; it never
/// performs them, so a test can assert on the list instead of on private state.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// Open the capture stream. The adapter reports the result back via
    /// [`Session::capture_started`].
    StartCapture,
    /// Tell the Pill core what this session is doing. Whether that is visible,
    /// and what replaces it afterwards, is the Pill core's call.
    ReportActivity(SessionActivity),
    /// Hand the captured samples to a worker under `session_id`; the worker
    /// reports its [`Outcome`] back via [`Session::on_outcome`].
    SpawnTranscription {
        samples: Vec<f32>,
        session_id: u64,
        session_kind: SessionKind,
    },
}

/// A capture stream the core can drain at stop. Abstracted so tests can drive
/// the lifecycle with a scripted fake instead of a live cpal stream.
pub trait CaptureHandle {
    /// Drain and return the captured samples. Called once, at stop; the handle
    /// is dropped immediately after (which stops the stream in production).
    fn take_samples(&self) -> Vec<f32>;
}

/// Where the lifecycle is right now. At most one variant holds a capture handle,
/// so "no overlapping capture" is structural, not a rule we have to police.
enum Phase<C> {
    /// Nothing happening.
    Idle,
    /// `StartCapture` emitted, awaiting the handle from the adapter.
    Starting { kind: SessionKind },
    /// Capturing; the pill shows live bars.
    Recording { kind: SessionKind, capture: C },
    /// Worker running under `session_id`; only its matching outcome reacts.
    /// (The pill's own Processing-since instant rides in the reported activity,
    /// stamped by `end`; the core needs only the id here.)
    Processing { session_id: u64 },
}

pub struct Session<C> {
    /// The dictation chord's activation FSM.
    dictate_fsm: activation::Fsm,
    /// The push-to-command chord's activation FSM. A separate instance from
    /// `dictate_fsm` so the two chords keep independent press/release state.
    command_fsm: activation::Fsm,
    /// Kept so a failed capture start can rebuild the relevant FSM into a clean,
    /// non-recording state.
    mode: activation::Mode,
    phase: Phase<C>,
    /// Monotonic id stamped on each dispatched worker; matched on its outcome so
    /// a superseded worker can't repaint a newer pill.
    session_seq: u64,
    /// Whether a transcriber is configured. Gates the commit-to-Processing
    /// decision at stop so we never park the pill in Processing with no worker
    /// coming to resolve it.
    transcriber_available: bool,
}

impl<C: CaptureHandle> Session<C> {
    pub fn new(mode: activation::Mode) -> Self {
        Self {
            dictate_fsm: activation::Fsm::new(mode),
            command_fsm: activation::Fsm::new(mode),
            mode,
            phase: Phase::Idle,
            session_seq: 0,
            transcriber_available: false,
        }
    }

    pub fn set_transcriber_available(&mut self, available: bool) {
        self.transcriber_available = available;
    }

    /// Rebuild both activation FSMs for a new mode (config reload).
    pub fn reset_activation(&mut self, mode: activation::Mode) {
        self.mode = mode;
        self.dictate_fsm = activation::Fsm::new(mode);
        self.command_fsm = activation::Fsm::new(mode);
    }

    /// The kind of the capture currently starting or running, if any. The
    /// adapter uses it to swallow the *other* chord's events for the duration,
    /// so the two chords can't fight over one microphone. `None` once the
    /// capture stops (while a worker runs a fresh chord may take over).
    pub fn capturing_kind(&self) -> Option<SessionKind> {
        match self.phase {
            Phase::Starting { kind } | Phase::Recording { kind, .. } => Some(kind),
            _ => None,
        }
    }

    /// Drive the dictation chord's FSM with a raw press/release event.
    pub fn on_dictate_input(&mut self, ev: InEvent) -> Vec<Command> {
        self.drive_fsm(SessionKind::Dictate, ev)
    }

    /// Drive the push-to-command chord's FSM with a raw press/release event.
    pub fn on_command_input(&mut self, ev: InEvent) -> Vec<Command> {
        self.drive_fsm(SessionKind::Command, ev)
    }

    /// Step the FSM belonging to `kind` and route its Start/Stop verdict into the
    /// shared lifecycle; the instant rides in with the event. Each chord has its
    /// own FSM, so one chord's presses never touch the other's state.
    fn drive_fsm(&mut self, kind: SessionKind, ev: InEvent) -> Vec<Command> {
        let now = match &ev {
            InEvent::Pressed(t) | InEvent::Released(t) => *t,
        };
        let fsm = match kind {
            SessionKind::Dictate => &mut self.dictate_fsm,
            SessionKind::Command => &mut self.command_fsm,
        };
        match fsm.step(ev) {
            OutEvent::Start => self.begin(kind),
            OutEvent::Stop => self.end(now),
            OutEvent::Ignore => Vec::new(),
        }
    }

    /// Begin a capture session. Replacing the phase drops any live capture
    /// handle (no overlap) and supersedes any in-flight tail — the superseded
    /// worker's outcome is later ignored by its session id.
    fn begin(&mut self, kind: SessionKind) -> Vec<Command> {
        self.phase = Phase::Starting { kind };
        vec![Command::StartCapture]
    }

    /// The adapter's report on a `StartCapture` command. `Some` promotes us to
    /// Recording, the first activity a session ever reports; `None` (open
    /// failed) resets the FSM so it doesn't believe it's recording — the
    /// capture/pill drift the PRD calls out. Nothing was reported in that case,
    /// so there is nothing to take back.
    pub fn capture_started(&mut self, capture: Option<C>) -> Vec<Command> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Starting { kind } => match capture {
                Some(capture) => {
                    self.phase = Phase::Recording { kind, capture };
                    // Every session this core starts is a chord press; the click
                    // path (#29) starts one from the pill itself.
                    vec![Command::ReportActivity(SessionActivity::Recording {
                        origin: Origin::Hotkey,
                    })]
                }
                None => {
                    tracing::error!("capture failed to start; session cancelled");
                    // No pill was ever shown (we only show it on success), so
                    // there's nothing to dismiss — just get back to a clean,
                    // non-recording state. Rebuild the FSM for whichever chord
                    // was starting, so it doesn't stay stuck believing it records.
                    match kind {
                        SessionKind::Dictate => self.dictate_fsm = activation::Fsm::new(self.mode),
                        SessionKind::Command => self.command_fsm = activation::Fsm::new(self.mode),
                    }
                    Vec::new()
                }
            },
            // A stray report with no start pending: ignore it and drop the
            // handle (if any) by leaving the phase as it was.
            other => {
                self.phase = other;
                Vec::new()
            }
        }
    }

    /// Stop the current capture. Drains the samples and, if there's enough audio
    /// and a transcriber, commits to Processing and hands the samples to a
    /// worker; otherwise the pill just disappears.
    fn end(&mut self, now: Instant) -> Vec<Command> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Recording { kind, capture } => {
                let samples = capture.take_samples();
                // `capture` drops here — the stream stops and the bars freeze.
                let duration_ms = samples.len() as u64 * 1000 / crate::audio::TARGET_SR as u64;
                let min = (crate::audio::TARGET_SR as u64 * MIN_CAPTURE_MS / 1000) as usize;
                if samples.len() < min {
                    // Nothing usable captured — nothing to show, no flash.
                    tracing::info!(duration_ms, "session: STOP (too short, dropped)");
                    return vec![Command::ReportActivity(SessionActivity::None)];
                }
                if !self.transcriber_available {
                    // No worker would ever resolve the pill; don't park it in
                    // Processing forever.
                    tracing::warn!(duration_ms, "session: STOP (no transcriber configured)");
                    return vec![Command::ReportActivity(SessionActivity::None)];
                }
                tracing::info!(duration_ms, "session: STOP (transcribing)");
                self.session_seq += 1;
                let session_id = self.session_seq;
                self.phase = Phase::Processing { session_id };
                vec![
                    Command::ReportActivity(SessionActivity::Processing { since: now }),
                    Command::SpawnTranscription {
                        samples,
                        session_id,
                        session_kind: kind,
                    },
                ]
            }
            // Released before the capture handle came back: abandon quietly, no
            // pill was shown.
            Phase::Starting { .. } => Vec::new(),
            // Stop with nothing recording (e.g. a stray release): report that
            // there is no session, matching the old "STOP without active capture".
            _ => {
                tracing::warn!("session: STOP without active capture");
                vec![Command::ReportActivity(SessionActivity::None)]
            }
        }
    }

    /// Apply a worker's reported outcome. Only the `Processing` phase that owns
    /// `session_id` reacts; an outcome from a superseded session is ignored, so
    /// a slow earlier worker can't repaint a later capture's pill.
    ///
    /// The session is over either way: how long a flash holds, and what follows
    /// it, belong to the Pill core.
    pub fn on_outcome(&mut self, session_id: u64, outcome: Outcome) -> Vec<Command> {
        let owns = matches!(self.phase, Phase::Processing { session_id: s, .. } if s == session_id);
        if !owns {
            return Vec::new();
        }
        self.phase = Phase::Idle;
        match outcome {
            Outcome::Delivered => vec![Command::ReportActivity(SessionActivity::Finished {
                ok: true,
            })],
            Outcome::Failed => vec![Command::ReportActivity(SessionActivity::Finished {
                ok: false,
            })],
            // Nothing usable was said — no flash to report, just an ended session.
            Outcome::Empty => vec![Command::ReportActivity(SessionActivity::None)],
        }
    }
}

impl CaptureHandle for crate::audio::capture::Capture {
    fn take_samples(&self) -> Vec<f32> {
        self.buffer.take()
    }
}

#[cfg(test)]
impl<C: CaptureHandle> Session<C> {
    fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording { .. })
    }
    fn is_processing(&self) -> bool {
        matches!(self.phase, Phase::Processing { .. })
    }
    fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activation::Mode;
    use std::time::Duration;

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_millis(ms)
    }

    /// The one activity a hotkey session ever starts with.
    fn recording() -> Command {
        Command::ReportActivity(SessionActivity::Recording {
            origin: Origin::Hotkey,
        })
    }

    /// A scripted capture: `take_samples` yields exactly what it was built with.
    struct FakeCapture {
        samples: Vec<f32>,
    }

    impl CaptureHandle for FakeCapture {
        fn take_samples(&self) -> Vec<f32> {
            self.samples.clone()
        }
    }

    /// Enough samples to clear the MIN_CAPTURE_MS floor.
    fn long_capture() -> FakeCapture {
        let n = (crate::audio::TARGET_SR as usize * (MIN_CAPTURE_MS as usize + 50)) / 1000;
        FakeCapture {
            samples: vec![0.1; n],
        }
    }

    /// Test rig: a Hold-mode session with a transcriber available.
    fn ready_session() -> Session<FakeCapture> {
        let mut s = Session::new(Mode::Hold {
            double_press_lock: false,
        });
        s.set_transcriber_available(true);
        s
    }

    /// Drive a session all the way into Processing, returning the live id.
    fn into_processing(s: &mut Session<FakeCapture>) -> u64 {
        assert_eq!(s.begin(SessionKind::Dictate), vec![Command::StartCapture]);
        assert_eq!(s.capture_started(Some(long_capture())), vec![recording()]);
        let cmds = s.end(t(1000));
        match &cmds[..] {
            [Command::ReportActivity(SessionActivity::Processing { .. }), Command::SpawnTranscription { session_id, .. }] => {
                *session_id
            }
            other => panic!("expected Processing + Spawn, got {other:?}"),
        }
    }

    #[test]
    fn happy_path_records_processes_and_flashes_green() {
        let mut s = ready_session();
        let id = into_processing(&mut s);
        assert!(s.is_processing());
        // The worker delivers: the session reports a successful finish and is
        // over — the flash that follows is the Pill core's to time.
        let cmds = s.on_outcome(id, Outcome::Delivered);
        assert_eq!(
            cmds,
            vec![Command::ReportActivity(SessionActivity::Finished {
                ok: true
            })]
        );
        assert!(s.is_idle());
    }

    #[test]
    fn stale_outcome_emits_no_activity_report() {
        let mut s = ready_session();
        let id = into_processing(&mut s);
        // An outcome for a *different* session (the user re-triggered, or a
        // slow earlier worker) must not touch the pill.
        let cmds = s.on_outcome(id.wrapping_add(1), Outcome::Delivered);
        assert!(cmds.is_empty(), "stale outcome produced commands: {cmds:?}");
        // And the real session is untouched — still processing.
        assert!(s.is_processing());
    }

    #[test]
    fn second_press_while_processing_does_not_overlap_capture() {
        let mut s = ready_session();
        let first = into_processing(&mut s);
        // A fresh press while the worker still runs supersedes it.
        assert_eq!(s.begin(SessionKind::Dictate), vec![Command::StartCapture]);
        assert_eq!(s.capture_started(Some(long_capture())), vec![recording()]);
        assert!(s.is_recording());
        // Stopping the new capture mints a *new* id (no id reuse, no overlap).
        let second = into_processing_from_recording(&mut s);
        assert_ne!(first, second);
        // The superseded worker's outcome is now ignored.
        assert!(s.on_outcome(first, Outcome::Delivered).is_empty());
    }

    // Helper: stop a session already in Recording, asserting Processing.
    fn into_processing_from_recording(s: &mut Session<FakeCapture>) -> u64 {
        match &s.end(t(2000))[..] {
            [Command::ReportActivity(SessionActivity::Processing { .. }), Command::SpawnTranscription { session_id, .. }] => {
                *session_id
            }
            other => panic!("expected Processing + Spawn, got {other:?}"),
        }
    }

    #[test]
    fn capture_start_failure_leaves_consistent_non_recording_state() {
        let mut s = ready_session();
        assert_eq!(s.begin(SessionKind::Dictate), vec![Command::StartCapture]);
        // The open failed: no pill, no recording, and the FSM is rebuilt so a
        // later press starts cleanly rather than being read as a "stop".
        let cmds = s.capture_started(None);
        assert!(cmds.is_empty(), "failed start produced commands: {cmds:?}");
        assert!(!s.is_recording());
        assert!(s.capturing_kind().is_none());
        // Next press starts a brand-new capture, proving the FSM isn't stuck
        // thinking it's already recording.
        assert_eq!(
            s.on_dictate_input(InEvent::Pressed(t(100))),
            vec![Command::StartCapture]
        );
    }

    #[test]
    fn too_short_capture_reports_no_activity_without_transcribing() {
        let mut s = ready_session();
        s.begin(SessionKind::Dictate);
        s.capture_started(Some(FakeCapture {
            samples: vec![0.1; 10],
        }));
        // Below the floor: nothing to show, no worker.
        assert_eq!(
            s.end(t(1000)),
            vec![Command::ReportActivity(SessionActivity::None)]
        );
        assert!(s.is_idle());
    }

    #[test]
    fn no_transcriber_reports_no_activity_instead_of_parking_in_processing() {
        let mut s = Session::new(Mode::Hold {
            double_press_lock: false,
        });
        // transcriber_available defaults to false.
        s.begin(SessionKind::Dictate);
        s.capture_started(Some(long_capture()));
        assert_eq!(
            s.end(t(1000)),
            vec![Command::ReportActivity(SessionActivity::None)]
        );
        assert!(s.is_idle());
    }

    #[test]
    fn empty_outcome_reports_no_activity_rather_than_a_flash() {
        let mut s = ready_session();
        let id = into_processing(&mut s);
        assert_eq!(
            s.on_outcome(id, Outcome::Empty),
            vec![Command::ReportActivity(SessionActivity::None)]
        );
        assert!(s.is_idle());
    }

    #[test]
    fn hold_press_release_runs_the_dictate_fsm_end_to_end() {
        let mut s = ready_session();
        // Press → start capture.
        assert_eq!(
            s.on_dictate_input(InEvent::Pressed(t(0))),
            vec![Command::StartCapture]
        );
        s.capture_started(Some(long_capture()));
        // Release → commit to a worker.
        let cmds = s.on_dictate_input(InEvent::Released(t(500)));
        assert!(matches!(
            cmds[..],
            [
                Command::ReportActivity(SessionActivity::Processing { .. }),
                Command::SpawnTranscription { .. }
            ]
        ));
    }

    /// Drive a chord (via its own input method) from idle all the way into
    /// Processing, asserting the spawn and returning its routed session kind.
    fn spawn_kind_for(
        s: &mut Session<FakeCapture>,
        input: fn(&mut Session<FakeCapture>, InEvent) -> Vec<Command>,
    ) -> SessionKind {
        assert_eq!(
            input(s, InEvent::Pressed(t(0))),
            vec![Command::StartCapture]
        );
        assert_eq!(s.capture_started(Some(long_capture())), vec![recording()]);
        match &input(s, InEvent::Released(t(500)))[..] {
            [Command::ReportActivity(SessionActivity::Processing { .. }), Command::SpawnTranscription { session_kind, .. }] => {
                *session_kind
            }
            other => panic!("expected Processing + Spawn, got {other:?}"),
        }
    }

    #[test]
    fn command_chord_routes_spawn_to_the_command_path() {
        let mut s = ready_session();
        // Driving the command chord's FSM end-to-end must tag the spawned worker
        // as Command, so the adapter routes it to the LLM instruction path.
        assert_eq!(
            spawn_kind_for(&mut s, Session::on_command_input),
            SessionKind::Command
        );
    }

    #[test]
    fn dictate_chord_routes_spawn_to_the_dictate_path() {
        let mut s = ready_session();
        // The dictation chord tags its worker as Dictate → plain postprocess path.
        assert_eq!(
            spawn_kind_for(&mut s, Session::on_dictate_input),
            SessionKind::Dictate
        );
    }

    #[test]
    fn one_chord_cannot_corrupt_the_others_press_release_state() {
        let mut s = ready_session();
        // The command chord is held → its FSM starts a capture.
        assert_eq!(
            s.on_command_input(InEvent::Pressed(t(0))),
            vec![Command::StartCapture]
        );
        // A stray release on the *dictate* chord (never pressed) is ignored: its
        // FSM has independent state, untouched by the command press. If the two
        // shared one FSM this would misfire as a Stop.
        assert!(s.on_dictate_input(InEvent::Released(t(100))).is_empty());
        // The command chord then still stops cleanly on its own release, routed
        // to the command path — proving its state survived intact.
        s.capture_started(Some(long_capture()));
        let cmds = s.on_command_input(InEvent::Released(t(200)));
        assert!(matches!(
            cmds[..],
            [
                Command::ReportActivity(SessionActivity::Processing { .. }),
                Command::SpawnTranscription {
                    session_kind: SessionKind::Command,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn command_capture_start_failure_resets_the_command_fsm() {
        let mut s = ready_session();
        assert_eq!(
            s.on_command_input(InEvent::Pressed(t(0))),
            vec![Command::StartCapture]
        );
        // Open failed: no pill, no recording, and the command FSM is rebuilt so a
        // later command press starts fresh instead of reading as a stop.
        assert!(s.capture_started(None).is_empty());
        assert!(!s.is_recording());
        assert!(s.capturing_kind().is_none());
        assert_eq!(
            s.on_command_input(InEvent::Pressed(t(100))),
            vec![Command::StartCapture]
        );
    }

    #[test]
    fn capturing_kind_tracks_the_active_chord() {
        let mut s = ready_session();
        assert_eq!(s.capturing_kind(), None);
        s.begin(SessionKind::Command);
        assert_eq!(s.capturing_kind(), Some(SessionKind::Command));
        s.capture_started(Some(long_capture()));
        assert_eq!(s.capturing_kind(), Some(SessionKind::Command));
        // Once processing, a new chord may take over → no longer "capturing".
        s.end(t(1000));
        assert_eq!(s.capturing_kind(), None);
    }
}
