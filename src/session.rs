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
// handle, the post-capture `Phase` (the old `Tail`), the monotonic session id,
// and the session kind. The winit event loop, the pill, the worker thread, the
// transcriber, and the clipboard all live on the far side of the `Command`
// boundary as adapters. The two FSMs are separate instances, so holding one
// chord can never corrupt the other's press/release state.
//
// Clock is a parameter, never ambient — no `Instant::now()` in here, matching
// the activation FSM one level down, whose events already carry an `Instant`.

use std::time::{Duration, Instant};

use crate::activation::{self, InEvent, OutEvent};

/// How long the pill lingers on a successful delivery, showing the green
/// border before it fades and disappears.
pub const SUCCESS_LINGER: Duration = Duration::from_millis(500);

/// Failures linger longer than successes — a red flash the user might miss in
/// 500 ms deserves an extra beat to register as "that one didn't land".
pub const ERROR_LINGER: Duration = Duration::from_millis(1200);

/// How long the terminal flash holds before the pill fades out. Shared with the
/// pill adapter, which uses it to time the fade; the core uses it in `tick` to
/// decide auto-dismiss.
pub fn linger(ok: bool) -> Duration {
    if ok {
        SUCCESS_LINGER
    } else {
        ERROR_LINGER
    }
}

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

/// The pill's logical mode. The core assigns the mode and stamps the transition
/// instant; the adapter derives every frame's bars, breathing pulse, and fade
/// from it. `since` is the core's `now` at the transition, so the adapter's
/// animation clock and the core's dismiss clock share one origin.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PillMode {
    /// Live capture: the adapter animates bars from the ring buffer.
    Recording,
    /// Worker running: frozen bars under a breathing border.
    Processing { since: Instant },
    /// Terminal green/red flash before the pill is dropped.
    Done { ok: bool, since: Instant },
}

/// An effect for the adapter to perform. The core returns these; it never
/// performs them, so a test can assert on the list instead of on private state.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// Open the capture stream. The adapter reports the result back via
    /// [`Session::capture_started`].
    StartCapture,
    /// Set the pill's logical mode (creating the window on `Recording`).
    SetPill(PillMode),
    /// Hand the captured samples to a worker under `session_id`; the worker
    /// reports its [`Outcome`] back via [`Session::on_outcome`].
    SpawnTranscription {
        samples: Vec<f32>,
        session_id: u64,
        session_kind: SessionKind,
    },
    /// Tear the pill down immediately (no flash).
    DismissPill,
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
    /// (The pill's own Processing-since instant rides in `PillMode`, set by
    /// `end`; the core needs only the id here.)
    Processing { session_id: u64 },
    /// Terminal flash, dismissed by `tick` once the linger elapses.
    Done { ok: bool, since: Instant },
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
    /// capture stops (during Processing/Done a fresh chord may take over).
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
    /// Recording and shows the pill; `None` (open failed) resets the FSM so it
    /// doesn't believe it's recording — the capture/pill drift the PRD calls out.
    pub fn capture_started(&mut self, capture: Option<C>) -> Vec<Command> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Starting { kind } => match capture {
                Some(capture) => {
                    self.phase = Phase::Recording { kind, capture };
                    vec![Command::SetPill(PillMode::Recording)]
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
                    // Nothing usable captured — disappear, no flash.
                    tracing::info!(duration_ms, "session: STOP (too short, dropped)");
                    return vec![Command::DismissPill];
                }
                if !self.transcriber_available {
                    // No worker would ever resolve the pill; don't park it in
                    // Processing forever.
                    tracing::warn!(duration_ms, "session: STOP (no transcriber configured)");
                    return vec![Command::DismissPill];
                }
                tracing::info!(duration_ms, "session: STOP (transcribing)");
                self.session_seq += 1;
                let session_id = self.session_seq;
                self.phase = Phase::Processing { session_id };
                vec![
                    Command::SetPill(PillMode::Processing { since: now }),
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
            // Stop with nothing recording (e.g. a stray release): clear any
            // lingering pill, matching the old "STOP without active capture".
            _ => {
                tracing::warn!("session: STOP without active capture");
                vec![Command::DismissPill]
            }
        }
    }

    /// Apply a worker's reported outcome. Only the `Processing` phase that owns
    /// `session_id` reacts; an outcome from a superseded session is ignored, so
    /// a slow earlier worker can't repaint a later capture's pill.
    pub fn on_outcome(&mut self, session_id: u64, outcome: Outcome, now: Instant) -> Vec<Command> {
        let owns = matches!(self.phase, Phase::Processing { session_id: s, .. } if s == session_id);
        if !owns {
            return Vec::new();
        }
        match outcome {
            Outcome::Delivered => {
                self.phase = Phase::Done { ok: true, since: now };
                vec![Command::SetPill(PillMode::Done { ok: true, since: now })]
            }
            Outcome::Failed => {
                self.phase = Phase::Done { ok: false, since: now };
                vec![Command::SetPill(PillMode::Done { ok: false, since: now })]
            }
            // Nothing usable was said — just disappear, no flash.
            Outcome::Empty => {
                self.phase = Phase::Idle;
                vec![Command::DismissPill]
            }
        }
    }

    /// Retire the terminal flash once its linger has elapsed. Emits exactly one
    /// `DismissPill` on the crossing, then nothing — the animation is otherwise
    /// self-driven in the adapter.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        if let Phase::Done { ok, since } = self.phase {
            if now.saturating_duration_since(since) >= linger(ok) {
                self.phase = Phase::Idle;
                return vec![Command::DismissPill];
            }
        }
        Vec::new()
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

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_millis(ms)
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
        assert_eq!(
            s.capture_started(Some(long_capture())),
            vec![Command::SetPill(PillMode::Recording)]
        );
        let cmds = s.end(t(1000));
        match &cmds[..] {
            [Command::SetPill(PillMode::Processing { .. }), Command::SpawnTranscription {
                session_id, ..
            }] => *session_id,
            other => panic!("expected Processing + Spawn, got {other:?}"),
        }
    }

    #[test]
    fn happy_path_records_processes_and_flashes_green() {
        let mut s = ready_session();
        let id = into_processing(&mut s);
        assert!(s.is_processing());
        // The worker delivers: pill goes to a green Done flash.
        let cmds = s.on_outcome(id, Outcome::Delivered, t(1500));
        assert_eq!(
            cmds,
            vec![Command::SetPill(PillMode::Done {
                ok: true,
                since: t(1500)
            })]
        );
    }

    #[test]
    fn stale_outcome_emits_no_pill_command() {
        let mut s = ready_session();
        let id = into_processing(&mut s);
        // An outcome for a *different* session (the user re-triggered, or a
        // slow earlier worker) must not touch the pill.
        let cmds = s.on_outcome(id.wrapping_add(1), Outcome::Delivered, t(1500));
        assert!(cmds.is_empty(), "stale outcome produced commands: {cmds:?}");
        // And the real session is untouched — still processing.
        assert!(s.is_processing());
    }

    #[test]
    fn tick_past_linger_emits_exactly_one_dismiss() {
        let mut s = ready_session();
        let id = into_processing(&mut s);
        s.on_outcome(id, Outcome::Delivered, t(2000));
        // Before the linger elapses: nothing.
        let early = t(2000) + SUCCESS_LINGER - Duration::from_millis(1);
        assert!(s.tick(early).is_empty());
        // Once past it: one dismiss...
        let late = t(2000) + SUCCESS_LINGER;
        assert_eq!(s.tick(late), vec![Command::DismissPill]);
        // ...and never again.
        assert!(s.tick(late + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn second_press_while_processing_does_not_overlap_capture() {
        let mut s = ready_session();
        let first = into_processing(&mut s);
        // A fresh press while the worker still runs supersedes it.
        assert_eq!(s.begin(SessionKind::Dictate), vec![Command::StartCapture]);
        assert_eq!(
            s.capture_started(Some(long_capture())),
            vec![Command::SetPill(PillMode::Recording)]
        );
        assert!(s.is_recording());
        // Stopping the new capture mints a *new* id (no id reuse, no overlap).
        let second = into_processing_from_recording(&mut s);
        assert_ne!(first, second);
        // The superseded worker's outcome is now ignored.
        assert!(s.on_outcome(first, Outcome::Delivered, t(3000)).is_empty());
    }

    // Helper: stop a session already in Recording, asserting Processing.
    fn into_processing_from_recording(s: &mut Session<FakeCapture>) -> u64 {
        match &s.end(t(2000))[..] {
            [Command::SetPill(PillMode::Processing { .. }), Command::SpawnTranscription {
                session_id,
                ..
            }] => *session_id,
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
    fn too_short_capture_dismisses_without_transcribing() {
        let mut s = ready_session();
        s.begin(SessionKind::Dictate);
        s.capture_started(Some(FakeCapture {
            samples: vec![0.1; 10],
        }));
        // Below the floor: dismiss, no worker.
        assert_eq!(s.end(t(1000)), vec![Command::DismissPill]);
        assert!(s.is_idle());
    }

    #[test]
    fn no_transcriber_dismisses_instead_of_parking_in_processing() {
        let mut s = Session::new(Mode::Hold {
            double_press_lock: false,
        });
        // transcriber_available defaults to false.
        s.begin(SessionKind::Dictate);
        s.capture_started(Some(long_capture()));
        assert_eq!(s.end(t(1000)), vec![Command::DismissPill]);
        assert!(s.is_idle());
    }

    #[test]
    fn empty_outcome_dismisses_the_pill() {
        let mut s = ready_session();
        let id = into_processing(&mut s);
        assert_eq!(
            s.on_outcome(id, Outcome::Empty, t(1500)),
            vec![Command::DismissPill]
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
                Command::SetPill(PillMode::Processing { .. }),
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
        assert_eq!(input(s, InEvent::Pressed(t(0))), vec![Command::StartCapture]);
        assert_eq!(
            s.capture_started(Some(long_capture())),
            vec![Command::SetPill(PillMode::Recording)]
        );
        match &input(s, InEvent::Released(t(500)))[..] {
            [Command::SetPill(PillMode::Processing { .. }), Command::SpawnTranscription {
                session_kind,
                ..
            }] => *session_kind,
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
                Command::SetPill(PillMode::Processing { .. }),
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
