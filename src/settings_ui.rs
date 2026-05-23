// egui-based settings window. Runs as a subprocess (`draft.exe --settings`)
// so it lives in its own event loop and can't deadlock the main pill /
// hotkey loop. On Save: writes config.toml, stashes API keys in the OS
// keyring, toggles autostart in the registry. The main process polls the
// subprocess; when it exits, the main process reloads config + re-registers
// the hotkey if it changed.

use crate::autostart;
use crate::config::{Activation, Config, PasteMode, Provider};
use crate::secrets;
use crate::transcribe::parakeet_download::{self, Progress as DlProgress};
use egui::{Color32, Frame, Margin, RichText, Rounding, Stroke, Vec2};
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

// Palette. Three-step elevation: panel (darkest) → card → control. Accent
// is a soft mint that doubles as the "on" colour for toggles and the Save
// button fill so the brand reads consistently.
const PANEL_BG: Color32 = Color32::from_rgb(22, 22, 26);
const CARD_FILL: Color32 = Color32::from_rgb(34, 34, 40);
const CARD_STROKE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 14);
const CONTROL_FILL: Color32 = Color32::from_rgb(48, 48, 56);
const CONTROL_HOVER: Color32 = Color32::from_rgb(58, 58, 68);
const DIVIDER: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 22);
const TEXT: Color32 = Color32::from_rgb(232, 232, 236);
const MUTED: Color32 = Color32::from_rgb(148, 148, 158);
const MUTED_STRONG: Color32 = Color32::from_rgb(178, 178, 188);
const ACCENT: Color32 = Color32::from_rgb(94, 207, 168);
const ACCENT_DIM: Color32 = Color32::from_rgb(64, 150, 122);
const ACCENT_ERR: Color32 = Color32::from_rgb(230, 120, 120);

// All inputs share this width so their left edges line up across rows.
const CONTROL_W: f32 = 220.0;
// Right column width (matches CONTROL_W plus the right-side gutter we leave
// before the card edge).
const RIGHT_COL_W: f32 = 240.0;

fn install_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.interact_size.y = 26.0;
    // Solid-column scrollbar: claims its own space instead of overlaying
    // content. Avoids the gutter where the scrollbar slid over the card's
    // right edge.
    style.spacing.scroll.floating = false;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.bar_inner_margin = 4.0;
    style.spacing.scroll.bar_outer_margin = 0.0;

    let v = &mut style.visuals;
    v.window_fill = PANEL_BG;
    v.panel_fill = PANEL_BG;
    v.window_rounding = Rounding::same(10.0);
    v.override_text_color = Some(TEXT);

    let r = Rounding::same(7.0);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
        &mut v.widgets.noninteractive,
    ] {
        w.rounding = r;
    }
    v.widgets.inactive.bg_fill = CONTROL_FILL;
    v.widgets.inactive.weak_bg_fill = CONTROL_FILL;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    v.widgets.hovered.bg_fill = CONTROL_HOVER;
    v.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 255, 255, 30));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);

    v.widgets.active.bg_fill = CONTROL_HOVER;
    v.widgets.active.weak_bg_fill = CONTROL_HOVER;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);

    v.widgets.open.bg_fill = CONTROL_HOVER;
    v.widgets.open.weak_bg_fill = CONTROL_HOVER;
    v.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT_DIM);

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
        self.save_status = Some((true, "Saved. Changes apply when this window closes.".into()));
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Footer pinned to bottom so the Save/Close pair never drifts as
        // sections grow or shrink (provider section changes height).
        egui::TopBottomPanel::bottom("footer")
            .show_separator_line(false)
            .exact_height(56.0)
            .frame(
                Frame::default()
                    .fill(PANEL_BG)
                    .inner_margin(Margin::symmetric(22.0, 0.0)),
            )
            .show(ctx, |ui| self.footer(ui, ctx));

        egui::CentralPanel::default()
            .frame(Frame::default().fill(PANEL_BG).inner_margin(Margin::symmetric(22.0, 20.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                    .show(ui, |ui| {
                        self.header(ui);
                        ui.add_space(16.0);
                        self.section_recording(ui);
                        ui.add_space(12.0);
                        self.section_transcription(ui, ctx);
                        ui.add_space(12.0);
                        self.section_output(ui);
                        ui.add_space(12.0);
                        self.section_system(ui);
                        ui.add_space(4.0);
                    });
            });
    }
}

impl SettingsApp {
    fn header(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Settings").size(24.0).strong().color(TEXT));
        ui.add_space(4.0);
        ui.label(
            RichText::new("Configure how Draft listens, transcribes, and pastes.")
                .size(13.0)
                .color(MUTED),
        );
    }

    fn section_recording(&mut self, ui: &mut egui::Ui) {
        card(ui, "Recording", |ui| {
            row(ui, "Hotkey", "Push-to-talk key combination.", |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.hotkey)
                        .desired_width(CONTROL_W)
                        .hint_text("Ctrl+Backslash"),
                );
            });
            divider(ui);
            row(ui, "Activation", "Hold the key, or tap to toggle.", |ui| {
                egui::ComboBox::from_id_salt("activation")
                    .width(CONTROL_W)
                    .selected_text(match self.cfg.activation {
                        Activation::Hold => "Hold",
                        Activation::Toggle => "Toggle",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.cfg.activation, Activation::Hold, "Hold");
                        ui.selectable_value(&mut self.cfg.activation, Activation::Toggle, "Toggle");
                    });
            });
            divider(ui);
            toggle_row(
                ui,
                &mut self.cfg.double_press_lock,
                "Double-press to lock",
                "Tap the hotkey twice quickly to keep recording hands-free.",
            );
        });
    }

    fn section_transcription(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        card(ui, "Transcription", |ui| {
            row(
                ui,
                "Provider",
                "Where your audio is transcribed.",
                |ui| {
                    egui::ComboBox::from_id_salt("provider")
                        .width(CONTROL_W)
                        .selected_text(provider_label(self.cfg.provider))
                        .show_ui(ui, |ui| {
                            for &p in ALL_PROVIDERS {
                                ui.selectable_value(&mut self.cfg.provider, p, provider_label(p));
                            }
                        });
                },
            );

            if secrets::slot_name(self.cfg.provider).is_some() {
                divider(ui);
                row(
                    ui,
                    "API key",
                    "Stored in Windows Credential Manager.",
                    |ui| {
                        if let Some(slot) = self.keys.get_mut(self.cfg.provider) {
                            ui.add(
                                egui::TextEdit::singleline(slot)
                                    .password(true)
                                    .desired_width(CONTROL_W)
                                    .hint_text("paste key…"),
                            );
                        }
                    },
                );
            }

            if matches!(self.cfg.provider, Provider::LocalParakeet) {
                divider(ui);
                self.parakeet_row(ui, ctx);
            }
        });
    }

    fn parakeet_row(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut state = self.download_state.lock().unwrap();
        split_row(
            ui,
            |ui| {
                ui.label(RichText::new("Local model").size(13.5).color(TEXT));
                ui.label(
                    RichText::new("Parakeet TDT 0.6B (int8) — runs entirely on this PC.")
                        .size(11.5)
                        .color(MUTED),
                );
            },
            RIGHT_COL_W,
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
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(label)
                            .size(11.5)
                            .color(MUTED)
                            .monospace(),
                    );
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                } else {
                    if ui
                        .add_sized(
                            [CONTROL_W, 30.0],
                            egui::Button::new("Download model (~670 MB)"),
                        )
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
                }
                if let Some(Err(msg)) = &state.finished {
                    ui.add_space(4.0);
                    ui.label(RichText::new(msg).size(11.5).color(ACCENT_ERR));
                }
            },
        );
    }

    fn section_output(&mut self, ui: &mut egui::Ui) {
        card(ui, "Output", |ui| {
            row(
                ui,
                "Paste mode",
                "Use Type for hosts that swallow Ctrl+V.",
                |ui| {
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
                },
            );
            divider(ui);
            toggle_row(
                ui,
                &mut self.cfg.append_trailing_space,
                "Append trailing space",
                "Adds one space after each transcript so the next word doesn't smash into it.",
            );
            divider(ui);
            toggle_row(
                ui,
                &mut self.cfg.restore_clipboard,
                "Restore clipboard",
                "Put your previous clipboard back after pasting.",
            );
        });
    }

    fn section_system(&mut self, ui: &mut egui::Ui) {
        card(ui, "System", |ui| {
            toggle_row(
                ui,
                &mut self.autostart_enabled,
                "Start with Windows",
                "Launches Draft automatically on sign-in (HKCU registry entry).",
            );
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // `with_layout` alone won't vertically center unless the child UI
        // knows its full height. We explicitly allocate the panel's full
        // (width × height) rectangle so cross-axis `Align::Center` has
        // something to centre against.
        let size = Vec2::new(ui.available_width(), ui.available_height());
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let save = ui.add(primary_button("Save"));
                if save.clicked() {
                    self.save();
                }
                ui.add_space(8.0);
                if ui.add(secondary_button("Close")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if let Some((ok, msg)) = &self.save_status {
                    ui.add_space(12.0);
                    let color = if *ok { ACCENT } else { ACCENT_ERR };
                    ui.label(RichText::new(msg).size(12.0).color(color));
                }
            },
        );
    }
}

// ---- layout helpers ----------------------------------------------------

fn card<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // Section eyebrow: tiny accent square + letter-spaced label.
    ui.horizontal(|ui| {
        let (sq, _) = ui.allocate_exact_size(Vec2::new(4.0, 4.0), egui::Sense::hover());
        ui.painter().rect_filled(sq, Rounding::same(1.0), ACCENT);
        ui.add_space(6.0);
        let spaced: String = title
            .to_uppercase()
            .chars()
            .collect::<Vec<_>>()
            .chunks(1)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join(" ");
        ui.label(
            RichText::new(spaced)
                .size(10.5)
                .color(MUTED_STRONG)
                .strong(),
        );
    });
    ui.add_space(6.0);

    let resp = Frame::default()
        .fill(CARD_FILL)
        .stroke(Stroke::new(1.0, CARD_STROKE))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin::symmetric(16.0, 14.0))
        .show(ui, body);

    // Backlit top edge: 1px slightly-brighter highlight inside the stroke
    // along the top, faded at the corners — sells the "raised surface" feel.
    let r = resp.response.rect;
    let painter = ui.painter();
    let top = r.top() + 0.5;
    let inset = 14.0;
    painter.hline(
        (r.left() + inset)..=(r.right() - inset),
        top,
        Stroke::new(1.0, Color32::from_white_alpha(18)),
    );

    resp.inner
}

/// One label/value row. Label column is capped so long captions can't slide
/// under the control on the right.
fn row(
    ui: &mut egui::Ui,
    label: &str,
    caption: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    split_row(
        ui,
        |ui| {
            ui.label(RichText::new(label).size(13.5).color(TEXT));
            ui.label(RichText::new(caption).size(11.5).color(MUTED));
        },
        230.0,
        control,
    );
}

/// Two-column row. Label column gets a fixed width on the left; the right
/// column claims ALL remaining horizontal space via `right_to_left`, so the
/// control hits the card's actual right inner edge — no pre-allocated
/// sub-region that could undershoot.
fn split_row(
    ui: &mut egui::Ui,
    left: impl FnOnce(&mut egui::Ui),
    control_w: f32,
    right: impl FnOnce(&mut egui::Ui),
) {
    let _ = control_w; // kept for API stability with the caller sites
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
        // Take *all* remaining horizontal space and right-align inside it.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(ui);
        });
    });
}

/// Full-row clickable toggle. The whole label/caption strip is the hit
/// area; on hover the row gets a faint highlight so the affordance reads.
fn toggle_row(ui: &mut egui::Ui, value: &mut bool, label: &str, caption: &str) {
    let id = ui.make_persistent_id(("toggle_row", label));
    let total_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(total_w, 44.0),
        egui::Sense::click(),
    );
    if resp.clicked() {
        *value = !*value;
    }
    if resp.hovered() {
        // Use from_black_alpha / from_white_alpha — passing (255,255,255,6)
        // to from_rgba_premultiplied was the bug that flashed the row near
        // white. Here we want a faint *lightening* wash.
        ui.painter().rect_filled(
            rect.expand2(Vec2::new(6.0, 2.0)),
            Rounding::same(8.0),
            Color32::from_white_alpha(8),
        );
    }
    // Manually paint text and toggle inside the allocated row rect so the
    // hit area exactly matches what the user sees.
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    text_ui.add_space(3.0);
    text_ui.label(RichText::new(label).size(13.5).color(TEXT));
    text_ui.label(RichText::new(caption).size(11.5).color(MUTED));

    let toggle_size = Vec2::new(36.0, 20.0);
    let toggle_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - toggle_size.x - 2.0, rect.center().y - toggle_size.y / 2.0),
        toggle_size,
    );
    paint_toggle(ui, toggle_rect, *value, id, resp.hovered());
}

fn paint_toggle(ui: &mut egui::Ui, rect: egui::Rect, on: bool, id: egui::Id, hovered: bool) {
    let how_on = ui.ctx().animate_bool_with_time(id, on, 0.15);
    let off = Color32::from_rgb(70, 70, 80);
    let on_col = ACCENT;
    let bg = lerp_color(off, on_col, how_on);
    let bg = if hovered { lighten(bg, 0.05) } else { bg };
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(rect.height() / 2.0), bg);

    let pad = 2.5;
    let knob_r = rect.height() / 2.0 - pad;
    let knob_x = egui::lerp((rect.left() + pad + knob_r)..=(rect.right() - pad - knob_r), how_on);
    let knob_pos = egui::pos2(knob_x, rect.center().y);
    // Subtle shadow under the knob for depth.
    painter.circle_filled(
        knob_pos + Vec2::new(0.0, 0.6),
        knob_r,
        Color32::from_rgba_premultiplied(0, 0, 0, 60),
    );
    painter.circle_filled(knob_pos, knob_r, Color32::from_rgb(245, 245, 248));
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn lighten(c: Color32, t: f32) -> Color32 {
    lerp_color(c, Color32::WHITE, t)
}

fn installed_badge(ui: &mut egui::Ui, text: &str) {
    // Caller is always in a right_to_left layout (split_row's right column).
    // Add text first → it lands at the right edge. Then the dot → lands to
    // the text's left. Visually: [●] [Installed (~670 MB)] flush to right.
    ui.label(RichText::new(text).color(ACCENT).size(13.0).strong());
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, ACCENT);
}

fn primary_button(text: &str) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let size = Vec2::new(96.0, 32.0);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let fill = if resp.is_pointer_button_down_on() {
            ACCENT_DIM
        } else if resp.hovered() {
            lighten(ACCENT, 0.08)
        } else {
            ACCENT
        };
        ui.painter().rect_filled(rect, Rounding::same(8.0), fill);
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(
            text.to_string(),
            egui::FontId::proportional(13.5),
            Color32::from_rgb(18, 30, 26),
        );
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, Color32::PLACEHOLDER);
        resp
    }
}

fn secondary_button(text: &str) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let size = Vec2::new(88.0, 32.0);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let fill = if resp.is_pointer_button_down_on() {
            CONTROL_HOVER
        } else if resp.hovered() {
            CONTROL_FILL
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, Rounding::same(8.0), fill);
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(
            text.to_string(),
            egui::FontId::proportional(13.5),
            MUTED_STRONG,
        );
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, Color32::PLACEHOLDER);
        resp
    }
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    let avail = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail, 1.0), egui::Sense::hover());
    ui.painter().hline(rect.left()..=rect.right(), rect.center().y, Stroke::new(1.0, DIVIDER));
    ui.add_space(8.0);
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
