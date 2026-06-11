// egui-based settings window. Runs as a subprocess (`draft.exe --settings`)
// so it lives in its own event loop and can't deadlock the main pill /
// hotkey loop. On Save: writes config.toml, stashes API keys in the OS
// keyring, toggles autostart in the registry. The main process polls the
// subprocess; when it exits, it reloads config + re-registers the hotkey if
// it changed.
//
// Layout is a two-pane "app settings" shell: a left rail navigates between
// sections (Recording / Transcription / Output / System) and the right pane
// shows that section's rows as flush groups separated by hairline rules.
//
// Colours are the shadcn "neutral + lime" DARK theme, converted from the
// project's oklch design tokens to sRGB. Neutral greys carry the structure;
// lime (the `--primary`) appears only on the Save button, "on" toggles, the
// progress bar, and the installed dot. Selection / focus use neutral grey,
// not the accent. All inputs share one width and one height.

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

    let baseline = Snapshot {
        cfg: cfg.clone(),
        keys: keys.clone(),
        autostart_enabled,
    };

    let app = SettingsApp {
        tab: Tab::Recording,
        cfg,
        keys,
        autostart_enabled,
        key_dialog: None,
        baseline,
        save_status: None,
        download_state: Arc::new(Mutex::new(DownloadState::initial())),
        history: crate::history::load(),
        history_filter: String::new(),
        confirm_clear_history: false,
        input_devices: crate::audio::capture::input_device_names(),
    };

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([760.0, 560.0])
        .with_min_inner_size([680.0, 460.0])
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Recording,
    Transcription,
    Replacements,
    Output,
    History,
    System,
}

impl Tab {
    const ALL: &'static [Tab] = &[
        Tab::Recording,
        Tab::Transcription,
        Tab::Replacements,
        Tab::Output,
        Tab::History,
        Tab::System,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Recording => "Recording",
            Tab::Transcription => "Transcription",
            Tab::Replacements => "Replacements",
            Tab::Output => "Output",
            Tab::History => "History",
            Tab::System => "System",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Tab::Recording => "How Draft listens for your voice.",
            Tab::Transcription => "Where your speech becomes text.",
            Tab::Replacements => "Fix misheard words and expand shorthand before pasting.",
            Tab::Output => "How the transcript reaches your cursor.",
            Tab::History => "Recent transcripts — recover anything a paste missed.",
            Tab::System => "Startup and app behaviour.",
        }
    }
}

// shadcn "neutral + lime" DARK theme, oklch → sRGB.
const BG: Color32 = Color32::from_rgb(10, 10, 10); // --background  oklch(0.145 0 0)
const SIDEBAR_BG: Color32 = Color32::from_rgb(23, 23, 23); // --sidebar/--card  0.205
const CONTROL_FILL: Color32 = Color32::from_rgb(32, 32, 35); // input surface
const CONTROL_HOVER: Color32 = Color32::from_rgb(44, 44, 48);
const SELECTED_BG: Color32 = Color32::from_rgb(38, 38, 38); // --accent  0.269 (selected nav)
const FG: Color32 = Color32::from_rgb(250, 250, 250); // --foreground  0.985
const MUTED_FG: Color32 = Color32::from_rgb(161, 161, 161); // --muted-foreground  0.708
const RING: Color32 = Color32::from_rgb(115, 115, 115); // --ring  0.556 (neutral focus)
const PRIMARY: Color32 = Color32::from_rgb(132, 204, 22); // --primary (lime)
const PRIMARY_HOVER: Color32 = Color32::from_rgb(146, 214, 40);
const PRIMARY_PRESSED: Color32 = Color32::from_rgb(110, 172, 18);
const PRIMARY_FG: Color32 = Color32::from_rgb(53, 84, 14); // --primary-foreground (text on lime)
const DESTRUCTIVE: Color32 = Color32::from_rgb(235, 107, 107); // --destructive
const TOGGLE_OFF: Color32 = Color32::from_rgb(54, 54, 58);

// Borders are SOLID greys, not semi-transparent strokes. A 1px stroke of a
// translucent colour gets spread by egui's ~1px feathering, leaving gaps that
// read as a broken / "pixely" line; an opaque colour feathers into a clean,
// continuous hairline.
const BORDER: Color32 = Color32::from_rgb(38, 38, 42); // dividers / faint seams
const CONTROL_BORDER: Color32 = Color32::from_rgb(52, 52, 58); // input & button outlines
const CONTROL_BORDER_HOVER: Color32 = Color32::from_rgb(80, 80, 88);

fn border() -> Stroke {
    Stroke::new(1.0, BORDER)
}
fn input_border() -> Stroke {
    Stroke::new(1.0, CONTROL_BORDER)
}

const RAIL_W: f32 = 184.0;
const CONTROL_W: f32 = 240.0;
const CONTROL_H: f32 = 32.0;
const RADIUS: f32 = 10.0; // shadcn --radius (0.625rem) — controls, buttons, popovers
const RADIUS_SM: f32 = 8.0; // nav items, row hover

fn install_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    // egui makes labels selectable by default, which shows the text-select
    // I-beam over our row/label text and makes controls feel un-clickable.
    style.interaction.selectable_labels = false;
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.interact_size = Vec2::new(40.0, CONTROL_H);
    style.spacing.combo_height = 320.0;
    style.spacing.scroll.floating = false;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.bar_inner_margin = 4.0;
    style.spacing.scroll.bar_outer_margin = 0.0;

    let v = &mut style.visuals;
    v.panel_fill = BG;
    v.window_fill = SIDEBAR_BG; // popups (combo dropdowns)
    v.window_stroke = border();
    v.window_rounding = Rounding::same(RADIUS);
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0.0, 6.0].into(),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(120),
    };
    v.override_text_color = Some(FG);
    // Neutral text selection — never the accent.
    v.selection.bg_fill = Color32::from_rgb(51, 51, 56);
    v.selection.stroke = Stroke::new(1.0, RING);
    v.hyperlink_color = PRIMARY;

    let r = Rounding::same(RADIUS);
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
    v.widgets.inactive.bg_stroke = input_border();
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, FG);

    v.widgets.hovered.bg_fill = CONTROL_HOVER;
    v.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, CONTROL_BORDER_HOVER);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, FG);

    v.widgets.active.bg_fill = CONTROL_HOVER;
    v.widgets.active.weak_bg_fill = CONTROL_HOVER;
    v.widgets.active.bg_stroke = Stroke::new(1.0, RING); // neutral focus ring
    v.widgets.active.fg_stroke = Stroke::new(1.0, FG);

    v.widgets.open.bg_fill = CONTROL_FILL;
    v.widgets.open.weak_bg_fill = CONTROL_FILL;
    v.widgets.open.bg_stroke = Stroke::new(1.0, RING);

    ctx.set_style(style);
}

#[derive(Default, Clone, PartialEq, Eq)]
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

/// On-open (and post-save) snapshot used to detect unsaved changes.
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    cfg: Config,
    keys: ProviderKeys,
    autostart_enabled: bool,
}

/// Open API-key editor. Follows the write-only pattern: we never prefill or
/// redisplay the stored secret — the buffer starts empty and only overwrites
/// the saved key if the user actually types one.
struct KeyDialog {
    provider: Provider,
    buffer: String,
    reveal: bool,
}

struct SettingsApp {
    tab: Tab,
    cfg: Config,
    keys: ProviderKeys,
    autostart_enabled: bool,
    key_dialog: Option<KeyDialog>,
    baseline: Snapshot,
    save_status: Option<(bool, String)>,
    download_state: Arc<Mutex<DownloadState>>,
    /// Snapshot of the transcript history as of when this window opened. The
    /// main process appends to it live; "Refresh" re-reads from disk.
    history: Vec<crate::history::Entry>,
    /// Case-insensitive substring filter for the history list. Empty = show all.
    history_filter: String,
    /// True while the "Clear history?" confirmation modal is open. The wipe only
    /// happens once the user confirms — clearing is irreversible.
    confirm_clear_history: bool,
    /// Input device names enumerated at window open, for the microphone picker.
    input_devices: Vec<String>,
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
    fn current_snapshot(&self) -> Snapshot {
        Snapshot {
            cfg: self.cfg.clone(),
            keys: self.keys.clone(),
            autostart_enabled: self.autostart_enabled,
        }
    }

    fn is_dirty(&self) -> bool {
        self.current_snapshot() != self.baseline
    }

    fn save(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.save_status = Some((false, format!("Config save failed: {e}")));
            return;
        }
        for &p in ALL_PROVIDERS {
            if secrets::slot_name(p).is_none() {
                continue;
            }
            if let Err(e) = secrets::save_key(p, self.keys.get(p)) {
                self.save_status = Some((false, format!("Keyring save failed ({p:?}): {e}")));
                return;
            }
        }
        if let Err(e) = autostart::set_enabled(self.autostart_enabled) {
            self.save_status = Some((false, format!("Autostart toggle failed: {e}")));
            return;
        }
        self.baseline = self.current_snapshot();
        self.save_status = Some((true, "Saved. Applies when this window closes.".into()));
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (save_shortcut, close_shortcut) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if save_shortcut
            && self.key_dialog.is_none()
            && !self.confirm_clear_history
            && self.is_dirty()
        {
            self.save();
        }
        if close_shortcut {
            // Esc dismisses an open dialog first, then the window.
            if self.key_dialog.is_some() {
                self.key_dialog = None;
            } else if self.confirm_clear_history {
                self.confirm_clear_history = false;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        egui::SidePanel::left("rail")
            .resizable(false)
            .exact_width(RAIL_W)
            .frame(Frame::default().fill(SIDEBAR_BG).inner_margin(Margin {
                left: 12.0,
                right: 12.0,
                top: 18.0,
                bottom: 14.0,
            }))
            .show(ctx, |ui| self.rail(ui));

        egui::TopBottomPanel::bottom("footer")
            .show_separator_line(false)
            .exact_height(60.0)
            .frame(
                Frame::default()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(28.0, 0.0)),
            )
            .show(ctx, |ui| self.footer(ui, ctx));

        egui::CentralPanel::default()
            .frame(Frame::default().fill(BG).inner_margin(Margin {
                left: 28.0,
                right: 24.0,
                top: 24.0,
                bottom: 8.0,
            }))
            .show(ctx, |ui| {
                pane_header(ui, self.tab);
                ui.add_space(20.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .show(ui, |ui| match self.tab {
                        Tab::Recording => self.tab_recording(ui),
                        Tab::Transcription => self.tab_transcription(ui, ctx),
                        Tab::Replacements => self.tab_replacements(ui),
                        Tab::Output => self.tab_output(ui),
                        Tab::History => self.tab_history(ui),
                        Tab::System => self.tab_system(ui),
                    });
            });

        // Modals sit above everything when open.
        self.key_dialog_view(ctx);
        self.confirm_clear_view(ctx);
    }
}

impl SettingsApp {
    fn rail(&mut self, ui: &mut egui::Ui) {
        for &tab in Tab::ALL {
            if nav_item(ui, tab.label(), tab == self.tab) {
                self.tab = tab;
            }
            ui.add_space(2.0);
        }

        // Version pinned to the bottom of the rail.
        let rem = ui.available_height();
        if rem > 28.0 {
            ui.add_space(rem - 20.0);
        }
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                    .size(11.0)
                    .color(MUTED_FG),
            );
        });
    }

    fn tab_recording(&mut self, ui: &mut egui::Ui) {
        group(ui, |ui| {
            row(ui, "Hotkey", "Push-to-talk key combination.", |ui| {
                text_input(ui, &mut self.cfg.hotkey, "Ctrl+Backslash", CONTROL_W);
            });
            divider(ui);
            row(ui, "Microphone", "Which input device Draft records from.", |ui| {
                let selected = self
                    .cfg
                    .input_device
                    .clone()
                    .unwrap_or_else(|| "System default".into());
                // "System default" (None) plus one option per enumerated device.
                let mut options: Vec<(Option<String>, &str)> = vec![(None, "System default")];
                options.extend(self.input_devices.iter().map(|n| (Some(n.clone()), n.as_str())));
                combo(ui, "input_device", &mut self.cfg.input_device, &selected, &options);
            });
            divider(ui);
            row(ui, "Activation", "Hold the key, or tap to toggle.", |ui| {
                let sel = match self.cfg.activation {
                    Activation::Hold => "Hold",
                    Activation::Toggle => "Toggle",
                };
                combo(
                    ui,
                    "activation",
                    &mut self.cfg.activation,
                    sel,
                    &[(Activation::Hold, "Hold"), (Activation::Toggle, "Toggle")],
                );
            });
            // The double-press lock only applies in Hold mode — reveal it
            // progressively rather than showing a dead control in Toggle.
            if matches!(self.cfg.activation, Activation::Hold) {
                divider(ui);
                toggle_row(
                    ui,
                    &mut self.cfg.double_press_lock,
                    "Double-press to lock",
                    "Tap the hotkey twice quickly to keep recording hands-free.",
                );
            }
        });
    }

    fn tab_transcription(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        group(ui, |ui| {
            row(ui, "Provider", "Where your audio is transcribed.", |ui| {
                let sel = provider_label(self.cfg.provider);
                let options: Vec<(Provider, &str)> =
                    ALL_PROVIDERS.iter().map(|&p| (p, provider_label(p))).collect();
                combo(ui, "provider", &mut self.cfg.provider, sel, &options);
            });

            if secrets::slot_name(self.cfg.provider).is_some() {
                divider(ui);
                let configured = !self.keys.get(self.cfg.provider).trim().is_empty();
                let provider = self.cfg.provider;
                row(ui, "API key", "Stored in Windows Credential Manager.", |ui| {
                    // One full-width control, so it lines up with the Provider
                    // dropdown above. Opens the editor; the secret itself is
                    // never shown back here.
                    if key_opener(ui, configured) {
                        self.key_dialog = Some(KeyDialog {
                            provider,
                            buffer: String::new(),
                            reveal: false,
                        });
                    }
                });
            }

            if matches!(self.cfg.provider, Provider::LocalParakeet) {
                divider(ui);
                self.parakeet_row(ui, ctx);
            }
        });
    }

    fn parakeet_row(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Recover rather than panic if the download worker poisoned the lock —
        // a failed download shouldn't take down the whole settings window.
        let mut state = self
            .download_state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        split_row(
            ui,
            |ui| {
                ui.label(RichText::new("Local model").size(13.5).color(FG));
                ui.label(
                    RichText::new("Parakeet TDT 0.6B (int8) — runs entirely on this PC.")
                        .size(11.5)
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
                            .rounding(Rounding::same(3.0))
                            .fill(PRIMARY),
                    );
                    ui.add_space(5.0);
                    ui.label(RichText::new(label).size(11.5).color(MUTED_FG).monospace());
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                } else {
                    if ui
                        .add(ghost_button("Download model (~670 MB)", CONTROL_W, CONTROL_H))
                        .clicked()
                    {
                        state.running = true;
                        state.finished = None;
                        state.progress = None;
                        let handle = self.download_state.clone();
                        let repaint_ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let cb = {
                                let handle = handle.clone();
                                let repaint_ctx = repaint_ctx.clone();
                                move |p: DlProgress| {
                                    let mut s = handle.lock().unwrap();
                                    s.progress = Some(p);
                                    repaint_ctx.request_repaint();
                                }
                            };
                            let result = parakeet_download::download(cb);
                            let mut s = handle.lock().unwrap();
                            s.running = false;
                            s.model_present = parakeet_download::is_present();
                            s.finished = Some(result.map_err(|e| e.to_string()));
                            drop(s);
                            // The progress repaint loop stops once running=false;
                            // wake the UI once more so the final state shows.
                            repaint_ctx.request_repaint();
                        });
                    }
                }
                if let Some(Err(msg)) = &state.finished {
                    ui.add_space(4.0);
                    ui.label(RichText::new(msg).size(11.5).color(DESTRUCTIVE));
                }
            },
        );
    }

    fn tab_replacements(&mut self, ui: &mut egui::Ui) {
        group(ui, |ui| {
            ui.label(
                RichText::new(
                    "Rules run top to bottom on every transcript before it's pasted. \
                     Each rule's output feeds the next. Use them to fix words your \
                     provider mishears, or to expand shorthand.",
                )
                .size(12.0)
                .color(MUTED_FG),
            );
            ui.add_space(16.0);

            if self.cfg.replacements.is_empty() {
                ui.label(
                    RichText::new("No rules yet.")
                        .size(12.5)
                        .color(MUTED_FG)
                        .italics(),
                );
                ui.add_space(12.0);
            }

            // Edit in place; defer the structural removal until after the
            // borrow ends so we don't mutate the Vec mid-iteration.
            let mut remove: Option<usize> = None;
            let count = self.cfg.replacements.len();
            for i in 0..count {
                if i > 0 {
                    divider(ui);
                }
                if replacement_editor(ui, i, &mut self.cfg.replacements[i]) {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                self.cfg.replacements.remove(i);
            }

            if count > 0 {
                ui.add_space(16.0);
            }
            if ui
                .add(ghost_button("Add replacement", 160.0, CONTROL_H))
                .clicked()
            {
                self.cfg.replacements.push(crate::config::Replacement::default());
            }
        });
    }

    fn tab_output(&mut self, ui: &mut egui::Ui) {
        group(ui, |ui| {
            row(
                ui,
                "Paste mode",
                "Use Type for hosts that swallow Ctrl+V.",
                |ui| {
                    let sel = match self.cfg.paste_mode {
                        PasteMode::Clipboard => "Clipboard (Ctrl+V)",
                        PasteMode::Unicode => "Type (Unicode)",
                    };
                    combo(
                        ui,
                        "paste_mode",
                        &mut self.cfg.paste_mode,
                        sel,
                        &[
                            (PasteMode::Clipboard, "Clipboard (Ctrl+V)"),
                            (PasteMode::Unicode, "Type (Unicode)"),
                        ],
                    );
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

    fn tab_history(&mut self, ui: &mut egui::Ui) {
        // Actions are deferred past the immutable borrow of `self.history` the
        // list rendering holds, then applied once the closure returns.
        let mut copy: Option<String> = None;
        let mut refresh = false;
        let mut clear = false;

        group(ui, |ui| {
            ui.horizontal(|ui| {
                let count = self.history.len();
                let summary = match count {
                    0 => "Nothing recorded yet.".to_string(),
                    1 => "1 transcript.".to_string(),
                    n => format!("{n} transcripts (newest first)."),
                };
                ui.label(RichText::new(summary).size(12.0).color(MUTED_FG));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(ghost_button("Refresh", 84.0, CONTROL_H)).clicked() {
                        refresh = true;
                    }
                    if count > 0 {
                        ui.add_space(8.0);
                        if ui.add(ghost_button("Clear", 72.0, CONTROL_H)).clicked() {
                            clear = true;
                        }
                    }
                });
            });
            ui.add_space(8.0);

            if self.history.is_empty() {
                ui.label(
                    RichText::new(
                        "Transcripts appear here the moment they're produced — even if the \
                         paste lands in the wrong place. Use Copy to put one back on your \
                         clipboard.",
                    )
                    .size(12.0)
                    .color(MUTED_FG)
                    .italics(),
                );
                return;
            }

            // Search box: filters the list as you type. Full width so it lines
            // up with the entries below.
            ui.add_sized(
                [ui.available_width(), CONTROL_H],
                egui::TextEdit::singleline(&mut self.history_filter)
                    .hint_text("Search transcripts…")
                    .vertical_align(egui::Align::Center),
            );
            ui.add_space(10.0);

            // Case-insensitive substring match over text and provider. Computed
            // once; an empty needle matches everything.
            let needle = self.history_filter.trim().to_lowercase();
            let matches = |e: &crate::history::Entry| {
                needle.is_empty()
                    || e.text.to_lowercase().contains(&needle)
                    || e.provider.to_lowercase().contains(&needle)
            };

            // Newest first. The store keeps oldest-first, so walk it in reverse.
            let now = crate::history::now_unix();
            let mut shown = 0usize;
            for entry in self.history.iter().rev().filter(|e| matches(e)) {
                if shown > 0 {
                    divider(ui);
                }
                shown += 1;
                ui.horizontal(|ui| {
                    let copy_w = 64.0;
                    let text_w = (ui.available_width() - copy_w - 12.0).max(160.0);
                    ui.vertical(|ui| {
                        ui.set_width(text_w);
                        ui.add(
                            egui::Label::new(RichText::new(&entry.text).size(13.0).color(FG))
                                .wrap(),
                        );
                        ui.add_space(3.0);
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                relative_time(now, entry.ts),
                                entry.provider
                            ))
                            .size(11.0)
                            .color(MUTED_FG),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(ghost_button("Copy", copy_w, 28.0)).clicked() {
                            copy = Some(entry.text.clone());
                        }
                    });
                });
            }

            // The list is non-empty but the filter hid everything.
            if shown == 0 {
                ui.label(
                    RichText::new("No transcripts match your search.")
                        .size(12.0)
                        .color(MUTED_FG)
                        .italics(),
                );
            }
        });

        if let Some(text) = copy {
            ui.output_mut(|o| o.copied_text = text);
            self.save_status = Some((true, "Copied to clipboard.".into()));
        }
        if clear {
            // Don't wipe on the click — open the confirmation modal first.
            self.confirm_clear_history = true;
        }
        if refresh {
            self.history = crate::history::load();
        }
    }

    fn tab_system(&mut self, ui: &mut egui::Ui) {
        group(ui, |ui| {
            toggle_row(
                ui,
                &mut self.autostart_enabled,
                "Start with Windows",
                "Launches Draft automatically on sign-in (HKCU registry entry).",
            );
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Top hairline so the footer reads as a distinct bar.
        ui.painter().hline(
            ui.max_rect().x_range(),
            ui.max_rect().top(),
            border(),
        );
        let dirty = self.is_dirty();
        let size = Vec2::new(ui.available_width(), ui.available_height());
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let save = ui.add(primary_button("Save", dirty));
                if save.clicked() && dirty {
                    self.save();
                }
                ui.add_space(8.0);
                if ui.add(ghost_button("Close", 84.0, 34.0)).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }

                ui.add_space(14.0);
                match (&self.save_status, dirty) {
                    (Some((false, msg)), _) => {
                        ui.label(RichText::new(msg).size(12.0).color(DESTRUCTIVE));
                    }
                    (Some((true, msg)), false) => {
                        ui.label(RichText::new(msg).size(12.0).color(MUTED_FG));
                    }
                    (_, true) => {
                        let (dot, _) =
                            ui.allocate_exact_size(Vec2::new(7.0, 7.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.5, MUTED_FG);
                        ui.add_space(6.0);
                        ui.label(RichText::new("Unsaved changes").size(12.0).color(MUTED_FG));
                    }
                    _ => {}
                }
            },
        );
    }

    /// Modal API-key editor: dimmed scrim + centred card. Returns its outcome
    /// via a local action so we can mutate `keys` / close after the borrow.
    fn key_dialog_view(&mut self, ctx: &egui::Context) {
        enum Act {
            None,
            Commit(Provider, String),
            Remove(Provider),
            Cancel,
        }
        let mut act = Act::None;
        {
            let Some(dlg) = self.key_dialog.as_mut() else {
                return;
            };
            let configured = !self.keys.get(dlg.provider).trim().is_empty();
            let screen = ctx.screen_rect();

            // Scrim above the panels; clicking it cancels.
            egui::Area::new(egui::Id::new("key_scrim"))
                .order(egui::Order::Middle)
                .fixed_pos(screen.left_top())
                .show(ctx, |ui| {
                    let r = ui.allocate_rect(screen, egui::Sense::click());
                    ui.painter()
                        .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(160));
                    if r.clicked() {
                        act = Act::Cancel;
                    }
                });

            egui::Window::new("key_dialog")
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .movable(false)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .frame(
                    Frame::default()
                        .fill(SIDEBAR_BG)
                        .stroke(border())
                        .rounding(Rounding::same(RADIUS))
                        .inner_margin(Margin::same(20.0))
                        .shadow(egui::epaint::Shadow {
                            offset: [0.0, 12.0].into(),
                            blur: 48.0,
                            spread: 0.0,
                            color: Color32::from_black_alpha(160),
                        }),
                )
                .show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.label(
                        RichText::new(format!("{} API key", provider_label(dlg.provider)))
                            .size(15.0)
                            .strong()
                            .color(FG),
                    );
                    ui.add_space(4.0);
                    let desc = if configured {
                        "A key is already saved. Enter a new one to replace it."
                    } else {
                        "Paste your key. It is stored in Windows Credential Manager."
                    };
                    ui.label(RichText::new(desc).size(12.0).color(MUTED_FG));
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        let show_w = 56.0;
                        let field_w = ui.available_width() - show_w - 8.0;
                        ui.add_sized(
                            [field_w, CONTROL_H],
                            egui::TextEdit::singleline(&mut dlg.buffer)
                                .password(!dlg.reveal)
                                .hint_text("paste key…")
                                .vertical_align(egui::Align::Center),
                        );
                        ui.add_space(8.0);
                        let eye = if dlg.reveal { "Hide" } else { "Show" };
                        if ui.add(ghost_button(eye, show_w, CONTROL_H)).clicked() {
                            dlg.reveal = !dlg.reveal;
                        }
                    });

                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        if configured && ui.add(ghost_button("Remove", 84.0, 34.0)).clicked() {
                            act = Act::Remove(dlg.provider);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let can_save = !dlg.buffer.trim().is_empty();
                            if ui.add(primary_button("Save", can_save)).clicked() && can_save {
                                act = Act::Commit(dlg.provider, dlg.buffer.clone());
                            }
                            ui.add_space(8.0);
                            if ui.add(ghost_button("Cancel", 84.0, 34.0)).clicked() {
                                act = Act::Cancel;
                            }
                        });
                    });
                });
        }

        match act {
            Act::Commit(p, buf) => {
                self.keys.set(p, buf);
                self.key_dialog = None;
            }
            Act::Remove(p) => {
                self.keys.set(p, String::new());
                self.key_dialog = None;
            }
            Act::Cancel => self.key_dialog = None,
            Act::None => {}
        }
    }

    /// "Clear history?" confirmation: dimmed scrim + centred card with a
    /// destructive confirm. The wipe only runs on explicit confirm; the scrim,
    /// Cancel, and Esc all back out without touching the file.
    fn confirm_clear_view(&mut self, ctx: &egui::Context) {
        if !self.confirm_clear_history {
            return;
        }
        enum Act {
            None,
            Confirm,
            Cancel,
        }
        let mut act = Act::None;
        let screen = ctx.screen_rect();

        egui::Area::new(egui::Id::new("confirm_clear_scrim"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.left_top())
            .show(ctx, |ui| {
                let r = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter()
                    .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(160));
                if r.clicked() {
                    act = Act::Cancel;
                }
            });

        egui::Window::new("confirm_clear")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .movable(false)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(
                Frame::default()
                    .fill(SIDEBAR_BG)
                    .stroke(border())
                    .rounding(Rounding::same(RADIUS))
                    .inner_margin(Margin::same(20.0))
                    .shadow(egui::epaint::Shadow {
                        offset: [0.0, 12.0].into(),
                        blur: 48.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(160),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(
                    RichText::new("Clear history?")
                        .size(15.0)
                        .strong()
                        .color(FG),
                );
                ui.add_space(4.0);
                let msg = match self.history.len() {
                    1 => "This permanently deletes the 1 saved transcript. \
                          You won't be able to recover it."
                        .to_string(),
                    n => format!(
                        "This permanently deletes all {n} saved transcripts. \
                         You won't be able to recover them."
                    ),
                };
                ui.label(RichText::new(msg).size(12.0).color(MUTED_FG));
                ui.add_space(18.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(destructive_button("Clear")).clicked() {
                        act = Act::Confirm;
                    }
                    ui.add_space(8.0);
                    if ui.add(ghost_button("Cancel", 84.0, 34.0)).clicked() {
                        act = Act::Cancel;
                    }
                });
            });

        match act {
            Act::Confirm => {
                self.confirm_clear_history = false;
                if let Err(e) = crate::history::clear() {
                    self.save_status = Some((false, format!("Couldn't clear history: {e}")));
                } else {
                    self.history.clear();
                    self.save_status = Some((true, "History cleared.".into()));
                }
            }
            Act::Cancel => self.confirm_clear_history = false,
            Act::None => {}
        }
    }
}

// ---- pane chrome -------------------------------------------------------

fn pane_header(ui: &mut egui::Ui, tab: Tab) {
    ui.label(RichText::new(tab.label()).size(21.0).strong().color(FG));
    ui.add_space(3.0);
    ui.label(RichText::new(tab.subtitle()).size(12.5).color(MUTED_FG));
}

/// A flush group of rows constrained to a readable column width.
fn group<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let max_w = 560.0_f32.min(ui.available_width());
    ui.allocate_ui_with_layout(
        Vec2::new(max_w, 0.0),
        egui::Layout::top_down(egui::Align::LEFT),
        |ui| {
            ui.set_max_width(max_w);
            body(ui)
        },
    )
    .inner
}

// ---- rail nav ----------------------------------------------------------

/// One left-rail nav item. Selected gets a neutral filled pill; hover gets a
/// faint wash. No accent colour. Returns true on click.
fn nav_item(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 32.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let id = ui.make_persistent_id(("nav", label));

    let hover_t = ui
        .ctx()
        .animate_bool_with_time(id.with("h"), resp.hovered() && !selected, 0.12);

    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, Rounding::same(RADIUS_SM), SELECTED_BG);
    } else if hover_t > 0.0 {
        painter.rect_filled(
            rect,
            Rounding::same(RADIUS_SM),
            Color32::from_white_alpha((hover_t * 8.0) as u8),
        );
    }

    let color = if selected || resp.hovered() { FG } else { MUTED_FG };
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), egui::FontId::proportional(13.5), color);
    let pos = egui::pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, Color32::PLACEHOLDER);

    resp.clicked()
}

// ---- dropdown ----------------------------------------------------------

/// shadcn-style select. The button shares the input metrics; the popup lists
/// options with a hover highlight and a trailing check on the current value.
fn combo<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    id: &str,
    current: &mut T,
    selected_text: &str,
    options: &[(T, &str)],
) {
    let r = egui::ComboBox::from_id_salt(id)
        .width(CONTROL_W)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (val, label) in options {
                if combo_item(ui, label, *current == *val) {
                    *current = val.clone();
                    ui.close_menu();
                }
            }
        });
    r.response.on_hover_cursor(egui::CursorIcon::PointingHand);
}

/// One option row in a dropdown: hover fill + a trailing checkmark when it's
/// the current value.
fn combo_item(ui: &mut egui::Ui, text: &str, selected: bool) -> bool {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() {
        ui.painter().rect_filled(rect, Rounding::same(6.0), SELECTED_BG);
    }
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), FG);
    let pos = egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, Color32::PLACEHOLDER);

    if selected {
        // Hand-drawn check so it doesn't depend on glyph coverage.
        let cy = rect.center().y;
        let cx = rect.right() - 16.0;
        ui.painter().add(egui::Shape::line(
            vec![
                egui::pos2(cx - 4.0, cy + 0.5),
                egui::pos2(cx - 1.0, cy + 3.5),
                egui::pos2(cx + 5.0, cy - 4.0),
            ],
            Stroke::new(1.6, FG),
        ));
    }
    resp.clicked()
}

/// API-key opener. A full-width control (same metrics as the dropdowns, so it
/// shares their left/right edges) showing whether a key is set, with a
/// trailing "Set / Change" affordance. Returns true on click.
fn key_opener(ui: &mut egui::Ui, configured: bool) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(CONTROL_W, CONTROL_H), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if resp.hovered() { CONTROL_HOVER } else { CONTROL_FILL };
    ui.painter().rect_filled(rect, Rounding::same(RADIUS), fill);
    ui.painter()
        .rect_stroke(rect, Rounding::same(RADIUS), input_border());

    let mut x = rect.left() + 12.0;
    if configured {
        ui.painter()
            .circle_filled(egui::pos2(x + 1.0, rect.center().y), 3.0, PRIMARY);
        x += 12.0;
    }
    let (status, scolor) = if configured {
        ("Configured", FG)
    } else {
        ("Not set", MUTED_FG)
    };
    let g = ui
        .painter()
        .layout_no_wrap(status.to_string(), egui::FontId::proportional(13.0), scolor);
    let gy = rect.center().y - g.size().y / 2.0;
    ui.painter().galley(egui::pos2(x, gy), g, Color32::PLACEHOLDER);

    let action = if configured { "Change" } else { "Set" };
    let ga = ui.painter().layout_no_wrap(
        action.to_string(),
        egui::FontId::proportional(12.5),
        MUTED_FG,
    );
    let ax = rect.right() - 12.0 - ga.size().x;
    ui.painter()
        .galley(egui::pos2(ax, rect.center().y - ga.size().y / 2.0), ga, Color32::PLACEHOLDER);

    resp.clicked()
}

// ---- rows --------------------------------------------------------------

/// Single-line text input forced to a fixed width and height so every input
/// in the window lines up.
fn text_input(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32) {
    ui.add_sized(
        [width, CONTROL_H],
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .vertical_align(egui::Align::Center),
    );
}

/// One label/value row. Label column is capped so long captions can't slide
/// under the control on the right.
fn row(ui: &mut egui::Ui, label: &str, caption: &str, control: impl FnOnce(&mut egui::Ui)) {
    split_row(
        ui,
        |ui| {
            ui.label(RichText::new(label).size(13.5).color(FG));
            ui.label(RichText::new(caption).size(11.5).color(MUTED_FG));
        },
        control,
    );
}

/// Two-column row: label column left, control claims the remaining width and
/// right-aligns so controls share a right edge.
fn split_row(
    ui: &mut egui::Ui,
    left: impl FnOnce(&mut egui::Ui),
    right: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        let total = ui.available_width();
        let label_w = (total - CONTROL_W - 24.0).clamp(120.0, 280.0);
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

/// One editable find/replace rule: an enable switch, the from/to fields, the
/// two match flags, and a remove button. Returns true when removal is asked.
fn replacement_editor(ui: &mut egui::Ui, idx: usize, rule: &mut crate::config::Replacement) -> bool {
    let mut remove = false;
    let field_w = 140.0;
    ui.horizontal(|ui| {
        // Drive gaps with explicit spacing so the row width is predictable
        // and doesn't wrap in the narrow window.
        ui.spacing_mut().item_spacing.x = 0.0;
        let id = ui.make_persistent_id(("repl_enabled", idx));
        if mini_switch(ui, rule.enabled, id) {
            rule.enabled = !rule.enabled;
        }
        ui.add_space(10.0);
        ui.add_sized(
            [field_w, CONTROL_H],
            egui::TextEdit::singleline(&mut rule.from)
                .hint_text("hears…")
                .vertical_align(egui::Align::Center),
        );
        ui.add_space(8.0);
        ui.label(RichText::new("→").size(15.0).color(MUTED_FG));
        ui.add_space(8.0);
        ui.add_sized(
            [field_w, CONTROL_H],
            egui::TextEdit::singleline(&mut rule.to)
                .hint_text("writes…")
                .vertical_align(egui::Align::Center),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(ghost_button("Remove", 72.0, CONTROL_H)).clicked() {
                remove = true;
            }
        });
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        // Indent the flags so they sit under the fields, clear of the switch.
        ui.add_space(46.0);
        ui.checkbox(&mut rule.whole_word, "Whole word");
        ui.add_space(14.0);
        ui.checkbox(&mut rule.case_sensitive, "Match case");
    });
    remove
}

/// Compact label-less toggle switch for inline use in list rows. Returns true
/// on click; the caller flips the bound value.
fn mini_switch(ui: &mut egui::Ui, on: bool, id: egui::Id) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(32.0, 18.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    paint_toggle(ui, rect, on, id, resp.hovered());
    resp.clicked()
}

/// Full-row clickable toggle. The whole label/caption strip is the hit area;
/// hover gives a faint highlight so the affordance reads.
fn toggle_row(ui: &mut egui::Ui, value: &mut bool, label: &str, caption: &str) {
    let id = ui.make_persistent_id(("toggle_row", label));
    let total_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, 44.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        *value = !*value;
    }
    let hover_t = ui
        .ctx()
        .animate_bool_with_time(id.with("hover"), resp.hovered(), 0.12);
    if hover_t > 0.0 {
        ui.painter().rect_filled(
            rect.expand2(Vec2::new(8.0, 2.0)),
            Rounding::same(RADIUS_SM),
            Color32::from_white_alpha((hover_t * 7.0) as u8),
        );
    }
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    text_ui.add_space(3.0);
    text_ui.label(RichText::new(label).size(13.5).color(FG));
    text_ui.label(RichText::new(caption).size(11.5).color(MUTED_FG));

    let toggle_size = Vec2::new(36.0, 20.0);
    let toggle_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.right() - toggle_size.x - 2.0,
            rect.center().y - toggle_size.y / 2.0,
        ),
        toggle_size,
    );
    paint_toggle(ui, toggle_rect, *value, id, resp.hovered());
}

fn paint_toggle(ui: &mut egui::Ui, rect: egui::Rect, on: bool, id: egui::Id, hovered: bool) {
    let how_on = ui.ctx().animate_bool_with_time(id, on, 0.15);
    let bg = lerp_color(TOGGLE_OFF, PRIMARY, how_on);
    let bg = if hovered { lighten(bg, 0.05) } else { bg };
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(rect.height() / 2.0), bg);

    let pad = 2.5;
    let knob_r = rect.height() / 2.0 - pad;
    let knob_x = egui::lerp((rect.left() + pad + knob_r)..=(rect.right() - pad - knob_r), how_on);
    let knob_pos = egui::pos2(knob_x, rect.center().y);
    painter.circle_filled(
        knob_pos + Vec2::new(0.0, 0.6),
        knob_r,
        Color32::from_black_alpha(60),
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
    // Caller is in a right_to_left layout: text first (lands right), then the
    // dot to its left → [●] [Installed (~670 MB)] flush right.
    ui.label(RichText::new(text).color(FG).size(13.0));
    ui.add_space(7.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, PRIMARY);
}

/// Primary (Save) button. Disabled when there's nothing to save; otherwise
/// lime fill that brightens on hover, darkens + scales down on press.
fn primary_button(text: &str, enabled: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let size = Vec2::new(94.0, 34.0);
        let sense = if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let (rect, resp) = ui.allocate_exact_size(size, sense);
        let id = ui.make_persistent_id(("primary_btn", text));
        let pressed = enabled && resp.is_pointer_button_down_on();
        let press_t = ui.ctx().animate_bool_with_time(id, pressed, 0.07);
        let draw_rect = rect.shrink(press_t * 1.2);

        let fill = if !enabled {
            CONTROL_FILL
        } else if pressed {
            PRIMARY_PRESSED
        } else if resp.hovered() {
            PRIMARY_HOVER
        } else {
            PRIMARY
        };
        ui.painter().rect_filled(draw_rect, Rounding::same(RADIUS), fill);
        let ink = if enabled { PRIMARY_FG } else { MUTED_FG };
        let galley =
            ui.painter()
                .layout_no_wrap(text.to_string(), egui::FontId::proportional(13.5), ink);
        let pos = draw_rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
        if enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

/// Destructive action button (confirming a history wipe). Red fill that
/// brightens on hover and darkens + scales down on press — same metrics as the
/// primary button so the two line up in a dialog footer.
fn destructive_button(text: &str) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let size = Vec2::new(94.0, 34.0);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let id = ui.make_persistent_id(("destructive_btn", text));
        let pressed = resp.is_pointer_button_down_on();
        let press_t = ui.ctx().animate_bool_with_time(id, pressed, 0.07);
        let draw_rect = rect.shrink(press_t * 1.2);

        let fill = if pressed {
            lerp_color(DESTRUCTIVE, Color32::BLACK, 0.18)
        } else if resp.hovered() {
            lighten(DESTRUCTIVE, 0.08)
        } else {
            DESTRUCTIVE
        };
        ui.painter().rect_filled(draw_rect, Rounding::same(RADIUS), fill);
        let galley =
            ui.painter()
                .layout_no_wrap(text.to_string(), egui::FontId::proportional(13.5), FG);
        let pos = draw_rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

/// Outline button: bordered, transparent fill, fills with the neutral accent
/// on hover. Used for Close, Show/Hide, and the model download.
fn ghost_button(text: &str, width: f32, height: f32) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click());
        let fill = if resp.is_pointer_button_down_on() {
            CONTROL_HOVER
        } else if resp.hovered() {
            SELECTED_BG
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, Rounding::same(RADIUS), fill);
        ui.painter()
            .rect_stroke(rect, Rounding::same(RADIUS), input_border());
        let color = if resp.hovered() { FG } else { MUTED_FG };
        let galley =
            ui.painter()
                .layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), color);
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    let avail = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.left()..=rect.right(), rect.center().y, border());
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

/// Coarse "x ago" rendering of a Unix timestamp relative to `now` — enough to
/// orient a recovery, without pulling in a date library.
fn relative_time(now: i64, ts: i64) -> String {
    let secs = (now - ts).max(0);
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
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
