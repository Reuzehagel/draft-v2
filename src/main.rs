// Draft — Windows push-to-talk speech-to-text.
// Step-4 build: hotkey → FSM → cpal capture + pill window (static rounded
// rect, random bar heights, click-through).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activation;
mod audio;
mod autostart;
mod config;
mod history;
mod hotkey;
mod llm;
mod logging;
mod paste;
mod paths;
mod pill;
mod postprocess;
mod secrets;
mod settings_ui;
mod single_instance;
mod transcribe;
mod tray;
mod update;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::transcribe::Transcriber;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

const PILL_FRAME_RATE_HZ: u64 = 30;

/// How long the pill lingers on a successful delivery, showing the green
/// border before it fades and disappears.
const SUCCESS_LINGER: Duration = Duration::from_millis(500);

/// Failures linger longer than successes — a red flash the user might miss in
/// 500 ms deserves an extra beat to register as "that one didn't land".
const ERROR_LINGER: Duration = Duration::from_millis(1200);

/// Release the on-device model from RAM after this much dictation inactivity.
const MODEL_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--settings") {
        // Settings subprocess: own event loop, no single-instance gate,
        // no tray, no hotkey. Runs eframe and writes config when saved.
        let _log_guard = logging::init()?;
        return settings_ui::run();
    }

    let _log_guard = logging::init()?;

    let guard = match single_instance::acquire()? {
        Some(g) => g,
        None => {
            tracing::info!("another instance is already running; exiting");
            return Ok(());
        }
    };

    let (cfg, first_run) = config::Config::load_with_first_run()?;
    tracing::info!(?cfg, first_run, "config loaded");

    let _update_state = update::spawn_check();

    let tray = tray::build(&format!("Draft — {}", cfg.hotkey))?;
    let menu_rx = tray::menu_event_receiver();

    let command_spec = cfg.push_to_command.then(|| cfg.command_hotkey.clone());
    let (hotkey_handle, hotkey_rx) = hotkey::register(&cfg.hotkey, command_spec.as_deref())?;
    tracing::info!(hotkey = %cfg.hotkey, command = ?command_spec, "hotkeys registered");

    let fsm_mode = fsm_mode_from_config(&cfg);
    let fsm = activation::Fsm::new(fsm_mode);
    let command_fsm = activation::Fsm::new(fsm_mode);

    let transcriber: Option<Arc<dyn Transcriber>> = transcribe::build(&cfg);
    if transcriber.is_none() {
        tracing::warn!(
            "no transcriber available — set MISTRAL_API_KEY to enable paste-on-stop"
        );
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let (outcome_tx, outcome_rx) = crossbeam_channel::unbounded();

    let mut app = App {
        tray,
        menu_rx,
        hotkey_handle: Some(hotkey_handle),
        hotkey_rx,
        fsm,
        command_fsm,
        capture: None,
        session_kind: SessionKind::Dictate,
        pill: None,
        bands: audio::level::BandMeter::new(pill::BAR_COUNT),
        transcriber,
        cfg,
        settings_child: None,
        tail: None,
        outcome_tx,
        outcome_rx,
        session_seq: 0,
        last_bars: vec![0.0; pill::BAR_COUNT],
    };
    if first_run {
        tracing::info!("first run detected; opening settings");
        app.open_settings();
    }
    event_loop.run_app(&mut app)?;

    drop(guard);
    Ok(())
}

/// How long the terminal flash holds before the pill fades out — failures
/// linger longer than successes so they aren't missed.
fn tail_linger(ok: bool) -> Duration {
    if ok {
        SUCCESS_LINGER
    } else {
        ERROR_LINGER
    }
}

fn fsm_mode_from_config(cfg: &config::Config) -> activation::Mode {
    match cfg.activation {
        config::Activation::Toggle => activation::Mode::Toggle,
        config::Activation::Hold => activation::Mode::Hold {
            double_press_lock: cfg.double_press_lock,
        },
    }
}

/// Which pipeline a capture feeds: dictation pastes the (post-processed)
/// transcript; command sends it to the LLM and pastes the answer (issue #4).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionKind {
    Dictate,
    Command,
}

/// What a dictation worker thread reports back to the UI loop once it finishes,
/// so the pill can show an honest result instead of a premature "success".
enum Outcome {
    /// Text was produced and the paste call succeeded.
    Delivered,
    /// Transcription returned nothing usable — disappear quietly.
    Empty,
    /// Transcription or paste errored — the transcript is recoverable from
    /// History but never reached the cursor. Flash the pill red.
    Failed,
}

/// The pill's post-capture lifecycle. While `Processing`, the worker is still
/// transcribing/pasting; `Done` is the terminal green/red flash before the
/// pill is dropped. `session` ties a `Processing` pill to the worker that owns
/// it, so a slow earlier worker can't hijack the pill of a later capture.
#[derive(Clone, Copy)]
enum Tail {
    Processing { session: u64, since: Instant },
    Done { ok: bool, since: Instant },
}

struct App {
    tray: tray::Tray,
    menu_rx: crossbeam_channel::Receiver<tray_icon::menu::MenuEvent>,
    /// `None` only transiently during re-registration (and after a failed
    /// restore, where hotkeys are dead until restart).
    hotkey_handle: Option<hotkey::HotkeyHandle>,
    hotkey_rx: crossbeam_channel::Receiver<hotkey::HotkeyEvent>,
    fsm: activation::Fsm,
    /// Separate FSM for the push-to-command chord, so holding one hotkey
    /// can't corrupt the other's press/release state.
    command_fsm: activation::Fsm,
    capture: Option<audio::capture::Capture>,
    /// What the active (or most recent) capture is for: plain dictation, or
    /// a spoken instruction whose LLM answer gets pasted. Set at session
    /// start, read at stop to route the worker.
    session_kind: SessionKind,
    pill: Option<pill::window::PillWindow>,
    bands: audio::level::BandMeter,
    transcriber: Option<Arc<dyn Transcriber>>,
    cfg: config::Config,
    settings_child: Option<std::process::Child>,
    /// Post-capture pill state: `Processing` while a worker runs, then `Done`
    /// for the terminal green/red flash. `None` when idle or recording.
    tail: Option<Tail>,
    /// Workers report their outcome here; polled each loop on the UI thread.
    outcome_tx: crossbeam_channel::Sender<(u64, Outcome)>,
    outcome_rx: crossbeam_channel::Receiver<(u64, Outcome)>,
    /// Monotonic id stamped on each dispatched worker; matched on its outcome.
    session_seq: u64,
    /// Most recent waveform bars, frozen and reused during the tail animation.
    last_bars: Vec<f32>,
}

impl App {
    fn start_session(&mut self, el: &ActiveEventLoop) {
        match audio::capture::Capture::start(self.cfg.input_device.as_deref()) {
            Ok(cap) => {
                tracing::info!(
                    device = %cap.device_name,
                    input_sr = cap.input_sr,
                    channels = cap.input_channels,
                    "session: START"
                );
                self.capture = Some(cap);
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to start capture");
                return;
            }
        }

        // A quick re-trigger during the tail animation supersedes it; a still
        // in-flight worker's outcome is then ignored by its session id.
        self.tail = None;

        match pill::window::PillWindow::create(el) {
            Ok(mut pw) => {
                self.bands.reset();
                // Paint one frame BEFORE showing so the initial reveal is
                // already the pill (not a transparent rectangle).
                let initial = vec![0.0; pill::BAR_COUNT];
                let _ = pw.render_recording(&initial);
                pw.show();
                self.pill = Some(pw);
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to create pill window");
            }
        }
    }

    /// Put the most recent transcript back on the clipboard, so a paste that
    /// landed nowhere can be recovered with a manual Ctrl+V. No-op (logged) if
    /// the history is empty or the clipboard can't be opened.
    fn copy_last_transcription(&mut self) {
        match history::last() {
            Some(entry) => match paste::set_clipboard(&entry.text) {
                Ok(()) => tracing::info!("last transcript copied to clipboard"),
                Err(e) => tracing::error!(error = %e, "failed to copy last transcript"),
            },
            None => tracing::info!("copy last transcript: history is empty"),
        }
    }

    /// Apply a worker's reported outcome to the pill. Outcomes from a
    /// superseded session (the user re-triggered before this one finished) are
    /// ignored — only the `Processing` pill that owns `session` reacts.
    fn handle_outcome(&mut self, session: u64, outcome: Outcome) {
        let owns = matches!(self.tail, Some(Tail::Processing { session: s, .. }) if s == session);
        if !owns {
            return;
        }
        match outcome {
            Outcome::Delivered => {
                self.tail = Some(Tail::Done {
                    ok: true,
                    since: Instant::now(),
                });
            }
            Outcome::Failed => {
                self.tail = Some(Tail::Done {
                    ok: false,
                    since: Instant::now(),
                });
            }
            // Nothing usable was said — just disappear, no flash.
            Outcome::Empty => self.dismiss_pill(),
        }
    }

    /// Tear down the pill immediately (no tail animation).
    fn dismiss_pill(&mut self) {
        self.tail = None;
        if let Some(pw) = self.pill.take() {
            drop(pw);
        }
    }

    fn stop_session(&mut self) {
        // Stop capturing immediately (this freezes the bars), but keep the
        // pill window around — if we end up dispatching a transcription we
        // replace the bars with a brief green-checkmark "success" animation.
        let Some(cap) = self.capture.take() else {
            self.dismiss_pill();
            tracing::warn!("session: STOP without active capture");
            return;
        };
        let samples = cap.buffer.take();
        let duration_ms = samples.len() as u64 * 1000 / audio::TARGET_SR as u64;
        if samples.len() < (audio::TARGET_SR as usize * 150) / 1000 {
            // Nothing usable captured — just disappear, no success checkmark.
            self.dismiss_pill();
            tracing::info!(duration_ms, "session: STOP (too short, dropped)");
            return;
        }
        let path = wav_dump_path();
        if let Err(e) = write_wav(&path, &samples) {
            tracing::error!(error = %e, "failed to write wav dump");
        } else {
            tracing::info!(
                duration_ms,
                path = %path.display(),
                samples = samples.len(),
                "session: STOP (wav written)"
            );
        }

        let Some(transcriber) = self.transcriber.clone() else {
            self.dismiss_pill();
            tracing::warn!("no transcriber configured; skipping paste");
            return;
        };

        // We're committing to a transcription — hold the pill in its
        // "processing" state until the worker reports back what really
        // happened, then show green (delivered) or red (failed).
        self.session_seq += 1;
        let session = self.session_seq;
        self.tail = Some(Tail::Processing {
            session,
            since: Instant::now(),
        });
        let outcome_tx = self.outcome_tx.clone();

        let append_space = self.cfg.append_trailing_space;
        let restore_clipboard = self.cfg.restore_clipboard;
        let pipeline = postprocess::Pipeline::from_config(&self.cfg);
        let paste_mode = match self.cfg.paste_mode {
            config::PasteMode::Clipboard => paste::PasteMode::Clipboard,
            config::PasteMode::Unicode => paste::PasteMode::Unicode,
        };
        let kind = self.session_kind;
        // Fetch the key on the UI thread — the keyring is process-global
        // state, no reason to touch it from every worker.
        let groq_key = match kind {
            SessionKind::Command => secrets::load_key(config::Provider::Groq),
            SessionKind::Dictate => None,
        };
        std::thread::spawn(move || {
            // Send the worker's verdict to the UI loop. The receiver outlives
            // every worker (it's owned by App), so a failed send only means the
            // app is shutting down — nothing to recover.
            let report = |o: Outcome| {
                let _ = outcome_tx.send((session, o));
            };
            let started = Instant::now();
            // Attribution rides with the result so history credits whichever
            // provider actually served this call (the fallback wrapper can
            // route to local Parakeet mid-call).
            let (text, stt_provider) = match transcriber.transcribe_attributed(&samples) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "transcription failed");
                    report(Outcome::Failed);
                    return;
                }
            };
            let elapsed_ms = started.elapsed().as_millis();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                tracing::info!(elapsed_ms, "transcription empty; nothing to paste");
                report(Outcome::Empty);
                return;
            }

            // What lands at the cursor, and who is credited in history.
            // Dictation runs the deterministic pipeline over the transcript;
            // push-to-command treats it as an instruction instead — no
            // pipeline (replacements and voice commands are for spoken
            // prose), the LLM's answer is what gets pasted.
            let (mut out, provider): (String, &'static str) = match kind {
                SessionKind::Command => {
                    let Some(key) = groq_key else {
                        tracing::error!(
                            "push-to-command needs a Groq API key — add one under \
                             Settings > Transcription with Groq selected"
                        );
                        report(Outcome::Failed);
                        return;
                    };
                    match llm::run_command(&key, trimmed) {
                        Ok(answer) => (answer, "command"),
                        Err(e) => {
                            tracing::error!(error = %e, "command transform failed");
                            report(Outcome::Failed);
                            return;
                        }
                    }
                }
                SessionKind::Dictate => (pipeline.apply(trimmed), stt_provider),
            };

            // Either stage can legitimately empty the text (a lone "scratch
            // that", a delete-everything replacement, a refusing model) —
            // don't paste a bare trailing space or record an empty entry.
            if out.trim().is_empty() {
                tracing::info!(elapsed_ms, "nothing left to paste");
                report(Outcome::Empty);
                return;
            }
            // Record the text BEFORE attempting paste: if the paste is
            // swallowed or lands in the wrong window, this is the only
            // surviving copy. Stored without the cosmetic trailing space.
            if let Err(e) = history::append(&out, provider) {
                tracing::warn!(error = %e, "failed to record transcript in history");
            }
            if append_space {
                out.push(' ');
            }
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis(),
                provider,
                chars = out.len(),
                "transcription complete"
            );
            // `deliver_text` reports Delivered the moment the paste keystroke
            // is sent, then keeps the thread alive briefly for clipboard
            // restore housekeeping — the pill shouldn't wait on that.
            if let Err(e) =
                paste::deliver_text(&out, paste_mode, restore_clipboard, || {
                    report(Outcome::Delivered)
                })
            {
                tracing::error!(error = %e, "paste failed");
                report(Outcome::Failed);
            }
        });
    }
}

fn wav_dump_path() -> PathBuf {
    std::env::temp_dir().join("draft-last.wav")
}

fn write_wav(path: &std::path::Path, samples: &[f32]) -> Result<()> {
    let bytes = transcribe::samples_to_wav_bytes(samples, audio::TARGET_SR)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

impl ApplicationHandler for App {
    fn resumed(&mut self, _el: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _el: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::RedrawRequested) {
            self.redraw_pill();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.poll_settings_child();
        while let Ok(ev) = self.menu_rx.try_recv() {
            if ev.id == self.tray.menu_ids.quit {
                tracing::info!("quit requested from tray");
                el.exit();
            } else if ev.id == self.tray.menu_ids.settings {
                self.open_settings();
            } else if ev.id == self.tray.menu_ids.copy_last {
                self.copy_last_transcription();
            }
        }

        while let Ok(ev) = self.hotkey_rx.try_recv() {
            let (chord, in_ev) = match ev {
                hotkey::HotkeyEvent::Pressed(c, t) => (c, activation::InEvent::Pressed(t)),
                hotkey::HotkeyEvent::Released(c, t) => (c, activation::InEvent::Released(t)),
            };
            let kind = match chord {
                hotkey::Chord::Dictate => SessionKind::Dictate,
                hotkey::Chord::Command => SessionKind::Command,
            };
            // A capture belongs to the chord that started it. Swallow the
            // other chord's events for the duration so the two FSMs can't
            // fight over one microphone.
            if self.capture.is_some() && self.session_kind != kind {
                continue;
            }
            let fsm = match chord {
                hotkey::Chord::Dictate => &mut self.fsm,
                hotkey::Chord::Command => &mut self.command_fsm,
            };
            match fsm.step(in_ev) {
                activation::OutEvent::Start => {
                    self.session_kind = kind;
                    self.start_session(el);
                }
                activation::OutEvent::Stop => self.stop_session(),
                activation::OutEvent::Ignore => {}
            }
        }

        // Workers report their outcome here; transition the pill to its
        // terminal green/red flash (or dismiss it on an empty result).
        while let Ok((session, outcome)) = self.outcome_rx.try_recv() {
            self.handle_outcome(session, outcome);
        }

        // Free the on-device model if dictation has been idle long enough.
        // Cheap (try_lock + elapsed check); a no-op for cloud providers.
        if let Some(t) = self.transcriber.as_ref() {
            t.unload_if_idle(MODEL_IDLE_TIMEOUT);
        }

        // Retire the pill once its terminal flash has run its course.
        if let Some(Tail::Done { ok, since }) = self.tail {
            if since.elapsed() >= tail_linger(ok) {
                self.dismiss_pill();
            }
        }

        // When the pill is up, drive frame redraws ourselves at ~30 Hz.
        // Otherwise idle wait so we don't spin.
        if self.pill.is_some() {
            self.redraw_pill();
            el.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(1000 / PILL_FRAME_RATE_HZ),
            ));
        } else {
            el.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(50)));
        }
    }
}

impl App {
    fn open_settings(&mut self) {
        if let Some(child) = self.settings_child.as_mut() {
            // Already running — drop a log line and don't spawn a second.
            if child.try_wait().ok().flatten().is_none() {
                tracing::info!("settings window already open");
                return;
            }
        }
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "current_exe failed");
                return;
            }
        };
        match std::process::Command::new(&exe).arg("--settings").spawn() {
            Ok(child) => {
                tracing::info!(pid = child.id(), "settings subprocess spawned");
                self.settings_child = Some(child);
            }
            Err(e) => tracing::error!(error = %e, "failed to spawn settings subprocess"),
        }
    }

    fn poll_settings_child(&mut self) {
        let Some(child) = self.settings_child.as_mut() else { return };
        match child.try_wait() {
            Ok(Some(status)) => {
                tracing::info!(?status, "settings subprocess exited; reloading config");
                self.settings_child = None;
                self.reload_config();
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "settings child try_wait failed");
                self.settings_child = None;
            }
        }
    }

    fn reload_config(&mut self) {
        let new_cfg = match config::Config::load() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "config reload failed");
                return;
            }
        };

        let bindings_changed = new_cfg.hotkey != self.cfg.hotkey
            || new_cfg.push_to_command != self.cfg.push_to_command
            || new_cfg.command_hotkey != self.cfg.command_hotkey;
        if bindings_changed {
            // Release the old bindings first — when only the command chord
            // changed, re-registering the unchanged main hotkey would
            // otherwise collide with our own still-live registration.
            self.hotkey_handle = None;
            let command_spec = new_cfg.push_to_command.then(|| new_cfg.command_hotkey.clone());
            match hotkey::register(&new_cfg.hotkey, command_spec.as_deref()) {
                Ok((handle, rx)) => {
                    self.hotkey_handle = Some(handle);
                    self.hotkey_rx = rx;
                    tracing::info!(
                        hotkey = %new_cfg.hotkey,
                        command = ?command_spec,
                        "hotkeys re-registered"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "hotkey re-register failed; restoring previous binding");
                    let old_spec = self
                        .cfg
                        .push_to_command
                        .then(|| self.cfg.command_hotkey.clone());
                    match hotkey::register(&self.cfg.hotkey, old_spec.as_deref()) {
                        Ok((handle, rx)) => {
                            self.hotkey_handle = Some(handle);
                            self.hotkey_rx = rx;
                        }
                        Err(e) => tracing::error!(
                            error = %e,
                            "restore failed; hotkeys inactive until restart"
                        ),
                    }
                }
            }
        }

        let fsm_mode = fsm_mode_from_config(&new_cfg);
        self.fsm = activation::Fsm::new(fsm_mode);
        self.command_fsm = activation::Fsm::new(fsm_mode);
        self.transcriber = transcribe::build(&new_cfg);
        self.cfg = new_cfg;
    }

    fn redraw_pill(&mut self) {
        let Some(pill) = self.pill.as_mut() else { return };

        // Post-capture states take over the pill until it's dismissed.
        match self.tail {
            // Terminal flash: green (delivered) or red (failed) border over the
            // frozen bars, holding then fading over the final 30% of the linger.
            Some(Tail::Done { ok, since }) => {
                let t = (since.elapsed().as_secs_f32() / tail_linger(ok).as_secs_f32())
                    .clamp(0.0, 1.0);
                let alpha = if t < 0.7 { 1.0 } else { ((1.0 - t) / 0.3).clamp(0.0, 1.0) };
                let res = if ok {
                    pill.render_success(&self.last_bars, alpha)
                } else {
                    pill.render_error(&self.last_bars, alpha)
                };
                if let Err(e) = res {
                    tracing::error!(error = %e, "pill outcome render failed");
                }
                return;
            }
            // Worker still running: frozen bars under a neutral border that
            // breathes (~0.8 Hz) so a slow round-trip reads as live, not hung.
            Some(Tail::Processing { since, .. }) => {
                let e = since.elapsed().as_secs_f32();
                let pulse = 0.5 - 0.5 * (e * std::f32::consts::TAU * 0.8).cos();
                if let Err(e) = pill.render_processing(&self.last_bars, pulse) {
                    tracing::error!(error = %e, "pill processing render failed");
                }
                return;
            }
            None => {}
        }

        let bars = if let Some(cap) = self.capture.as_ref() {
            let raw = self.bands.tick(&cap.buffer).to_vec();
            audio::level::shape_bars(&raw)
        } else {
            vec![0.0; pill::BAR_COUNT]
        };
        self.last_bars = bars.clone();
        if let Err(e) = pill.render_recording(&bars) {
            tracing::error!(error = %e, "pill render failed");
        }
    }
}
