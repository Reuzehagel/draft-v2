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
use crate::pill::geom::{Hover, Motion};
use crate::pill::hook::HookEvent;
use crate::pill::monitor::{Displays, Home, HomeMonitor};
use crate::session::{Command, Session, SessionKind};
use crate::transcribe::Transcriber;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

const PILL_FRAME_RATE_HZ: u64 = 30;

/// How far outside the nub a cursor counts as having reached it, in logical
/// pixels. The nub is 36x10 and deliberately small; asking the cursor to land
/// on it exactly would make the bar hard to open, and asking only that the
/// cursor be on the *window* would open it from across the envelope.
const HOVER_REACH: f32 = 8.0;

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

    let tray = tray::build(&tray_status(&cfg, None, !history::is_empty()))?;
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

    let (monitor_policy, pinned_path) = monitor_policy_from_config(&cfg);

    let mut app = App {
        tray,
        menu_rx,
        hotkey_handle: Some(hotkey_handle),
        hotkey_rx,
        session,
        pill_core: Pill::new(),
        pill: PillAdapter::new(hook_tx),
        fullscreen: pill::fullscreen::Watcher::new(),
        // Installed in `resumed`, if residency wants it: the hook has to be
        // registered on the thread with the message loop, and that thread is
        // only running from there on.
        foreground_hook: None,
        hovered: false,
        home: Home::new(monitor_policy, pinned_path),
        // Enumerated for real in `resumed`, on the same path a display change
        // takes. Nothing can be placed before there is an event loop anyway.
        displays: Displays::default(),
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
fn tray_status(
    cfg: &config::Config,
    update: Option<&update::UpdateInfo>,
    has_history: bool,
) -> tray::Status {
    tray::Status {
        hotkey: cfg.hotkey.clone(),
        provider: cfg.provider,
        update: update.map(|u| u.latest_version.clone()),
        has_history,
    }
}

/// The home-monitor policy and its pinned path, per config.
///
/// Read at launch and on every config reload — and by the *session-only* pill
/// too: one policy governs both, because two would mean two code paths and a
/// mode-dependent surprise about where the pill turns up.
fn monitor_policy_from_config(cfg: &config::Config) -> (pill::monitor::Policy, Option<String>) {
    let policy = match cfg.pill.monitor {
        config::MonitorPolicy::Focused => pill::monitor::Policy::Focused,
        config::MonitorPolicy::Cursor => pill::monitor::Policy::Cursor,
        config::MonitorPolicy::Primary => pill::monitor::Policy::Primary,
        config::MonitorPolicy::Pinned => pill::monitor::Policy::Pinned,
    };
    (policy, cfg.pill.monitor_pinned_path.clone())
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
    /// residency toggle, the hover poll and the fullscreen watcher are the rest.
    pill_core: Pill,
    pill: PillAdapter,
    /// Whether a fullscreen app is on the pill's home monitor. Pure: it is fed
    /// a probe and says when the answer moved.
    fullscreen: pill::fullscreen::Watcher,
    /// The `EVENT_SYSTEM_FOREGROUND` hook, alive exactly while the pill is
    /// resident — with residency off there is no hook, no probe, and no idle
    /// work at all. `None` also when installing it failed, which costs reaction
    /// speed and nothing else.
    foreground_hook: Option<pill::fullscreen::ForegroundHook>,
    /// Whether the cursor is on the pill, as [`App::poll_hover`] last saw it.
    /// Held here rather than pushed straight at the core because presence is
    /// decided by three facts at once — see [`App::presence`].
    hovered: bool,
    /// Which monitor the pill lives on, and the policy deriving it. Pure: it is
    /// fed the display snapshot below plus the cheap per-poll signals, and says
    /// when the answer moved.
    home: Home,
    /// The connected monitors as of the last enumeration. Refreshed only when
    /// the display topology or DPI changes — enumerating per poll would be
    /// `EnumDisplayMonitors` plus `QueryDisplayConfig` at 20 Hz, which is
    /// exactly the idle cost residency exists to avoid.
    displays: Displays,
    /// The messages winit doesn't surface, posted by the pill window's wndproc
    /// subclass. The home monitor consumes the display ones; the wakeup ladder
    /// (#49) is the remaining consumer.
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
                    let more =
                        match audio::capture::Capture::start(self.cfg.input_device.as_deref()) {
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
        let now = Instant::now();
        for cmd in cmds {
            match cmd {
                // Derived immediately before the window exists, so the pill
                // lands where the policy says even when nothing has been
                // polling — which is every session-only pill, since the poll
                // is skipped while there is no window to move.
                //
                // Off a fresh enumeration, not the cached desk: the display
                // snapshot is kept current by the pill window's own hook, and
                // between sessions there is no window and therefore no hook. A
                // monitor plugged in since the last dictation would otherwise
                // be invisible. `Create` happens once per session at most,
                // beside opening a microphone — this is not the expensive part.
                pill::core::Command::Create => {
                    self.rederive_home();
                    self.pill.create(el);
                }
                pill::core::Command::SetMode(mode) => self.pill.set_mode(mode, now),
                pill::core::Command::Show => self.pill.show(now),
                pill::core::Command::Hide => self.pill.hide(now),
                pill::core::Command::Destroy => self.pill.destroy(now),
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

    /// Push the current state to both surfaces that report it: the tray's
    /// tooltip and menu, and the pill's Copy button.
    ///
    /// The two share a fact — whether there is a transcript to recover — so
    /// they are refreshed together and off one read of it. "Copy last
    /// transcription" and the bar's Copy button are the same recovery path;
    /// one of them live while the other is not would be a lie about the same
    /// history.
    fn refresh_status(&mut self) {
        let has_history = !history::is_empty();
        self.tray.apply(&tray_status(
            &self.cfg,
            self.update_available.as_ref(),
            has_history,
        ));
        self.pill_core.set_has_history(has_history);
        let enabled = std::array::from_fn(|i| self.pill_core.enabled(i));
        self.pill.set_enabled(enabled);
    }

    /// Perform a button press. The core decided *which* button — including
    /// whether it was live at all — so there is nothing to check here.
    fn run_button(&mut self, action: pill::core::Action) {
        match action {
            pill::core::Action::Copy => self.copy_last_transcription(),
            // The settings window takes focus; the pill still does not. It is a
            // subprocess, so nothing about this window changes.
            pill::core::Action::Settings => self.open_settings(),
            pill::core::Action::Dictate => {
                tracing::info!("pill: dictate button clicked (wired up in #30)")
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
    /// First point at which a window can be created, so this is where residency
    /// takes effect: the nub is on screen from launch, not from the first
    /// dictation.
    fn resumed(&mut self, el: &ActiveEventLoop) {
        // Before the presence, not after: `apply_presence` is what creates the
        // window, and a window has to be created *somewhere*.
        self.rederive_home();
        // This is also the first moment the foreground hook can be registered:
        // the docs require the registering thread to have a message loop.
        self.apply_residency(el);
        // Whether there is anything to copy is read here rather than at build
        // time: the tray was built before the event loop, and the bar's Copy
        // button needs the same answer.
        self.refresh_status();
    }

    /// Deliberately empty of *painting*. The pill drives its own frames from
    /// `about_to_wait`, and *only* when it has one to draw — a
    /// `RedrawRequested → redraw()` path would put the resident nub in a
    /// `WM_PAINT` loop, re-pushing an unchanged surface forever. A layered
    /// window's pixels are maintained by the system; there is nothing to repaint.
    ///
    /// What does arrive here is the mouse: while the bar is up,
    /// `WS_EX_TRANSPARENT` is off and the pill is a real mouse target, so
    /// per-button hover is `CursorMoved` rather than another poll (#20).
    /// Everything else winit offers is ignored.
    fn window_event(&mut self, _el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.pill.window_id() != Some(id) {
            return;
        }
        let now = Instant::now();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let x = self.pill.offset_in_window(position.x as f32);
                let hovered = x.and_then(|x| self.pill_core.button_at(x));
                self.pill.set_cursor(x, hovered, now);
            }
            // The collapse itself is the hover poll's call — this only puts the
            // indicator out, so a cursor leaving by the corner (which stays
            // click-through) doesn't leave a button lit.
            WindowEvent::CursorLeft { .. } => self.pill.set_cursor(None, None, now),
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if let Some(action) = self.pill.cursor_x.and_then(|x| self.pill_core.action_at(x)) {
                    self.run_button(action);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.poll_settings_child(el);
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
            self.refresh_status();
        }

        // The messages winit doesn't surface, arriving from the pill window's
        // wndproc subclass. What they all mean here is the same thing: the
        // layered surface the system has been maintaining for us may not have
        // survived, so push it again.
        //
        // This is the *only* thing that re-pushes an idle nub. Notably absent:
        // WM_DWMCOMPOSITIONCHANGED, which fires often and means nothing for a
        // per-pixel-alpha layered window — following it would put the pill back
        // in a repaint loop by another name.
        //
        // Draining is not optional either way; an unread channel would grow for
        // the life of the process. The home monitor (#43) hangs further
        // behaviour off these, and the wakeup ladder (#49) will in turn.
        while let Ok(ev) = self.hook_rx.try_recv() {
            let repush = match ev {
                // The one event that overrides the home monitor's idle-only
                // derivation, latch and all: unplug, resolution change, lid
                // close and RDP reconnect all arrive here, and the alternative
                // is a pill positioned into a coordinate space that no longer
                // exists. Re-placing re-renders, so nothing more to re-push.
                HookEvent::DisplayChanged => {
                    tracing::info!("display topology changed");
                    self.rederive_home();
                    true
                }
                // The home monitor's scale is what the pill renders at, so a
                // DPI change re-places it: the work area moved with it, and the
                // surface is now the wrong resolution. A *refresh*, not a
                // re-derivation — the latch break belongs to WM_DISPLAYCHANGE
                // alone, and a scaling slider is not a reason to change which
                // monitor the pill lives on.
                HookEvent::DpiChanged { dpi } => {
                    tracing::info!(dpi, "pill monitor dpi changed");
                    self.refresh_home();
                    true
                }
                // Defensive: the compositor is torn down and rebuilt around a
                // lock, an RDP reconnect and a display wake, and a layered
                // surface does not reliably survive that. Coming back is cheap;
                // coming back to an invisible pill is not recoverable without
                // a dictation.
                HookEvent::DisplayPower { on } => {
                    tracing::info!(on, "session display power");
                    on
                }
                HookEvent::SessionLock { locked } => {
                    tracing::info!(locked, "session lock");
                    !locked
                }
                HookEvent::SessionReconnected => {
                    tracing::info!("session reattached to a terminal");
                    true
                }
            };
            if repush {
                self.pill.repush();
            }
        }

        // At most one message, only when a newer release exists.
        if let Ok(info) = self.update_rx.try_recv() {
            self.update_available = Some(info);
            self.refresh_status();
        }

        // Free the on-device model if dictation has been idle long enough.
        // Cheap (try_lock + elapsed check); a no-op for cloud providers.
        if let Some(t) = self.transcriber.as_ref() {
            t.unload_if_idle(MODEL_IDLE_TIMEOUT);
        }

        // Retire the pill once its terminal flash has run its course.
        let now = Instant::now();
        let cmds = self.pill_core.tick(now);
        self.run_pill_commands(cmds, el);

        // Is a fullscreen app in the way? Before the hover poll, which skips
        // its cursor read entirely while the answer is yes.
        self.poll_fullscreen(el, now);

        // Is the cursor over the pill? Before the home poll, which latches
        // while the pill is expanded — so the answer this produces is the one
        // that latch reads.
        self.poll_hover(el);

        // Where the pill should be living, on the loop's own cadence. Latched
        // unless the pill is idle, and free for the policies with no per-poll
        // signal to read.
        self.poll_home(now);

        // Frames are pushed only when there is one to push. A settled nub wants
        // none at all — that is what makes residency free: one
        // `UpdateLayeredWindow` when it arrives, and then the system owns the
        // surface until something actually happens.
        if self.pill.wants_frame(now) {
            self.pill.redraw(now);
            el.set_control_flow(ControlFlow::WaitUntil(
                now + Duration::from_millis(1000 / PILL_FRAME_RATE_HZ),
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

    /// The pill's presence, from the three facts that decide it. **One
    /// function, one writer**: three drivers each pushing their own fact would
    /// be three chances to overwrite the other two — the hover poll runs every
    /// loop, and saying `Resident` there would take the pill straight back out
    /// over the fullscreen app the watcher just hid it from.
    ///
    /// The order is the priority. Residency off means the pill does not exist
    /// to suppress; suppression outranks hover, which is why the hover poll can
    /// skip its cursor read entirely while a game is up.
    fn presence(&self) -> pill::core::Presence {
        if !self.cfg.pill.resident {
            pill::core::Presence::Off
        } else if self.fullscreen.suppressed() {
            pill::core::Presence::Suppressed
        } else {
            pill::core::Presence::Resident {
                expanded: self.hovered,
            }
        }
    }

    /// Hand the current presence to the Pill core and perform whatever it
    /// decides that means. Note what this does *not* do: create or destroy a
    /// window. Presence is a fact about what the user asked for; whether a
    /// window exists is the core's conclusion from presence *and* activity, and
    /// mid-session those disagree — which is exactly the case that must not
    /// snatch the pill away.
    fn apply_presence(&mut self, el: &ActiveEventLoop) {
        let cmds = self.pill_core.set_presence(self.presence());
        self.run_pill_commands(cmds, el);
    }

    /// Put the pill where residency now says it should be: match the watcher to
    /// the toggle, look once, and apply the presence that falls out.
    ///
    /// The order is the whole of it, which is why it is one function rather
    /// than three calls at each of the two sites that need them — launch and a
    /// config reload. Probing *before* the presence is what stops a Draft that
    /// starts (or has residency switched back on) while a game is up from
    /// putting a nub over it for a loop.
    fn apply_residency(&mut self, el: &ActiveEventLoop) {
        self.apply_watcher();
        self.poll_fullscreen(el, Instant::now());
        self.apply_presence(el);
    }

    /// Install or drop the foreground hook to match residency, and make the
    /// watcher look again either way.
    ///
    /// With residency off the watcher does **no work at all**: no hook, so no
    /// callbacks; and [`Self::poll_fullscreen`] returns before it can probe. So
    /// whatever it last decided is stale by the time residency comes back —
    /// hence the rearm on both edges.
    fn apply_watcher(&mut self) {
        if self.cfg.pill.resident == self.foreground_hook.is_some() {
            return;
        }
        self.foreground_hook = self
            .cfg
            .pill
            .resident
            .then(pill::fullscreen::ForegroundHook::install)
            .flatten();
        self.fullscreen.rearm();
    }

    /// Is a fullscreen app on the pill's monitor? The watcher decides whether
    /// this loop is one that should look at all; all this does is hand it the
    /// home monitor and the means to take a probe.
    ///
    /// Skipped outright with residency off — see [`Self::apply_watcher`].
    /// Deliberately *not* skipped during a session: the pill is visible either
    /// way, because activity outranks presence, and the fullscreen state has to
    /// be current for the moment the flash retires.
    fn poll_fullscreen(&mut self, el: &ActiveEventLoop, now: Instant) {
        if !self.cfg.pill.resident {
            return;
        }
        let home = self.home.current().map(|h| h.id);
        let changed = pill::fullscreen::foreground_changed();
        if let Some(suppressed) = self
            .fullscreen
            .poll(now, changed, home, pill::fullscreen::probe)
        {
            tracing::debug!(suppressed, "fullscreen app on the pill's monitor");
            self.apply_presence(el);
        }
    }

    /// Hand the home-monitor core this moment's signals and move the pill if it
    /// says the answer changed. The one path every derivation takes; what
    /// differs between them is only the trigger.
    fn derive_home(&mut self, trigger: pill::monitor::Trigger, now: Instant) {
        let signals = pill::monitor::sample(self.home.policy());
        if let Some(home) = self.home.update(trigger, &self.displays, signals, now) {
            tracing::debug!(monitor = home.id, dpi = home.dpi, "pill home monitor");
            self.pill.set_home(home);
        }
    }

    /// Re-derive unconditionally, breaking the latch, off a freshly enumerated
    /// desk. Startup, `WM_DISPLAYCHANGE`, a DPI change and a policy change all
    /// come through here — one path, so a pill that comes back from an unplug
    /// cannot behave differently from one that just launched.
    ///
    /// The enumeration is what makes this the expensive one, and why the
    /// per-loop poll doesn't do it.
    fn rederive_home(&mut self) {
        self.displays = pill::monitor::enumerate();
        self.derive_home(pill::monitor::Trigger::Rederive, Instant::now());
    }

    /// Re-read the home monitor's own metrics after a DPI change, leaving the
    /// policy — and therefore which monitor the pill is on — alone.
    fn refresh_home(&mut self) {
        self.displays = pill::monitor::enumerate();
        if let Some(home) = self.home.refresh(&self.displays) {
            tracing::debug!(
                monitor = home.id,
                dpi = home.dpi,
                "pill home monitor rescaled"
            );
            self.pill.set_home(home);
        }
    }

    /// Expand the pill when the cursor arrives over it, and collapse it when it
    /// leaves.
    ///
    /// **Polling, not a mouse target.** The pill is click-through whenever it
    /// is not showing buttons, so it receives no mouse messages to enter *by* —
    /// one `GetCursorPos` against the window's own rect is the whole mechanism.
    /// It keeps polling once expanded: winit's `CursorLeft` is what puts the
    /// indicator out, but the cursor can leave without one (a teleport, another
    /// window taking the pointer), and a pill left expanded under no cursor
    /// would be a pill that swallows clicks forever.
    ///
    /// Skipped with no window, which is what keeps residency-off free of idle
    /// work. Hover cannot expand a recording pill — the Pill core enforces
    /// that, so this hands over the cursor's answer unconditionally and lets it
    /// decide.
    fn poll_hover(&mut self, el: &ActiveEventLoop) {
        // Expansion only rides along on presence — it is meaningless when the
        // pill is off or suppressed, and there is nothing on screen to be over
        // in either case. So the cursor is not even read: this is the one loop
        // whose cost residency-off and a fullscreen game both have to escape.
        if !self.pill.has_window() || !self.cfg.pill.resident || self.fullscreen.suppressed() {
            self.hovered = false;
            return;
        }
        let Some(cursor) = pill::monitor::cursor_pos() else {
            return;
        };
        self.hovered = self
            .pill
            .cursor_over(cursor, self.pill_core.showing_buttons());
        self.apply_presence(el);
    }

    /// The ordinary per-loop sample. The core decides whether the latch applies
    /// and whether the cursor has dwelt long enough; all this does is hand it
    /// what only the adapter can know.
    ///
    /// Skipped entirely with no window on screen. With residency off and no
    /// session running there is nothing to move, and residency off means *no
    /// idle work at all* — a `GetForegroundWindow` every 50 ms is exactly the
    /// cost that promise rules out. The pill that a session then creates is
    /// still placed by the policy: [`Self::run_pill_commands`] derives ahead of
    /// every `Create`.
    fn poll_home(&mut self, now: Instant) {
        if !self.pill.has_window() {
            return;
        }
        let trigger = pill::monitor::Trigger::Poll {
            idle: self.pill.is_idle(now),
        };
        self.derive_home(trigger, now);
    }

    fn poll_settings_child(&mut self, el: &ActiveEventLoop) {
        let Some(child) = self.settings_child.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                tracing::info!(?status, "settings subprocess exited; reloading config");
                self.settings_child = None;
                self.reload_config(el);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "settings child try_wait failed");
                self.settings_child = None;
            }
        }
    }

    fn reload_config(&mut self, el: &ActiveEventLoop) {
        let new_cfg = match config::Config::load() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "config reload failed");
                // The settings session may still have cleared the history even
                // though its config didn't load — don't leave the menu stale.
                self.refresh_status();
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
            let command_spec = new_cfg
                .push_to_command
                .then(|| new_cfg.command_hotkey.clone());
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
        // A new monitor policy takes effect on the next ordinary poll, latch
        // and all: settings closing is not one of the events that may move the
        // pill mid-sentence. In practice that is immediate, because the pill is
        // idle while the user is in the settings window — and with no window at
        // all the `Create` path derives instead.
        let (policy, pinned_path) = monitor_policy_from_config(&self.cfg);
        self.home.configure(policy, pinned_path);
        self.displays = pill::monitor::enumerate();
        self.poll_home(Instant::now());
        // Residency rides beside the activation reset: hand the new value to
        // the Pill core and let it decide. Toggled off mid-session it changes
        // nothing on screen until the flash retires, because activity outranks
        // presence — the pill is never snatched away mid-dictation.
        self.apply_residency(el);
        // The tooltip names the hotkey and provider, and settings can clear
        // the history — so the tray follows a settings change without a restart.
        self.refresh_status();
    }
}

/// The pill window and its animation, driven by the [`PillMode`] the Pill core
/// derives. The core decides *what* mode, *when* to transition, and whether a
/// window exists at all; the adapter derives every frame's geometry from the
/// motion model, and its bars from the ring buffer. It holds no lifecycle rules.
///
/// The whole of its animation state is one [`Motion`] — where the pill was,
/// where it is going, and when it set off. There is no per-mode animation code
/// left here: a mode change starts a tween, and every frame is `motion.at(now)`.
struct PillAdapter {
    window: Option<pill::window::PillWindow>,
    /// Handed to each window it creates, so the wndproc hook can post to the
    /// app loop.
    hook_tx: crossbeam_channel::Sender<HookEvent>,
    /// Where the pill lives. Held even with no window, so the next `Create`
    /// lands on the right monitor without having to re-derive first. `None`
    /// only before the first derivation — and on a machine with no monitors at
    /// all, where there is nowhere to put a window anyway.
    home: Option<HomeMonitor>,
    bands: audio::level::BandMeter,
    /// The current logical mode; `None` when there is no window.
    mode: Option<PillMode>,
    /// The transition in flight — or a settled Geom, once it has finished.
    motion: Motion,
    /// A frame is owed that the animation state alone would not ask for: the
    /// one that lands a finished transition. Cleared by [`Self::redraw`].
    ///
    /// This is what makes an idle nub cost nothing: with no motion running and
    /// no frame owed, the adapter asks for none at all and the system keeps the
    /// layered surface alive by itself.
    frame_owed: bool,
    /// A `Hide` or `Destroy` the core has issued that the pill is still
    /// animating its way to. Both arrive in the same command list as the mode
    /// change that concealing *is*, so performing them on arrival would cut
    /// that conceal off at its first frame.
    ///
    /// This defers *when* a teardown happens; it never decides *whether* one
    /// does. See [`Self::supersede_teardown`] for the one case where a deferred
    /// teardown is dropped — which is also the core's call, not the adapter's.
    pending: Option<Teardown>,
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
    /// Which button the cursor is on, and the fade between it and the last.
    /// Deliberately outside the [`Motion`]: per-button hover is per-button
    /// state that one `Geom` cannot carry (#29).
    hover: Hover,
    /// Whether each button is live, as the Pill core last derived it. Cached
    /// here because it is a per-frame drawing input and the core is not asked
    /// per frame; [`App::refresh_status`] is what keeps the two in step.
    enabled: [bool; pill::core::BUTTON_COUNT],
    /// The cursor's last known offset from the pill's centre, in logical
    /// pixels. `MouseInput` carries no position, so this is what a click is
    /// tested against.
    cursor_x: Option<f32>,
}

/// What to do with the window once the motion taking it off screen has run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Teardown {
    /// Off screen, window kept — the resident case, where the next reveal must
    /// not have to rebuild a layered window.
    Hide,
    /// Off screen and gone. Only reached when the pill has no reason to exist
    /// at all: residency off, and no session running.
    Destroy,
}

impl PillAdapter {
    fn new(hook_tx: crossbeam_channel::Sender<HookEvent>) -> Self {
        Self {
            window: None,
            hook_tx,
            home: None,
            bands: audio::level::BandMeter::new(pill::BAR_COUNT),
            mode: None,
            motion: Motion::settled(PillMode::Hidden, Instant::now()),
            frame_owed: false,
            pending: None,
            handoff_since: None,
            ring: None,
            hover: Hover::new(Instant::now()),
            enabled: [true; pill::core::BUTTON_COUNT],
            cursor_x: None,
        }
    }

    /// Adopt the core's view of which buttons are live.
    fn set_enabled(&mut self, enabled: [bool; pill::core::BUTTON_COUNT]) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.frame_owed = true;
        }
    }

    /// Where the cursor is, as a logical offset from the pill's centre, and
    /// which button that lights. A cursor wandering inside one slab moves
    /// nothing, so it costs no frame.
    fn set_cursor(&mut self, offset: Option<f32>, hovered: Option<usize>, now: Instant) {
        self.cursor_x = offset;
        if self.hover.set(hovered, now) {
            self.frame_owed = true;
        }
    }

    fn window_id(&self) -> Option<WindowId> {
        self.window.as_ref().map(|pw| pw.id())
    }

    /// Where the pill is, in physical virtual-screen pixels — `None` with no
    /// window.
    fn rect(&self) -> Option<pill::monitor::Rect> {
        self.window.as_ref().map(|pw| pw.rect())
    }

    /// Whether the cursor is inside the region that keeps the pill expanded.
    ///
    /// A direct comparison of physical virtual-screen pixels: `GetCursorPos`
    /// and the window's placement are both in that space, and scaling either by
    /// [`pill::window::PillWindow::scale`] would put the test in a space
    /// neither of them is in.
    ///
    /// **The region is not the same coming and going.** Opening asks the cursor
    /// to be near the *nub* — the window is the envelope, and a 36x10 nub that
    /// sprang open from 60px away would be a pill that expands at anything
    /// passing along the bottom of the screen. Staying open asks only that the
    /// cursor be on the window, which is where the buttons now are. The overlap
    /// between the two is the hysteresis: nothing can sit on a boundary and
    /// flicker.
    ///
    /// What this does not decide is clicks: a layered window hit-tests on
    /// per-pixel alpha, so the corners stay click-through however it answers.
    fn cursor_over(&self, cursor: (i32, i32), expanded: bool) -> bool {
        let Some(r) = self.rect() else {
            return false;
        };
        let inside = |r: pill::monitor::Rect| {
            cursor.0 >= r.left && cursor.0 < r.right && cursor.1 >= r.top && cursor.1 < r.bottom
        };
        if expanded {
            return inside(r);
        }
        inside(self.nub_reach(r))
    }

    /// The nub, grown by [`HOVER_REACH`] on every side — the region a hover has
    /// to reach to open the bar. In physical pixels, off the window's own rect,
    /// so it lands on the nub at any DPI.
    fn nub_reach(&self, r: pill::monitor::Rect) -> pill::monitor::Rect {
        let scale = self.window.as_ref().map_or(1.0, |pw| pw.scale());
        let half_w = ((pill::geom::NUB_W / 2.0 + HOVER_REACH) * scale).round() as i32;
        let half_h = ((pill::geom::NUB_H / 2.0 + HOVER_REACH) * scale).round() as i32;
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        pill::monitor::Rect {
            left: cx - half_w,
            top: cy - half_h,
            right: cx + half_w,
            bottom: cy + half_h,
        }
    }

    /// A `CursorMoved` position — physical pixels from the window's top-left —
    /// as the logical offset from the pill's centre the button slabs are stated
    /// in. This is the *one* place the scale divides: the slabs are logical,
    /// every pixel Windows reports is not.
    fn offset_in_window(&self, physical_x: f32) -> Option<f32> {
        let (r, pw) = (self.rect()?, self.window.as_ref()?);
        Some((physical_x - r.width() as f32 / 2.0) / pw.scale())
    }

    fn set_ring(&mut self, ring: audio::ring::Buffer) {
        self.ring = Some(ring);
    }

    /// Adopt a home monitor the core just derived, moving the window there if
    /// there is one. With no window this only records it — the next `Create`
    /// reads it.
    fn set_home(&mut self, home: HomeMonitor) {
        self.home = Some(home);
        if let Some(pw) = self.window.as_mut() {
            if let Err(e) = pw.set_home(home) {
                tracing::error!(error = %e, "could not move the pill to its home monitor");
            }
        }
    }

    fn has_window(&self) -> bool {
        self.window.is_some()
    }

    /// Whether the pill is doing nothing — the only stretch in which it may
    /// move. Latched at session start and while expanded, so it cannot skate to
    /// another monitor mid-sentence, nor slide out from under the hand about to
    /// click it.
    ///
    /// A transition still running counts as busy even when the mode it is
    /// heading for is `Idle`. The mode flips at the *start* of the morph, and
    /// the move is a hard cut — one without the other would tear the conceal or
    /// the reveal in half.
    fn is_idle(&self, now: Instant) -> bool {
        matches!(
            self.mode,
            None | Some(PillMode::Hidden) | Some(PillMode::Idle)
        ) && !self.motion.is_running(now)
    }

    /// Whether the pill has a frame to draw right now. False for a settled nub,
    /// which is the point: residency costs one `UpdateLayeredWindow` and then
    /// nothing until something happens.
    fn wants_frame(&self, now: Instant) -> bool {
        if self.window.is_none() {
            return false;
        }
        self.frame_owed
            || self.motion.is_running(now)
            || self.hover.is_running(now)
            || self.mode_self_animates()
    }

    /// The modes that produce new pixels without a transition running: live
    /// bars, the working breath, and the tail of a handoff still falling.
    fn mode_self_animates(&self) -> bool {
        match self.mode {
            Some(PillMode::Recording { .. }) | Some(PillMode::Processing { .. }) => true,
            // The flash itself is a still image. The only thing moving under it
            // is a handoff that outlived Processing.
            Some(PillMode::Done { .. }) => handoff_damping(self.handoff_since) > 0.0,
            _ => false,
        }
    }

    /// Drop a deferred teardown, because the core has since said it wants the
    /// pill again.
    ///
    /// Not a lifecycle decision of the adapter's own: the core issues its
    /// commands in order, and a later `Create` or `Show` supersedes an earlier
    /// `Hide`/`Destroy` that has not been performed yet. All the adapter is
    /// doing is refusing to perform a command the core has already overruled —
    /// which is exactly what deferring it made possible.
    fn supersede_teardown(&mut self) {
        self.pending = None;
    }

    /// Build the window, off screen. A failure leaves us without one; every
    /// later command is a no-op until the core asks for another.
    fn create(&mut self, el: &ActiveEventLoop) {
        // A window still here means a `Destroy` is deferred behind a conceal.
        // The core has now asked for a window and there is one — reuse it
        // rather than tearing a layered window down to build the same thing
        // back a frame later.
        if self.window.is_some() {
            self.supersede_teardown();
            return;
        }
        let Some(home) = self.home else {
            tracing::error!("no home monitor to put the pill on");
            return;
        };
        match pill::window::PillWindow::create(el, self.hook_tx.clone(), home) {
            Ok(pw) => self.window = Some(pw),
            Err(e) => {
                tracing::error!(error = %e, "failed to create pill window");
                self.window = None;
            }
        }
    }

    /// Apply a mode the core derived: start the transition into it from
    /// whatever the pill currently *looks* like.
    ///
    /// Starting from the drawn geometry rather than from the previous mode's is
    /// what makes an interrupted transition continue rather than jump — a chord
    /// pressed halfway through a reveal grows from the half-revealed nub.
    ///
    /// Note what is *not* reset on the way out of `Recording`: the meter keeps
    /// its clock, so the waveform running under the handoff is the same one
    /// that was running a frame earlier, with no sideways jump at the change.
    fn set_mode(&mut self, mode: PillMode, now: Instant) {
        let entering_recording = matches!(mode, PillMode::Recording { .. })
            && !matches!(self.mode, Some(PillMode::Recording { .. }));
        // Stamp the handoff on the way *into* Processing only, so a `Done` that
        // follows keeps counting from the mode change rather than restarting.
        if let PillMode::Processing { since } = mode {
            if !matches!(self.mode, Some(PillMode::Processing { .. })) {
                self.handoff_since = Some(since);
            }
        }
        if entering_recording {
            self.handoff_since = None;
            self.bands.reset();
        }
        // Click-through is off exactly while the bar is up, and the flip
        // happens with the mode rather than on a timer: the window becomes a
        // mouse target at the instant it has something to click.
        //
        // Note it goes *back on* at the start of the collapse, not the end. The
        // pill is on its way out from under the cursor either way, and a window
        // that swallowed clicks through a 90 ms fade would be swallowing them
        // for the app underneath.
        let buttons = mode.shows_buttons();
        if buttons != self.mode.is_some_and(PillMode::shows_buttons) {
            if let Some(pw) = self.window.as_ref() {
                pw.set_click_through(!buttons);
            }
            if !buttons {
                // Nothing to hover once the bar is gone, and a hover left
                // standing would light a button on the next reveal.
                self.set_cursor(None, None, now);
            }
        }
        let tween = pill::geom::transition(self.mode.unwrap_or(PillMode::Hidden), mode);
        self.motion = Motion::start(self.motion.at(now), mode, tween, now);
        self.mode = Some(mode);
        self.frame_owed = true;
        // Paint the first frame before the `Show` that follows reveals it, so
        // what appears is already the pill and never a blank rectangle.
        self.redraw(now);
    }

    fn show(&mut self, now: Instant) {
        self.supersede_teardown();
        // Paint before revealing, so what appears is already the pill and never
        // a blank rectangle. Normally a no-op: the `SetMode` that precedes
        // every `Show` has drawn that frame already, and re-pushing an
        // identical surface is the per-frame cost residency exists to avoid.
        if !self.window.as_ref().is_some_and(|pw| pw.has_frame()) {
            self.redraw(now);
        }
        if let Some(pw) = self.window.as_ref() {
            pw.show();
        }
    }

    /// Take the pill off screen — once the motion doing so has finished. The
    /// core issues `Hide` with the mode change that *is* the conceal, so hiding
    /// the window here and now would cut that animation off at its first frame.
    fn hide(&mut self, now: Instant) {
        self.pending = Some(Teardown::Hide);
        self.flush_pending(now);
    }

    /// Tear the window down, on the same terms as [`Self::hide`]: the conceal
    /// runs first, then the window goes.
    fn destroy(&mut self, now: Instant) {
        self.pending = Some(Teardown::Destroy);
        self.flush_pending(now);
    }

    /// Perform a deferred hide/destroy if the motion that had to run first is
    /// over. Called after every frame, so the teardown lands on the frame after
    /// the last one the conceal drew.
    fn flush_pending(&mut self, now: Instant) {
        let Some(teardown) = self.pending else {
            return;
        };
        if self.motion.is_running(now) {
            return;
        }
        self.pending = None;
        if let Some(pw) = self.window.as_ref() {
            pw.hide();
        }
        if teardown == Teardown::Destroy {
            // The ring goes with the window — it belongs to a capture that is
            // long over by the time the pill has no reason to exist.
            self.mode = None;
            self.ring = None;
            self.handoff_since = None;
            self.frame_owed = false;
            drop(self.window.take());
        }
    }

    /// Re-push the surface the pill is already showing. The system maintains a
    /// layered window's pixels on its own, so this is only for the events that
    /// can invalidate them out from under us — see [`PillWindow::repush`].
    fn repush(&mut self) {
        let Some(pw) = self.window.as_mut() else {
            return;
        };
        if let Err(e) = pw.repush() {
            tracing::error!(error = %e, "pill surface re-push failed");
        }
    }

    /// Draw one frame: the motion's geometry at `now`, with this frame's bars.
    fn redraw(&mut self, now: Instant) {
        let Some(pill) = self.window.as_mut() else {
            return;
        };
        let mut geom = self.motion.at(now);
        // The breath rides on top of the morph rather than being part of it:
        // it is a sustained oscillation with no end state, so it cannot be a
        // lerp between two Geoms.
        if let Some(PillMode::Processing { since }) = self.mode {
            geom = pill::geom::breathe(geom, now.saturating_duration_since(since));
        }
        // The bars' *heights* are not part of the Geom — the Geom carries the
        // row's opacity, and the waveform is live data. The handoff drains it
        // over the same 320 ms the border is crossfading across.
        let ring = matches!(self.mode, Some(PillMode::Recording { .. }))
            .then_some(self.ring.as_ref())
            .flatten();
        let bars = if geom.bars > 0.0 {
            bars_for_frame(&mut self.bands, ring, handoff_damping(self.handoff_since))
        } else {
            Vec::new()
        };
        // Per-button hover, likewise: the Geom carries the bar's *growth*, and
        // which button is lit is state beside it.
        let slots = self.hover.slots(now, |i| self.enabled[i]);
        if let Err(e) = pill.render(&geom, &bars, &slots) {
            tracing::error!(error = %e, "pill render failed");
        }
        // One more frame is owed while a transition is still running, so the
        // frame that lands it is drawn even if the loop wakes up past its end.
        // The hover fade is a second, smaller one with the same need.
        self.frame_owed = self.motion.is_running(now) || self.hover.is_running(now);
        self.flush_pending(now);
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
