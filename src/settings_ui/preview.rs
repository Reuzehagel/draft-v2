// `draft.exe --settings-preview <dir>`: open the settings window over the
// real config, show each pane in rail order, and save each as
// `<dir>/<nn>-<pane>.png`, then close. The pixels are the window's own — the
// same renderer, fonts and scale the user sees — read back with egui's
// screenshot command, so a review of the PNGs is a review of the window.
//
// Rows that only appear in some states (a pinned display, a missing key, a
// typed model id) are shot too, as `<nn>-<pane>-<state>.png`: each state is
// set through the form's own methods, and the form is reloaded over the real
// config afterwards. Nothing is ever saved, and the close isn't held by the
// unsaved prompt.

use super::state::{Form, ModelPick};
use super::{install_style, native_options, SettingsApp, Tab};
use crate::config::{ChatBackend, Config, MonitorPolicy};
use crate::secrets::KeySlot;
use std::path::PathBuf;

/// How long a pane is shown before it is captured: long enough for egui's
/// animations (switch knobs, fades) to finish, which run ~0.1 s.
const SETTLE_SECS: f64 = 0.4;

/// One screenshot: a pane, and the state it is shown in.
#[derive(Clone, Copy)]
struct Shot {
    tab: Tab,
    /// Appended to the file name; empty for the pane as configured.
    state: &'static str,
    set: fn(&mut Form),
}

/// The states worth a look beyond the pane as configured: each shows rows
/// the user's own config may never reach.
const STATES: &[Shot] = &[
    Shot {
        tab: Tab::Pill,
        state: "pinned",
        set: |form| form.cfg.pill.monitor = MonitorPolicy::Pinned,
    },
    Shot {
        tab: Tab::Commands,
        state: "no-key",
        set: |form| {
            form.cfg.push_to_command = true;
            form.keys.set(KeySlot::Cerebras, String::new());
            form.cfg.chat_backend = Some(ChatBackend::Cerebras);
            form.cfg.chat_model = None;
        },
    },
    Shot {
        tab: Tab::Commands,
        state: "other",
        set: |form| {
            form.keys.set(KeySlot::Cerebras, "preview".into());
            form.choose_backend(ChatBackend::Cerebras);
            form.choose_model(ModelPick::Other);
        },
    },
];

/// Every pane as configured, then each state after its pane, in rail order.
fn shots() -> Vec<Shot> {
    Tab::ALL
        .iter()
        .flat_map(|&tab| {
            let plain = Shot {
                tab,
                state: "",
                set: |_| {},
            };
            std::iter::once(plain).chain(STATES.iter().filter(move |s| s.tab == tab).copied())
        })
        .collect()
}

pub fn run(dir: PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dir)?;
    let cfg = Config::load()?;
    let app = Preview {
        app: SettingsApp::open(cfg.clone()),
        cfg,
        shots: shots(),
        dir,
        next: 0,
        shown_at: None,
        requested: false,
    };
    eframe::run_native(
        "Draft Settings Preview",
        native_options(),
        Box::new(|cc| {
            install_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

struct Preview {
    app: SettingsApp,
    /// The config as loaded, which the form is reset to after each state.
    cfg: Config,
    shots: Vec<Shot>,
    dir: PathBuf,
    /// Index into `shots` of the one being shown.
    next: usize,
    /// When the shot being shown was first drawn.
    shown_at: Option<f64>,
    /// A screenshot of it has been asked for and not yet answered.
    requested: bool,
}

impl eframe::App for Preview {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let Shot { tab, state, .. } = self.shots[self.next];
            let index = Tab::ALL.iter().position(|&t| t == tab).unwrap_or(0);
            let suffix = if state.is_empty() {
                String::new()
            } else {
                format!("-{state}")
            };
            let path = self.dir.join(format!(
                "{:02}-{}{suffix}.png",
                index + 1,
                tab.label().to_lowercase()
            ));
            if let Err(e) = image::save_buffer(
                &path,
                image.as_raw(),
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            ) {
                tracing::error!(path = %path.display(), error = %e, "preview not saved");
            }
            // Back to the config as loaded, so the next shot starts clean
            // and the window closes with nothing unsaved.
            if !state.is_empty() {
                self.app.form = Form::load(self.cfg.clone());
            }
            self.next += 1;
            self.shown_at = None;
            self.requested = false;
        }

        let Some(shot) = self.shots.get(self.next) else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        self.app.tab = shot.tab;
        let now = ctx.input(|i| i.time);
        if self.shown_at.is_none() {
            (shot.set)(&mut self.app.form);
            self.shown_at = Some(now);
        }
        self.app.ui(ctx);

        let shown_at = self.shown_at.unwrap_or(now);
        if !self.requested && now - shown_at >= SETTLE_SECS {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            self.requested = true;
        }
        ctx.request_repaint();
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.app.focus_trap.before_frame(raw_input);
    }
}
