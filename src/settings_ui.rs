// egui-based settings window. Runs as a subprocess (`draft.exe --settings`)
// so it lives in its own event loop and can't deadlock the main pill /
// hotkey loop. On Save: writes config.toml, stashes API keys in the OS
// keyring, toggles autostart in the registry. The main process polls the
// subprocess; when it exits, the main process reloads config + re-registers
// the hotkey if it changed.
//
// Visual language: shadcn/ui, ported to egui. A cool zinc palette, soft
// *solid* hairline borders (never white-alpha — those read as harsh), Card
// sections with a title + muted description, bordered inputs/selects with a
// mint focus ring, a switch, and tactile buttons that scale on press.

use crate::autostart;
use crate::config::{Activation, Config, PasteMode, Provider};
use crate::secrets;
use crate::transcribe::parakeet_download::{self, Progress as DlProgress};
use egui::{Color32, CursorIcon, FontId, Frame, Margin, RichText, Rounding, Sense, Stroke, Vec2};
use std::sync::{Arc, Mutex};

pub fn run() -> anyhow::Result<()> {
    let cfg = Config::load().unwrap_or_default();
    let autostart_enabled = autostart::is_enabled();

    let mut keys = ProviderKeys::default();
    for &p in ALL_PROVIDERS {
        keys.set(p, secrets::load_key(p).unwrap_or_default());
    }

    let app = SettingsApp {
        cfg,
        keys,
        autostart_enabled,
        save_status: None,
        download_state: Arc::new(Mutex::new(DownloadState::initial())),
    };

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([600.0, 880.0])
        .with_min_inner_size([560.0, 640.0])
        .with_title("Draft — Settings");

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Draft Settings",
        options,
        Box::new(|cc| {
            install_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

const ALL_PROVIDERS: &[Provider] = &[
    Provider::LocalParakeet,
    Provider::Mistral,
    Provider::Groq,
    Provider::Openai,
    Provider::Xai,
    Provider::Elevenlabs,
    Provider::Reson8,
];

// ---- shadcn zinc palette (dark) ----------------------------------------
// `background` → `card` → `input` is a gentle three-step lift. Borders are a
// solid mid-zinc, not a white overlay, so they stay soft on every surface.
const BG: Color32 = Color32::from_rgb(12, 12, 14); // --background
const CARD: Color32 = Color32::from_rgb(20, 20, 23); // --card
const POPOVER: Color32 = Color32::from_rgb(26, 26, 30); // --popover (combo menu)
const BORDER: Color32 = Color32::from_rgb(40, 40, 45); // --border
const INPUT_BG: Color32 = Color32::from_rgb(27, 27, 31); // --input field fill
const INPUT_BORDER: Color32 = Color32::from_rgb(48, 48, 54);
const MUTED: Color32 = Color32::from_rgb(40, 40, 45); // ghost-hover / switch off
const MUTED_HOVER: Color32 = Color32::from_rgb(50, 50, 56);
const FG: Color32 = Color32::from_rgb(236, 237, 241); // --foreground
const MUTED_FG: Color32 = Color32::from_rgb(146, 148, 157); // --muted-foreground
const ACCENT: Color32 = Color32::from_rgb(94, 207, 168); // --primary (mint brand)
const ACCENT_HOVER: Color32 = Color32::from_rgb(112, 216, 182);
const ACCENT_PRESSED: Color32 = Color32::from_rgb(78, 186, 150);
const ACCENT_DIM: Color32 = Color32::from_rgb(60, 140, 114); // selection / progress
const ACCENT_FG: Color32 = Color32::from_rgb(8, 28, 22); // text on primary
const ERR: Color32 = Color32::from_rgb(228, 124, 124); // --destructive

// Radii. Concentric-ish: card 12, controls 8.
const CARD_RADIUS: f32 = 12.0;
const INPUT_RADIUS: f32 = 8.0;

// All inputs share this width so their left edges line up across rows.
const CONTROL_W: f32 = 220.0;

fn install_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    // Tight vertical item spacing: egui inserts this gap around *every*
    // stacked widget, so a small value keeps label→caption pairings tight.
    style.spacing.item_spacing = Vec2::new(10.0, 4.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.interact_size.y = 32.0;
    style.spacing.combo_height = 320.0;
    // Solid-column scrollbar: claims its own space instead of overlaying
    // content, so it never slides over a card's right edge.
    style.spacing.scroll.floating = false;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.bar_inner_margin = 6.0;
    style.spacing.scroll.bar_outer_margin = 0.0;

    let v = &mut style.visuals;
    v.window_fill = BG;
    v.panel_fill = BG;
    v.override_text_color = Some(FG);

    // Popups (the combo menu) are the `--popover` surface: a touch lighter
    // than cards, a soft border, gentle shadow.
    v.window_fill = BG;
    v.window_rounding = Rounding::same(INPUT_RADIUS);
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.popup_shadow = egui::epaint::Shadow {
        offset: Vec2::new(0.0, 6.0),
        blur: 18.0,
        spread: 0.0,
        color: Color32::from_black_alpha(90),
    };
    v.menu_rounding = Rounding::same(INPUT_RADIUS);

    // Text fields paint on `extreme_bg_color`; pin it to the input fill so
    // text inputs and combo boxes share one surface.
    v.extreme_bg_color = INPUT_BG;

    let r = Rounding::same(INPUT_RADIUS);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
        &mut v.widgets.noninteractive,
    ] {
        w.rounding = r;
    }

    // Resting control: input fill + a soft solid border.
    v.widgets.inactive.bg_fill = INPUT_BG;
    v.widgets.inactive.weak_bg_fill = INPUT_BG;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, INPUT_BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, FG);

    // Hover: barely-there lift of the border.
    v.widgets.hovered.bg_fill = INPUT_BG;
    v.widgets.hovered.weak_bg_fill = INPUT_BG;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(64, 64, 71));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, FG);

    // Active / focused: mint ring.
    v.widgets.active.bg_fill = INPUT_BG;
    v.widgets.active.weak_bg_fill = INPUT_BG;
    v.widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, FG);

    v.widgets.open.bg_fill = INPUT_BG;
    v.widgets.open.weak_bg_fill = INPUT_BG;
    v.widgets.open.bg_stroke = Stroke::new(1.5, ACCENT);

    // Selected combo item / text selection / progress fill.
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;

    ctx.set_style(style);
}

#[derive(Default)]
struct ProviderKeys {
    mistral: String,
    groq: String,
    openai: String,
    xai: String,
    elevenlabs: String,
    reson8: String,
}

impl ProviderKeys {
    fn get(&self, p: Provider) -> &str {
        match p {
            Provider::Mistral => &self.mistral,
            Provider::Groq => &self.groq,
            Provider::Openai => &self.openai,
            Provider::Xai => &self.xai,
            Provider::Elevenlabs => &self.elevenlabs,
            Provider::Reson8 => &self.reson8,
            Provider::LocalParakeet => "",
        }
    }
    fn get_mut(&mut self, p: Provider) -> Option<&mut String> {
        Some(match p {
            Provider::Mistral => &mut self.mistral,
            Provider::Groq => &mut self.groq,
            Provider::Openai => &mut self.openai,
            Provider::Xai => &mut self.xai,
            Provider::Elevenlabs => &mut self.elevenlabs,
            Provider::Reson8 => &mut self.reson8,
            Provider::LocalParakeet => return None,
        })
    }
    fn set(&mut self, p: Provider, v: String) {
        if let Some(slot) = self.get_mut(p) {
            *slot = v;
        }
    }
}

struct SettingsApp {
    cfg: Config,
    keys: ProviderKeys,
    autostart_enabled: bool,
    save_status: Option<(bool, String)>,
    download_state: Arc<Mutex<DownloadState>>,
}

struct DownloadState {
    model_present: bool,
    running: bool,
    progress: Option<DlProgress>,
    finished: Option<Result<(), String>>,
}

impl DownloadState {
    fn initial() -> Self {
        Self {
            model_present: parakeet_download::is_present(),
            running: false,
            progress: None,
            finished: None,
        }
    }
}

impl SettingsApp {
    fn save(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.save_status = Some((false, format!("config save failed: {e}")));
            return;
        }
        for &p in ALL_PROVIDERS {
            if secrets::slot_name(p).is_none() {
                continue;
            }
            if let Err(e) = secrets::save_key(p, self.keys.get(p)) {
                self.save_status = Some((false, format!("keyring save failed ({p:?}): {e}")));
                return;
            }
        }
        if let Err(e) = autostart::set_enabled(self.autostart_enabled) {
            self.save_status = Some((false, format!("autostart toggle failed: {e}")));
            return;
        }
        self.save_status = Some((true, "Saved — changes apply when this window closes.".into()));
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Footer pinned to bottom so the Save/Close pair never drifts as
        // sections grow or shrink (the provider section changes height).
        egui::TopBottomPanel::bottom("footer")
            .show_separator_line(false)
            .exact_height(64.0)
            .frame(
                Frame::default()
                    .fill(BG)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(Margin::symmetric(24.0, 0.0)),
            )
            .show(ctx, |ui| self.footer(ui, ctx));

        egui::CentralPanel::default()
            .frame(Frame::default().fill(BG).inner_margin(Margin {
                left: 24.0,
                right: 16.0,
                top: 22.0,
                bottom: 8.0,
            }))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                    .show(ui, |ui| {
                        // Leave a little gutter so content doesn't kiss the scrollbar.
                        ui.style_mut().spacing.item_spacing.y = 4.0;
                        self.header(ui);
                        ui.add_space(20.0);
                        self.section_recording(ui);
                        ui.add_space(16.0);
                        self.section_transcription(ui, ctx);
                        ui.add_space(16.0);
                        self.section_output(ui);
                        ui.add_space(16.0);
                        self.section_system(ui);
                        ui.add_space(4.0);
                    });
            });
    }
}

impl SettingsApp {
    fn header(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Settings").size(23.0).strong().color(FG));
        ui.add_space(3.0);
        ui.label(
            RichText::new("Configure how Draft listens, transcribes, and pastes.")
                .size(13.0)
                .color(MUTED_FG),
        );
    }

    fn section_recording(&mut self, ui: &mut egui::Ui) {
        section(
            ui,
            "Recording",
            "How Draft starts and stops listening.",
            |ui| {
                field_row(ui, "Hotkey", "Push-to-talk key combination.", |ui| {
                    text_input(ui, &mut self.cfg.hotkey, "Ctrl+Backslash");
                });
                separator(ui);
                field_row(ui, "Activation", "Hold the key, or tap to toggle.", |ui| {
                    egui::ComboBox::from_id_salt("activation")
                        .width(CONTROL_W)
                        .selected_text(match self.cfg.activation {
                            Activation::Hold => "Hold",
                            Activation::Toggle => "Toggle",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.cfg.activation, Activation::Hold, "Hold");
                            ui.selectable_value(
                                &mut self.cfg.activation,
                                Activation::Toggle,
                                "Toggle",
                            );
                        });
                });
                separator(ui);
                switch_row(
                    ui,
                    &mut self.cfg.double_press_lock,
                    "Double-press to lock",
                    "Tap the hotkey twice quickly to keep recording hands-free.",
                );
            },
        );
    }

    fn section_transcription(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        section(
            ui,
            "Transcription",
            "Where your speech becomes text.",
            |ui| {
                field_row(ui, "Provider", "Engine that transcribes your audio.", |ui| {
                    egui::ComboBox::from_id_salt("provider")
                        .width(CONTROL_W)
                        .selected_text(provider_label(self.cfg.provider))
                        .show_ui(ui, |ui| {
                            for &p in ALL_PROVIDERS {
                                ui.selectable_value(&mut self.cfg.provider, p, provider_label(p));
                            }
                        });
                });

                if secrets::slot_name(self.cfg.provider).is_some() {
                    separator(ui);
                    field_row(ui, "API key", "Stored in Windows Credential Manager.", |ui| {
                        if let Some(slot) = self.keys.get_mut(self.cfg.provider) {
                            ui.add(
                                egui::TextEdit::singleline(slot)
                                    .password(true)
                                    .margin(Margin::symmetric(10.0, 7.0))
                                    .desired_width(CONTROL_W)
                                    .hint_text("paste key…"),
                            );
                        }
                    });
                }

                if matches!(self.cfg.provider, Provider::LocalParakeet) {
                    separator(ui);
                    self.parakeet_row(ui, ctx);
                }
            },
        );
    }

    fn parakeet_row(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut state = self.download_state.lock().unwrap();
        split_row(
            ui,
            |ui| {
                ui.label(RichText::new("Local model").size(13.5).color(FG));
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Parakeet TDT 0.6B (int8) — runs entirely on this PC.")
                        .size(12.0)
                        .color(MUTED_FG),
                );
            },
            |ui| {
                if state.model_present && !state.running {
                    installed_badge(ui, "Installed (~670 MB)");
                } else if state.running {
                    let (label, pct) = state
                        .progress
                        .as_ref()
                        .map(|p| (format_progress_label(p), progress_fraction(p)))
                        .unwrap_or_else(|| ("Starting download…".into(), 0.0));
                    ui.add(
                        egui::ProgressBar::new(pct)
                            .desired_width(CONTROL_W)
                            .desired_height(6.0)
                            .rounding(Rounding::same(3.0)),
                    );
                    ui.add_space(5.0);
                    ui.label(
                        RichText::new(label)
                            .size(11.5)
                            .color(MUTED_FG)
                            .monospace(),
                    );
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                } else if ui
                    .add(primary_button("Download model (~670 MB)", CONTROL_W))
                    .clicked()
                {
                    state.running = true;
                    state.finished = None;
                    state.progress = None;
                    let handle = self.download_state.clone();
                    std::thread::spawn(move || {
                        let cb = {
                            let handle = handle.clone();
                            move |p: DlProgress| {
                                let mut s = handle.lock().unwrap();
                                s.progress = Some(p);
                            }
                        };
                        let result = parakeet_download::download(cb);
                        let mut s = handle.lock().unwrap();
                        s.running = false;
                        s.model_present = parakeet_download::is_present();
                        s.finished = Some(result.map_err(|e| e.to_string()));
                    });
                }
                if let Some(Err(msg)) = &state.finished {
                    ui.add_space(4.0);
                    ui.label(RichText::new(msg).size(11.5).color(ERR));
                }
            },
        );
    }

    fn section_output(&mut self, ui: &mut egui::Ui) {
        section(
            ui,
            "Output",
            "How transcribed text reaches your app.",
            |ui| {
                field_row(ui, "Paste mode", "Use Type for hosts that swallow Ctrl+V.", |ui| {
                    egui::ComboBox::from_id_salt("paste_mode")
                        .width(CONTROL_W)
                        .selected_text(match self.cfg.paste_mode {
                            PasteMode::Clipboard => "Clipboard (Ctrl+V)",
                            PasteMode::Unicode => "Type (Unicode)",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.cfg.paste_mode,
                                PasteMode::Clipboard,
                                "Clipboard (Ctrl+V)",
                            );
                            ui.selectable_value(
                                &mut self.cfg.paste_mode,
                                PasteMode::Unicode,
                                "Type (Unicode)",
                            );
                        });
                });
                separator(ui);
                switch_row(
                    ui,
                    &mut self.cfg.append_trailing_space,
                    "Append trailing space",
                    "Adds one space after each transcript so the next word doesn't smash into it.",
                );
                separator(ui);
                switch_row(
                    ui,
                    &mut self.cfg.restore_clipboard,
                    "Restore clipboard",
                    "Put your previous clipboard back after pasting.",
                );
            },
        );
    }

    fn section_system(&mut self, ui: &mut egui::Ui) {
        section(ui, "System", "Startup and integration.", |ui| {
            switch_row(
                ui,
                &mut self.autostart_enabled,
                "Start with Windows",
                "Launches Draft automatically on sign-in (HKCU registry entry).",
            );
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Allocate the panel's full rect so cross-axis centering has a height
        // to centre against.
        let size = Vec2::new(ui.available_width(), ui.available_height());
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui.add(primary_button("Save", 92.0)).clicked() {
                    self.save();
                }
                ui.add_space(8.0);
                if ui.add(ghost_button("Close", 84.0)).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if let Some((ok, msg)) = &self.save_status {
                    ui.add_space(14.0);
                    let color = if *ok { ACCENT } else { ERR };
                    ui.label(RichText::new(msg).size(12.0).color(color));
                }
            },
        );
    }
}

// ---- layout helpers ----------------------------------------------------

/// shadcn `Card`: a bordered surface with a title + muted description header,
/// then the body. Grouping comes from the card and whitespace, not glows.
fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    description: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    Frame::default()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(CARD_RADIUS))
        .inner_margin(Margin::symmetric(20.0, 18.0))
        .show(ui, |ui| {
            ui.label(RichText::new(title).size(15.5).strong().color(FG));
            ui.add_space(3.0);
            ui.label(RichText::new(description).size(12.5).color(MUTED_FG));
            ui.add_space(16.0);
            body(ui)
        })
        .inner
}

/// One label/value row: label + caption stacked on the left, control pinned
/// to the right edge of the card.
fn field_row(ui: &mut egui::Ui, label: &str, caption: &str, control: impl FnOnce(&mut egui::Ui)) {
    split_row(
        ui,
        |ui| {
            ui.label(RichText::new(label).size(13.5).color(FG));
            ui.add_space(2.0);
            ui.label(RichText::new(caption).size(12.0).color(MUTED_FG));
        },
        control,
    );
}

/// Two-column row. The label column gets a fixed width on the left; the right
/// column claims ALL remaining width via `right_to_left`, so the control hits
/// the card's actual right inner edge.
fn split_row(ui: &mut egui::Ui, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let total = ui.available_width();
        let label_w = (total - 240.0).clamp(140.0, 320.0);
        ui.allocate_ui_with_layout(
            Vec2::new(label_w, 0.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.set_max_width(label_w);
                left(ui);
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(ui);
        });
    });
}

/// A shadcn-styled single-line text input with consistent padding.
fn text_input(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(value)
            .margin(Margin::symmetric(10.0, 7.0))
            .desired_width(CONTROL_W)
            .hint_text(hint),
    );
}

/// Full-row clickable switch. The whole label/caption strip is the hit area
/// (well over the 40px minimum); on hover the row gets a faint wash.
fn switch_row(ui: &mut egui::Ui, value: &mut bool, label: &str, caption: &str) {
    let id = ui.make_persistent_id(("switch_row", label));
    let total_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, 40.0), Sense::click());
    if resp.clicked() {
        *value = !*value;
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            rect.expand2(Vec2::new(8.0, 2.0)),
            Rounding::same(8.0),
            Color32::from_white_alpha(6),
        );
    }
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    text_ui.add_space(2.0);
    text_ui.label(RichText::new(label).size(13.5).color(FG));
    text_ui.add_space(2.0);
    text_ui.label(RichText::new(caption).size(12.0).color(MUTED_FG));

    let toggle_size = Vec2::new(38.0, 22.0);
    let toggle_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - toggle_size.x, rect.center().y - toggle_size.y / 2.0),
        toggle_size,
    );
    paint_switch(ui, toggle_rect, *value, id, resp.hovered());
}

fn paint_switch(ui: &mut egui::Ui, rect: egui::Rect, on: bool, id: egui::Id, hovered: bool) {
    let how_on = ui.ctx().animate_bool_with_time(id, on, 0.15);
    let off = MUTED;
    let track = lerp_color(off, ACCENT, how_on);
    let track = if hovered { lighten(track, 0.04) } else { track };
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(rect.height() / 2.0), track);

    let pad = 2.5;
    let knob_r = rect.height() / 2.0 - pad;
    let knob_x = egui::lerp((rect.left() + pad + knob_r)..=(rect.right() - pad - knob_r), how_on);
    let knob_pos = egui::pos2(knob_x, rect.center().y);
    // Faint shadow under the knob for just enough lift.
    painter.circle_filled(
        knob_pos + Vec2::new(0.0, 0.5),
        knob_r,
        Color32::from_black_alpha(40),
    );
    painter.circle_filled(knob_pos, knob_r, Color32::from_rgb(248, 249, 251));
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn lighten(c: Color32, t: f32) -> Color32 {
    lerp_color(c, Color32::WHITE, t)
}

fn installed_badge(ui: &mut egui::Ui, text: &str) {
    // Caller is in a right_to_left layout (split_row's right column): add the
    // text first so it lands at the right edge, then the dot to its left.
    ui.label(RichText::new(text).color(ACCENT).size(13.0).strong());
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, ACCENT);
}

/// Primary (filled mint) button. Scales to 0.96 on press for tactile feel;
/// the hit area stays full-size so the press never shifts layout.
fn primary_button(text: &str, width: f32) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
        let pressed = resp.is_pointer_button_down_on();
        let fill = if pressed {
            ACCENT_PRESSED
        } else if resp.hovered() {
            ACCENT_HOVER
        } else {
            ACCENT
        };
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let vis = press_scale(rect, pressed);
        ui.painter().rect_filled(vis, Rounding::same(INPUT_RADIUS), fill);
        paint_centered(ui, vis, text, ACCENT_FG);
        resp
    }
}

/// Ghost button: transparent until hover, then a faint muted fill.
fn ghost_button(text: &str, width: f32) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
        let pressed = resp.is_pointer_button_down_on();
        let fill = if pressed {
            MUTED_HOVER
        } else if resp.hovered() {
            MUTED
        } else {
            Color32::TRANSPARENT
        };
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let vis = press_scale(rect, pressed);
        ui.painter().rect_filled(vis, Rounding::same(INPUT_RADIUS), fill);
        let color = if resp.hovered() { FG } else { MUTED_FG };
        paint_centered(ui, vis, text, color);
        resp
    }
}

fn press_scale(rect: egui::Rect, pressed: bool) -> egui::Rect {
    if pressed {
        rect.shrink2(Vec2::new(rect.width() * 0.02, rect.height() * 0.02))
    } else {
        rect
    }
}

fn paint_centered(ui: &egui::Ui, rect: egui::Rect, text: &str, color: Color32) {
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(13.5), color);
    let pos = rect.center() - galley.size() / 2.0;
    painter.galley(pos, galley, Color32::PLACEHOLDER);
}

/// Hairline separator between rows, in the border colour.
fn separator(ui: &mut egui::Ui) {
    ui.add_space(7.0);
    let avail = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail, 1.0), Sense::hover());
    ui.painter()
        .hline(rect.left()..=rect.right(), rect.center().y, Stroke::new(1.0, BORDER));
    ui.add_space(7.0);
}

// ---- progress formatting ----------------------------------------------

fn progress_fraction(p: &DlProgress) -> f32 {
    let per_file = 1.0 / p.file_count.max(1) as f32;
    let within = match p.bytes_total {
        Some(total) if total > 0 => (p.bytes_done as f32 / total as f32).clamp(0.0, 1.0),
        _ => 0.0,
    };
    (p.file_index as f32 * per_file + within * per_file).clamp(0.0, 1.0)
}

fn format_progress_label(p: &DlProgress) -> String {
    let done_mb = p.bytes_done as f64 / 1_048_576.0;
    match p.bytes_total {
        Some(total) if total > 0 => {
            let total_mb = total as f64 / 1_048_576.0;
            format!(
                "{}/{}  {:>6.1} / {:>6.1} MB",
                p.file_index + 1,
                p.file_count,
                done_mb,
                total_mb
            )
        }
        _ => format!("{}/{}  {:>6.1} MB", p.file_index + 1, p.file_count, done_mb),
    }
}

fn provider_label(p: Provider) -> &'static str {
    match p {
        Provider::LocalParakeet => "Local (Parakeet)",
        Provider::Mistral => "Mistral (Voxtral)",
        Provider::Groq => "Groq",
        Provider::Openai => "OpenAI",
        Provider::Xai => "xAI",
        Provider::Elevenlabs => "ElevenLabs",
        Provider::Reson8 => "Reson8",
    }
}
