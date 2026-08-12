// Draft — Windows push-to-talk speech-to-text.
//
// The dictation lifecycle lives in `session` as a pure command-returning core,
// and the pill's own life in `pill::core` as a second one beside it. This file
// is the adapter: it translates winit/hotkey/tray events into `Session` inputs,
// executes the `Command`s that core returns (open the mic, spawn a worker, and
// report what the session is doing to the Pill core), performs the Pill core's
// commands against a real window, and feeds worker outcomes back by id.

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

use crate::pill::core::{Pill, PillMode};
use crate::pill::hook::HookEvent;
use crate::session::{Command, Session, SessionKind};
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
    // Owned here rather than by the window, so the pill can be created and
    // destroyed under a receiver that outlives every one of them.
    let (hook_tx, hook_rx) = crossbeam_channel::unbounded();

    let mut app = App {
        tray,
        menu_rx,
        hotkey_handle: Some(hotkey_handle),
        hotkey_rx,
        session,
        pill_core: Pill::new(),
        pill: PillAdapter::new(hook_tx),
        hook_rx,
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
    /// push-to-command), the capture handle, session id, and session kind;
    /// hands back `Command`s to perform.
    session: Session<audio::capture::Capture>,
    /// The pure owner of the pill's life. `Session` is one of its drivers; the
    /// residency toggle, the fullscreen watcher and the hover poller follow.
    pill_core: Pill,
    pill: PillAdapter,
    /// The messages winit doesn't surface, posted by the pill window's wndproc
    /// subclass. Nothing acts on them yet — the home monitor (#43), the
    /// fullscreen watcher (#45) and the wakeup ladder (#49) are their consumers.
    hook_rx: crossbeam_channel::Receiver<HookEvent>,
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
    /// Execute the `Command`s the session core returned. `StartCapture` is the
    /// one effect that reports a result straight back into the core, whose
    /// follow-up commands (a Recording report, or nothing on failure) are
    /// executed in turn.
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
                Command::ReportActivity(activity) => {
                    let cmds = self.pill_core.on_session(activity, Instant::now());
                    self.run_pill_commands(cmds, el);
                }
                Command::SpawnTranscription {
                    samples,
                    session_id,
                    session_kind,
                } => self.spawn_worker(samples, session_id, session_kind),
            }
        }
    }

    /// Perform the Pill core's commands. Nothing here decides anything: the
    /// core says create/show/hide/destroy and which mode, the adapter obeys.
    fn run_pill_commands(&mut self, cmds: Vec<pill::core::Command>, el: &ActiveEventLoop) {
        for cmd in cmds {
            match cmd {
                pill::core::Command::Create => self.pill.create(el),
                pill::core::Command::SetMode(mode) => self.pill.set_mode(mode),
                pill::core::Command::Show => self.pill.show(),
                pill::core::Command::Hide => self.pill.hide(),
                pill::core::Command::Destroy => self.pill.destroy(),
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

        // Workers report their outcome here; the session core ignores stale ids
        // and reports its last activity, which the Pill core turns into a
        // terminal flash (or into nothing, when there was nothing to say).
        while let Ok((id, outcome)) = self.outcome_rx.try_recv() {
            let cmds = self.session.on_outcome(id, outcome);
            self.run_commands(cmds, el);
            // A finished dictation may have been the first transcript ever
            // recorded, which is what enables "Copy last transcription".
            self.refresh_tray();
        }

        // The messages winit doesn't surface, arriving from the pill window's
        // wndproc subclass. Drained and logged rather than acted on: the hook
        // is a prefactor, and its consumers land one ticket at a time. Draining
        // is not optional — an unread channel would grow for the life of the
        // process.
        while let Ok(ev) = self.hook_rx.try_recv() {
            match ev {
                HookEvent::DisplayChanged => tracing::info!("display topology changed"),
                HookEvent::DpiChanged { dpi } => tracing::info!(dpi, "pill monitor dpi changed"),
                HookEvent::DisplayPower { on } => tracing::info!(on, "session display power"),
                HookEvent::SessionLock { locked } => tracing::info!(locked, "session lock"),
            }
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
        let cmds = self.pill_core.tick(Instant::now());
        self.run_pill_commands(cmds, el);

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

/// The pill window and its animation, driven by the [`PillMode`] the Pill core
/// derives. The core decides *what* mode, *when* to transition, and whether a
/// window exists at all; the adapter derives every frame's bars, breathing
/// pulse, and fade — including easing the bars flat once the mode leaves
/// `Recording`. It holds no lifecycle rules.
struct PillAdapter {
    window: Option<pill::window::PillWindow>,
    /// Handed to each window it creates, so the wndproc hook can post to the
    /// app loop.
    hook_tx: crossbeam_channel::Sender<HookEvent>,
    bands: audio::level::BandMeter,
    /// The current logical mode; `None` when no pill is shown.
    mode: Option<PillMode>,
    /// When the handoff started, i.e. when the mode last became `Processing`.
    ///
    /// Kept on the adapter rather than read off the mode because the handoff
    /// can outlive `Processing`: a worker that resolves inside 320 ms puts the
    /// pill in `Done` mid-fall, and `Done`'s own `since` is the flash's clock,
    /// not the handoff's. Without this the bars would snap flat on that frame —
    /// exactly the seam the handoff exists to remove.
    handoff_since: Option<Instant>,
    /// Read-only clone of the active capture's ring buffer, for live bars.
    ring: Option<audio::ring::Buffer>,
}

impl PillAdapter {
    fn new(hook_tx: crossbeam_channel::Sender<HookEvent>) -> Self {
        Self {
            window: None,
            hook_tx,
            bands: audio::level::BandMeter::new(pill::BAR_COUNT),
            mode: None,
            handoff_since: None,
            ring: None,
        }
    }

    fn set_ring(&mut self, ring: audio::ring::Buffer) {
        self.ring = Some(ring);
    }

    fn is_active(&self) -> bool {
        self.window.is_some()
    }

    /// Build the window, off screen. A failure leaves us without one; every
    /// later command is a no-op until the core asks for another.
    fn create(&mut self, el: &ActiveEventLoop) {
        match pill::window::PillWindow::create(el, self.hook_tx.clone()) {
            Ok(pw) => self.window = Some(pw),
            Err(e) => {
                tracing::error!(error = %e, "failed to create pill window");
                self.window = None;
            }
        }
    }

    /// Apply a mode the core derived. Entering `Recording` starts the meter from
    /// silence and paints one frame immediately, so the reveal that follows is
    /// already the pill (not a transparent rectangle); the rest just swap the
    /// mode, and the next redraw picks the bars up from there.
    ///
    /// Note what is *not* reset on the way out of `Recording`: the meter keeps
    /// its clock, so the waveform running under the handoff is the same one that
    /// was running a frame earlier, with no sideways jump at the mode change.
    fn set_mode(&mut self, mode: PillMode) {
        let entering_recording = matches!(mode, PillMode::Recording { .. })
            && !matches!(self.mode, Some(PillMode::Recording { .. }));
        // Stamp the handoff on the way *into* Processing only, so a `Done` that
        // follows keeps counting from the mode change rather than restarting.
        if let PillMode::Processing { since } = mode {
            if !matches!(self.mode, Some(PillMode::Processing { .. })) {
                self.handoff_since = Some(since);
            }
        }
        self.mode = Some(mode);
        if entering_recording {
            self.handoff_since = None;
            self.bands.reset();
            if let Some(pw) = self.window.as_mut() {
                let _ = pw.render_recording(&flat_bars());
            }
        }
    }

    fn show(&self) {
        if let Some(pw) = self.window.as_ref() {
            pw.show();
        }
    }

    fn hide(&self) {
        if let Some(pw) = self.window.as_ref() {
            pw.hide();
        }
    }

    /// Tear the window down. The ring goes with it — it belongs to a capture
    /// that is long over by the time the pill has no reason to exist.
    fn destroy(&mut self) {
        self.mode = None;
        self.ring = None;
        self.handoff_since = None;
        drop(self.window.take());
    }

    fn redraw(&mut self) {
        let Some(pill) = self.window.as_mut() else {
            return;
        };
        match self.mode {
            // Terminal flash: green (delivered) or red (failed) border over the
            // bar row, holding then fading over the final 30% of the linger.
            // The row is normally already flat — the handoff took it there long
            // before the worker came back — but a fast outcome can land
            // mid-fall, so the fall goes on underneath rather than snapping.
            Some(PillMode::Done { ok, since }) => {
                let total = pill::core::linger(ok).as_secs_f32();
                let t = (since.elapsed().as_secs_f32() / total).clamp(0.0, 1.0);
                let alpha = if t < 0.7 {
                    1.0
                } else {
                    ((1.0 - t) / 0.3).clamp(0.0, 1.0)
                };
                let bars = bars_for_frame(&mut self.bands, None, handoff_damping(self.handoff_since));
                let res = if ok {
                    pill.render_success(&bars, alpha)
                } else {
                    pill.render_error(&bars, alpha)
                };
                if let Err(e) = res {
                    tracing::error!(error = %e, "pill outcome render failed");
                }
            }
            // Worker still running: a neutral border breathing (~0.8 Hz) so a
            // slow round-trip reads as live, not hung. The first `HANDOFF` of
            // this mode is still the tail of the capture — the waveform keeps
            // running underneath while the ease drains it to flat, and the
            // border crossfades in over the same ramp.
            Some(PillMode::Processing { since }) => {
                let pulse =
                    0.5 - 0.5 * (since.elapsed().as_secs_f32() * std::f32::consts::TAU * 0.8).cos();
                let damping = handoff_damping(self.handoff_since);
                let bars = bars_for_frame(&mut self.bands, None, damping);
                if let Err(e) = pill.render_processing(&bars, pulse, 1.0 - damping) {
                    tracing::error!(error = %e, "pill processing render failed");
                }
            }
            // Nothing to paint. `Idle` and `Expanded` are unreachable while
            // presence is pinned to `Off`; the nub they render lands with
            // residency itself (#17, #27).
            Some(PillMode::Hidden) | Some(PillMode::Idle) | Some(PillMode::Expanded) | None => {}
            // Live capture: animate bars from the ring buffer, undamped.
            Some(PillMode::Recording { .. }) => {
                let bars = bars_for_frame(&mut self.bands, self.ring.as_ref(), 1.0);
                if let Err(e) = pill.render_recording(&bars) {
                    tracing::error!(error = %e, "pill render failed");
                }
            }
        }
    }
}

/// A flat row — every bar at its resting height. What "stopped listening" looks
/// like, and all Processing and Done ever show once the handoff has run.
fn flat_bars() -> Vec<f32> {
    vec![0.0; pill::BAR_COUNT]
}

/// How much of the waveform is left, given when the handoff started. `None` —
/// no handoff yet — is the recording case's full strength.
fn handoff_damping(since: Option<Instant>) -> f32 {
    since.map_or(1.0, |t| pill::core::handoff_damping(t.elapsed()))
}

/// This frame's bars, scaled by `damping`.
///
/// `ring` is `Some` only while capture is live. Past that the meter *holds*
/// instead: the ring was drained into the worker the moment recording stopped,
/// so ticking it would ease the bars toward the silence of an empty buffer and
/// the handoff's own fall would have nothing left to take down. Either way the
/// meter's clock keeps advancing, which is what makes the waveform continuous
/// across the mode change rather than jumping sideways into the fall.
///
/// A free function over the fields it needs rather than a method: `redraw`
/// holds a mutable borrow of the window across the whole match, and `&mut self`
/// here would collide with it.
fn bars_for_frame(
    bands: &mut audio::level::BandMeter,
    ring: Option<&audio::ring::Buffer>,
    damping: f32,
) -> Vec<f32> {
    // Past the fall there is nothing left to shape — and nothing to gain from
    // advancing a meter whose output is about to be multiplied by zero.
    if damping <= 0.0 {
        return flat_bars();
    }
    let raw = match ring {
        Some(ring) => bands.tick(ring).to_vec(),
        None => bands.hold().to_vec(),
    };
    audio::level::shape_bars(&raw)
        .into_iter()
        .map(|v| v * damping)
        .collect()
}
