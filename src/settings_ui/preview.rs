// `draft.exe --settings-preview <dir>`: open the settings window over the
// real config, show each pane in rail order, and save each as
// `<dir>/<nn>-<pane>.png`, then close. The pixels are the window's own — the
// same renderer, fonts and scale the user sees — read back with egui's
// screenshot command, so a review of the PNGs is a review of the window.
//
// It only looks: nothing is clicked or edited, so the form is never dirty,
// Save is never reached, and the close isn't held by the unsaved prompt.

use super::{install_style, native_options, SettingsApp, Tab};
use crate::config::Config;
use std::path::PathBuf;

/// How long a pane is shown before it is captured: long enough for egui's
/// animations (switch knobs, fades) to finish, which run ~0.1 s.
const SETTLE_SECS: f64 = 0.4;

pub fn run(dir: PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dir)?;
    let app = Preview {
        app: SettingsApp::open(Config::load()?),
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
    dir: PathBuf,
    /// Index into `Tab::ALL` of the pane being shown.
    next: usize,
    /// When the pane being shown was first drawn.
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
            let tab = Tab::ALL[self.next];
            let path = self.dir.join(format!(
                "{:02}-{}.png",
                self.next + 1,
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
            self.next += 1;
            self.shown_at = None;
            self.requested = false;
        }

        let Some(&tab) = Tab::ALL.get(self.next) else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        self.app.tab = tab;
        self.app.ui(ctx);

        let now = ctx.input(|i| i.time);
        let shown_at = *self.shown_at.get_or_insert(now);
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
