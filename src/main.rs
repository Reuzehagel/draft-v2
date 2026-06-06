// Draft — Windows push-to-talk speech-to-text.
// Step-4 build: hotkey → FSM → cpal capture + pill window (static rounded
// rect, random bar heights, click-through).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activation;
mod audio;
mod autostart;
mod config;
mod hotkey;
mod logging;
mod paste;
mod paths;
mod pill;
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

/// How long the pill lingers after a capture, showing the green border
/// before it fades and disappears.
const SUCCESS_LINGER: Duration = Duration::from_millis(500);

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

    let (hotkey_handle, hotkey_rx) = hotkey::register(&cfg.hotkey)?;
    tracing::info!(hotkey = %cfg.hotkey, "hotkey registered");

    let fsm_mode = fsm_mode_from_config(&cfg);
    let fsm = activation::Fsm::new(fsm_mode);

    let transcriber: Option<Arc<dyn Transcriber>> = build_transcriber(&cfg);
    if transcriber.is_none() {
        tracing::warn!(
            "no transcriber available — set MISTRAL_API_KEY to enable paste-on-stop"
        );
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App {
        tray,
        menu_rx,
        hotkey_handle,
        hotkey_rx,
        fsm,
        capture: None,
        pill: None,
        bands: audio::level::BandMeter::new(pill::BAR_COUNT),
        transcriber,
        cfg,
        settings_child: None,
        success_anim: None,
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
    hotkey_handle: hotkey::HotkeyHandle,
    hotkey_rx: crossbeam_channel::Receiver<hotkey::HotkeyEvent>,
    fsm: activation::Fsm,
    capture: Option<audio::capture::Capture>,
    pill: Option<pill::window::PillWindow>,
    bands: audio::level::BandMeter,
    transcriber: Option<Arc<dyn Transcriber>>,
    cfg: config::Config,
    settings_child: Option<std::process::Child>,
    /// When set, the pill is playing its post-capture success animation
    /// (green border) starting at this instant, after which it's dropped.
    success_anim: Option<Instant>,
    /// Most recent waveform bars, frozen and reused during the success linger.
    last_bars: Vec<f32>,
}

fn build_transcriber(cfg: &config::Config) -> Option<Arc<dyn Transcriber>> {
    match cfg.provider {
        config::Provider::LocalParakeet => {
            if !transcribe::parakeet_download::is_present() {
                tracing::warn!(
                    "Parakeet model files missing — open Settings and click \
                     'Download model' to fetch them"
                );
                return None;
            }
            let dir = match transcribe::parakeet_download::model_dir() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(error = %e, "resolve model dir failed");
                    return None;
                }
            };
            // Lazy: the ~700MB model is pulled into RAM on first dictation
            // and released again after MODEL_IDLE_TIMEOUT of inactivity.
            let t = transcribe::parakeet::ParakeetTranscriber::new(&dir);
            Some(Arc::new(t) as Arc<dyn Transcriber>)
        }
        config::Provider::Mistral => {
            let key = secrets::load_key(config::Provider::Mistral)?;
            match transcribe::mistral::MistralTranscriber::new(key) {
                Ok(t) => Some(Arc::new(t) as Arc<dyn Transcriber>),
                Err(e) => {
                    tracing::error!(error = %e, "failed to build Mistral transcriber");
                    None
                }
            }
        }
        config::Provider::Reson8 => {
            let key = secrets::load_key(config::Provider::Reson8)?;
            match transcribe::reson8::Reson8Transcriber::new(key) {
                Ok(t) => Some(Arc::new(t) as Arc<dyn Transcriber>),
                Err(e) => {
                    tracing::error!(error = %e, "failed to build Reson8 transcriber");
                    None
                }
            }
        }
        other => {
            tracing::warn!(?other, "provider not yet implemented; no transcriber");
            None
        }
    }
}

impl App {
    fn start_session(&mut self, el: &ActiveEventLoop) {
        match audio::capture::Capture::start() {
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

        // A quick re-trigger during the success linger supersedes it.
        self.success_anim = None;

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

    /// Tear down the pill immediately (no success animation).
    fn dismiss_pill(&mut self) {
        self.success_anim = None;
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

        // We're committing to a transcription — play the success linger.
        self.success_anim = Some(Instant::now());

        let append_space = self.cfg.append_trailing_space;
        let restore_clipboard = self.cfg.restore_clipboard;
        let paste_mode = match self.cfg.paste_mode {
            config::PasteMode::Clipboard => paste::PasteMode::Clipboard,
            config::PasteMode::Unicode => paste::PasteMode::Unicode,
        };
        std::thread::spawn(move || {
            let started = Instant::now();
            let text = match transcriber.transcribe(&samples) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "transcription failed");
                    return;
                }
            };
            let elapsed_ms = started.elapsed().as_millis();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                tracing::info!(elapsed_ms, "transcription empty; nothing to paste");
                return;
            }
            let mut out = trimmed.to_owned();
            if append_space {
                out.push(' ');
            }
            tracing::info!(
                elapsed_ms,
                provider = transcriber.name(),
                chars = out.len(),
                "transcription complete"
            );
            if let Err(e) = paste::deliver_text(&out, paste_mode, restore_clipboard) {
                tracing::error!(error = %e, "paste failed");
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
            }
        }

        while let Ok(ev) = self.hotkey_rx.try_recv() {
            let in_ev = match ev {
                hotkey::HotkeyEvent::Pressed(t) => activation::InEvent::Pressed(t),
                hotkey::HotkeyEvent::Released(t) => activation::InEvent::Released(t),
            };
            match self.fsm.step(in_ev) {
                activation::OutEvent::Start => self.start_session(el),
                activation::OutEvent::Stop => self.stop_session(),
                activation::OutEvent::Ignore => {}
            }
        }

        // Free the on-device model if dictation has been idle long enough.
        // Cheap (try_lock + elapsed check); a no-op for cloud providers.
        if let Some(t) = self.transcriber.as_ref() {
            t.unload_if_idle(MODEL_IDLE_TIMEOUT);
        }

        // Retire the pill once the success animation has run its course.
        if let Some(start) = self.success_anim {
            if start.elapsed() >= SUCCESS_LINGER {
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

        if new_cfg.hotkey != self.cfg.hotkey {
            match hotkey::register(&new_cfg.hotkey) {
                Ok((handle, rx)) => {
                    self.hotkey_handle = handle;
                    self.hotkey_rx = rx;
                    tracing::info!(hotkey = %new_cfg.hotkey, "hotkey re-registered");
                }
                Err(e) => {
                    tracing::error!(error = %e, "hotkey re-register failed; keeping old binding");
                }
            }
        }

        self.fsm = activation::Fsm::new(fsm_mode_from_config(&new_cfg));
        self.transcriber = build_transcriber(&new_cfg);
        self.cfg = new_cfg;
    }

    fn redraw_pill(&mut self) {
        let Some(pill) = self.pill.as_mut() else { return };

        // Success state: soft-green border over the frozen bars, holding then
        // fading out over the final stretch (elapsed fraction of the linger).
        if let Some(start) = self.success_anim {
            let t = (start.elapsed().as_secs_f32() / SUCCESS_LINGER.as_secs_f32()).clamp(0.0, 1.0);
            // Hold fully opaque, then fade over the last 30%.
            let alpha = if t < 0.7 { 1.0 } else { ((1.0 - t) / 0.3).clamp(0.0, 1.0) };
            if let Err(e) = pill.render_success(&self.last_bars, alpha) {
                tracing::error!(error = %e, "pill success render failed");
            }
            return;
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
