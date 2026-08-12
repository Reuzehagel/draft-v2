// Draft — Windows push-to-talk speech-to-text.
//
// The dictation lifecycle lives in `session` as a pure command-returning core.
// This file is the adapter: it translates winit/hotkey/tray events into
// `Session` inputs, executes the `Command`s the core returns (open the mic,
// drive the pill, spawn a worker), and feeds worker outcomes back by id.

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
mod session;
mod settings_ui;
mod single_instance;
mod transcribe;
mod tray;
mod update;

use anyhow::Result;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::session::{Command, PillMode, Session, SessionKind};
use crate::transcribe::Transcriber;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

const PILL_FRAME_RATE_HZ: u64 = 30;

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

    let update_rx = update::spawn_check();

    let tray = tray::build(&tray_status(&cfg, None))?;
    let menu_rx = tray::menu_event_receiver();

    let command_spec = cfg.push_to_command.then(|| cfg.command_hotkey.clone());
    let (hotkey_handle, hotkey_rx) = hotkey::register(&cfg.hotkey, command_spec.as_deref())?;
    tracing::info!(hotkey = %cfg.hotkey, command = ?command_spec, "hotkeys registered");

    let fsm_mode = fsm_mode_from_config(&cfg);

    let transcriber: Option<Arc<dyn Transcriber>> = transcribe::build(&cfg);
    if transcriber.is_none() {
        tracing::warn!("no transcriber available — set MISTRAL_API_KEY to enable paste-on-stop");
    }

    let mut session = Session::new(fsm_mode);
    session.set_transcriber_available(transcriber.is_some());

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let (outcome_tx, outcome_rx) = crossbeam_channel::unbounded();

    let mut app = App {
        tray,
        menu_rx,
        hotkey_handle: Some(hotkey_handle),
        hotkey_rx,
        session,
        pill: PillAdapter::new(),
        transcriber,
        cfg,
        settings_child: None,
        outcome_tx,
        outcome_rx,
        update_rx,
        update_available: None,
    };
    if first_run {
        tracing::info!("first run detected; opening settings");
        app.open_settings();
    }
    event_loop.run_app(&mut app)?;

    drop(guard);
    Ok(())
}

/// Gather what the tray should currently say. Called at the events that can
/// change it — startup, config reload, a finished dictation, the update check
/// reporting — never on a timer.
fn tray_status(cfg: &config::Config, update: Option<&update::UpdateInfo>) -> tray::Status {
    tray::Status {
        hotkey: cfg.hotkey.clone(),
        provider: cfg.provider,
        update: update.map(|u| u.latest_version.clone()),
        has_history: !history::is_empty(),
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

struct App {
    tray: tray::Tray,
    menu_rx: crossbeam_channel::Receiver<tray_icon::menu::MenuEvent>,
    /// `None` only transiently during re-registration (and after a failed
    /// restore, where hotkeys are dead until restart).
    hotkey_handle: Option<hotkey::HotkeyHandle>,
    hotkey_rx: crossbeam_channel::Receiver<hotkey::HotkeyEvent>,
    /// The pure dictation lifecycle. Owns both activation FSMs (dictate and
    /// push-to-command), the capture handle, tail, session id, and session kind;
    /// hands back `Command`s to perform.
    session: Session<audio::capture::Capture>,
    pill: PillAdapter,
    transcriber: Option<Arc<dyn Transcriber>>,
    cfg: config::Config,
    settings_child: Option<std::process::Child>,
    /// Workers report their outcome here; polled each loop on the UI thread.
    outcome_tx: crossbeam_channel::Sender<(u64, session::Outcome)>,
    outcome_rx: crossbeam_channel::Receiver<(u64, session::Outcome)>,
    /// One-shot: the update check reports here if a newer release exists.
    update_rx: crossbeam_channel::Receiver<update::UpdateInfo>,
    /// Kept so the tooltip still mentions the update after later refreshes.
    update_available: Option<update::UpdateInfo>,
}

impl App {
    /// Execute the `Command`s the core returned. `StartCapture` is the one
    /// effect that reports a result straight back into the core, whose follow-up
    /// commands (show the pill, or nothing on failure) are executed in turn.
    fn run_commands(&mut self, cmds: Vec<Command>, el: &ActiveEventLoop) {
        let mut queue: VecDeque<Command> = cmds.into_iter().collect();
        while let Some(cmd) = queue.pop_front() {
            match cmd {
                Command::StartCapture => {
                    let more = match audio::capture::Capture::start(self.cfg.input_device.as_deref())
                    {
                        Ok(cap) => {
                            tracing::info!(
                                device = %cap.device_name,
                                input_sr = cap.input_sr,
                                channels = cap.input_channels,
                                "session: START"
                            );
                            // The pill animates live bars from a read-only clone
                            // of the ring buffer; the core keeps the handle it
                            // drains at stop.
                            self.pill.set_ring(cap.buffer.clone());
                            self.session.capture_started(Some(cap))
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "failed to start capture");
                            self.session.capture_started(None)
                        }
                    };
                    queue.extend(more);
                }
                Command::SetPill(mode) => self.pill.set_mode(mode, el),
                Command::DismissPill => self.pill.dismiss(),
                Command::SpawnTranscription {
                    samples,
                    session_id,
                    session_kind,
                } => self.spawn_worker(samples, session_id, session_kind),
            }
        }
    }

    /// Off-thread pipeline for a committed capture: transcribe → postprocess →
    /// history → paste, reporting one [`session::Outcome`] back by id.
    ///
    /// History is recorded *before* the paste attempt — if the paste is
    /// swallowed or lands in the wrong window, that record is the only surviving
    /// copy. Do not reorder.
    fn spawn_worker(&self, samples: Vec<f32>, session_id: u64, kind: SessionKind) {
        let Some(transcriber) = self.transcriber.clone() else {
            // The core gates on transcriber availability, so this is defensive:
            // resolve the pill instead of parking it in Processing.
            tracing::warn!("no transcriber configured; skipping paste");
            let _ = self.outcome_tx.send((session_id, session::Outcome::Empty));
            return;
        };
        let outcome_tx = self.outcome_tx.clone();
        let append_space = self.cfg.append_trailing_space;
        let restore_clipboard = self.cfg.restore_clipboard;
        let pipeline = postprocess::Pipeline::from_config(&self.cfg);
        let paste_mode = match self.cfg.paste_mode {
            config::PasteMode::Clipboard => paste::PasteMode::Clipboard,
            config::PasteMode::Unicode => paste::PasteMode::Unicode,
        };
        // Fetch the key on the UI thread — the keyring is process-global state,
        // no reason to touch it from every worker.
        let groq_key = match kind {
            SessionKind::Command => secrets::load_key(config::Provider::Groq),
            SessionKind::Dictate => None,
        };
        std::thread::spawn(move || {
            // Debug artifact: the last capture, on disk as a wav.
            let path = wav_dump_path();
            match write_wav(&path, &samples) {
                Ok(()) => tracing::info!(
                    samples = samples.len(),
                    path = %path.display(),
                    "session: STOP (wav written)"
                ),
                Err(e) => tracing::error!(error = %e, "failed to write wav dump"),
            }

            // Send the worker's verdict to the UI loop. The receiver outlives
            // every worker (it's owned by App), so a failed send only means the
            // app is shutting down — nothing to recover.
            let report = |o: session::Outcome| {
                let _ = outcome_tx.send((session_id, o));
            };
            let started = Instant::now();
            // Attribution rides with the result so history credits whichever
            // provider actually served this call (the fallback wrapper can
            // route to local Parakeet mid-call).
            let (text, stt_provider) = match transcriber.transcribe_attributed(&samples) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "transcription failed");
                    report(session::Outcome::Failed);
                    return;
                }
            };
            let elapsed_ms = started.elapsed().as_millis();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                tracing::info!(elapsed_ms, "transcription empty; nothing to paste");
                report(session::Outcome::Empty);
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
                        report(session::Outcome::Failed);
                        return;
                    };
                    match llm::run_command(&key, trimmed) {
                        Ok(answer) => (answer, "command"),
                        Err(e) => {
                            tracing::error!(error = %e, "command transform failed");
                            report(session::Outcome::Failed);
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
                report(session::Outcome::Empty);
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
            if let Err(e) = paste::deliver_text(&out, paste_mode, restore_clipboard, || {
                report(session::Outcome::Delivered)
            }) {
                tracing::error!(error = %e, "paste failed");
                report(session::Outcome::Failed);
            }
        });
    }

    /// Re-read what the tray should say and push it to the shell.
    fn refresh_tray(&self) {
        self.tray
            .apply(&tray_status(&self.cfg, self.update_available.as_ref()));
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

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::RedrawRequested) {
            self.pill.redraw();
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
            if matches!(self.session.capturing_kind(), Some(active) if active != kind) {
                continue;
            }
            // Both chords now run entirely through the pure `Session` core,
            // which owns an independent FSM for each.
            let cmds = match chord {
                hotkey::Chord::Dictate => self.session.on_dictate_input(in_ev),
                hotkey::Chord::Command => self.session.on_command_input(in_ev),
            };
            self.run_commands(cmds, el);
        }

        // Workers report their outcome here; the core transitions the pill to
        // its terminal flash (or dismisses it) and ignores stale ids.
        while let Ok((id, outcome)) = self.outcome_rx.try_recv() {
            let cmds = self.session.on_outcome(id, outcome, Instant::now());
            self.run_commands(cmds, el);
            // A finished dictation may have been the first transcript ever
            // recorded, which is what enables "Copy last transcription".
            self.refresh_tray();
        }

        // At most one message, only when a newer release exists.
        if let Ok(info) = self.update_rx.try_recv() {
            self.update_available = Some(info);
            self.refresh_tray();
        }

        // Free the on-device model if dictation has been idle long enough.
        // Cheap (try_lock + elapsed check); a no-op for cloud providers.
        if let Some(t) = self.transcriber.as_ref() {
            t.unload_if_idle(MODEL_IDLE_TIMEOUT);
        }

        // Retire the pill once its terminal flash has run its course.
        let cmds = self.session.tick(Instant::now());
        self.run_commands(cmds, el);

        // When the pill is up, drive frame redraws ourselves at ~30 Hz.
        // Otherwise idle wait so we don't spin.
        if self.pill.is_active() {
            self.pill.redraw();
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
        let Some(child) = self.settings_child.as_mut() else {
            return;
        };
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
                // The settings session may still have cleared the history even
                // though its config didn't load — don't leave the menu stale.
                self.refresh_tray();
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
        self.session.reset_activation(fsm_mode);
        self.transcriber = transcribe::build(&new_cfg);
        self.session
            .set_transcriber_available(self.transcriber.is_some());
        self.cfg = new_cfg;
        // The tooltip names the hotkey and provider, and settings can clear
        // the history — so the tray follows a settings change without a restart.
        self.refresh_tray();
    }
}

/// The pill window and its animation, driven by a logical [`PillMode`] the core
/// assigns. The core decides *what* mode and *when* to transition; the adapter
/// derives every frame's bars, breathing pulse, and fade — including freezing
/// the bars the moment the mode leaves `Recording`.
struct PillAdapter {
    window: Option<pill::window::PillWindow>,
    bands: audio::level::BandMeter,
    /// Most recent waveform bars, frozen and reused once capture stops.
    last_bars: Vec<f32>,
    /// The current logical mode; `None` when no pill is shown.
    mode: Option<PillMode>,
    /// Read-only clone of the active capture's ring buffer, for live bars.
    ring: Option<audio::ring::Buffer>,
}

impl PillAdapter {
    fn new() -> Self {
        Self {
            window: None,
            bands: audio::level::BandMeter::new(pill::BAR_COUNT),
            last_bars: vec![0.0; pill::BAR_COUNT],
            mode: None,
            ring: None,
        }
    }

    fn set_ring(&mut self, ring: audio::ring::Buffer) {
        self.ring = Some(ring);
    }

    fn is_active(&self) -> bool {
        self.window.is_some()
    }

    /// Apply a mode set by the core. `Recording` starts a fresh session — a new
    /// window, dropping any lingering flash pill; the rest just swap the mode,
    /// so the next redraw freezes the bars (Processing/Done render `last_bars`
    /// without touching the ring).
    fn set_mode(&mut self, mode: PillMode, el: &ActiveEventLoop) {
        if let PillMode::Recording = mode {
            match pill::window::PillWindow::create(el) {
                Ok(mut pw) => {
                    self.bands.reset();
                    // Paint one frame BEFORE showing so the initial reveal is
                    // already the pill (not a transparent rectangle).
                    let initial = vec![0.0; pill::BAR_COUNT];
                    let _ = pw.render_recording(&initial);
                    pw.show();
                    self.window = Some(pw);
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to create pill window");
                    self.window = None;
                }
            }
        }
        self.mode = Some(mode);
    }

    /// Tear the pill down immediately (no flash).
    fn dismiss(&mut self) {
        self.mode = None;
        self.ring = None;
        if let Some(pw) = self.window.take() {
            drop(pw);
        }
    }

    fn redraw(&mut self) {
        let Some(pill) = self.window.as_mut() else {
            return;
        };
        match self.mode {
            // Terminal flash: green (delivered) or red (failed) border over the
            // frozen bars, holding then fading over the final 30% of the linger.
            Some(PillMode::Done { ok, since }) => {
                let total = session::linger(ok).as_secs_f32();
                let t = (since.elapsed().as_secs_f32() / total).clamp(0.0, 1.0);
                let alpha = if t < 0.7 {
                    1.0
                } else {
                    ((1.0 - t) / 0.3).clamp(0.0, 1.0)
                };
                let res = if ok {
                    pill.render_success(&self.last_bars, alpha)
                } else {
                    pill.render_error(&self.last_bars, alpha)
                };
                if let Err(e) = res {
                    tracing::error!(error = %e, "pill outcome render failed");
                }
            }
            // Worker still running: frozen bars under a neutral border that
            // breathes (~0.8 Hz) so a slow round-trip reads as live, not hung.
            Some(PillMode::Processing { since }) => {
                let e = since.elapsed().as_secs_f32();
                let pulse = 0.5 - 0.5 * (e * std::f32::consts::TAU * 0.8).cos();
                if let Err(e) = pill.render_processing(&self.last_bars, pulse) {
                    tracing::error!(error = %e, "pill processing render failed");
                }
            }
            // Live capture (or a just-created window): animate bars from the
            // ring buffer and keep `last_bars` current for the freeze.
            Some(PillMode::Recording) | None => {
                let bars = if let Some(ring) = self.ring.as_ref() {
                    let raw = self.bands.tick(ring).to_vec();
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
    }
}
