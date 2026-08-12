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
// The module splits along what-changes-together lines:
// - `theme`   — every colour, metric, and the egui style install.
// - `widgets` — reusable stateless widgets (rows, toggles, buttons, modal).
// - here      — the app: state, tabs, dialogs, save/dirty logic.
// Read the header comments of `theme` and `widgets` before adding UI; they
// document the layout invariants (measure-then-allocate, bounded
// right_to_left, shared control metrics) this window depends on.

mod theme;
mod widgets;

use crate::autostart;
use crate::config::{Activation, Config, PasteMode, Provider};
use crate::secrets;
use crate::transcribe::parakeet_download::{self, Progress as DlProgress};
use egui::{Frame, Margin, RichText, Rounding, Vec2};
use std::sync::{Arc, Mutex};
use theme::*;
use widgets::*;

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

    let vocab_buffer = cfg.vocabulary.join("\n");
    let app = SettingsApp {
        tab: Tab::Recording,
        cfg,
        vocab_buffer,
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
    /// Editable text behind `cfg.vocabulary` — one term per line. The Vec is
    /// re-derived from this on every edit; the buffer keeps blank lines the
    /// user is still typing around.
    vocab_buffer: String,
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
                // Small on purpose: the scroll bar should sit near the window
                // edge, not float in the middle — `bar_inner_margin` already
                // keeps it clear of the content.
                right: 10.0,
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

        ui.add_space(14.0);

        group(ui, |ui| {
            toggle_row(
                ui,
                &mut self.cfg.push_to_command,
                "Push-to-command",
                "Hold a second hotkey and speak an instruction instead of \
                 dictating — the AI's answer is pasted at your cursor. \
                 Uses Groq; add its API key under Transcription.",
            );
            if self.cfg.push_to_command {
                divider(ui);
                row(ui, "Command hotkey", "Same syntax as the main hotkey.", |ui| {
                    text_input(ui, &mut self.cfg.command_hotkey, "Ctrl+Shift+Backslash", CONTROL_W);
                });
            }
        });
    }

    fn tab_transcription(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        group(ui, |ui| {
            row(ui, "Provider", "Where your audio is transcribed.", |ui| {
                let sel = self.cfg.provider.label();
                let options: Vec<(Provider, &str)> =
                    ALL_PROVIDERS.iter().map(|&p| (p, p.label())).collect();
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
            } else {
                divider(ui);
                toggle_row(
                    ui,
                    &mut self.cfg.fallback_to_local,
                    "Offline fallback",
                    "If the cloud call fails, the local model transcribes \
                     instead, so the dictation isn't lost. Requires the \
                     downloaded Parakeet model.",
                );
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

        ui.add_space(14.0);

        group(ui, |ui| {
            ui.label(RichText::new("Custom vocabulary").size(13.5).color(FG));
            ui.add_space(2.0);
            ui.label(
                RichText::new(
                    "Words your provider keeps mishearing — names, jargon, \
                     product terms. One per line. OpenAI and Groq take these \
                     as a hint; other providers ignore them, so use a \
                     replacement rule there instead.",
                )
                .size(11.5)
                .color(MUTED_FG),
            );
            ui.add_space(10.0);
            let resp = ui.add(
                egui::TextEdit::multiline(&mut self.vocab_buffer)
                    .desired_rows(5)
                    .desired_width(f32::INFINITY)
                    .hint_text(hint("e.g.  Janssen\n      kubectl\n      Reson8")),
            );
            if resp.changed() {
                self.cfg.vocabulary = self
                    .vocab_buffer
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
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
            divider(ui);
            toggle_row(
                ui,
                &mut self.cfg.voice_commands,
                "Voice commands",
                "Saying \"new line\", \"new paragraph\", \"scratch that\", or \
                 \"all caps\" formats the transcript instead of appearing in it.",
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
                    .hint_text(hint("Search transcripts…"))
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

    /// Modal API-key editor. Returns its outcome via a local action so we can
    /// mutate `keys` / close after the borrow.
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
            let scrim_clicked = modal_card(ctx, "key_dialog", |ui| {
                ui.label(
                    RichText::new(format!("{} API key", dlg.provider.label()))
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
                            .hint_text(hint("paste key…"))
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
            if scrim_clicked && matches!(act, Act::None) {
                act = Act::Cancel;
            }
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

    /// "Clear history?" confirmation with a destructive confirm. The wipe only
    /// runs on explicit confirm; the scrim, Cancel, and Esc all back out
    /// without touching the file.
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
        let scrim_clicked = modal_card(ctx, "confirm_clear", |ui| {
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
            // Pin the row height: an unconstrained right_to_left layout
            // fills the window's whole available rect and balloons it.
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), 34.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if ui.add(destructive_button("Clear")).clicked() {
                        act = Act::Confirm;
                    }
                    ui.add_space(8.0);
                    if ui.add(ghost_button("Cancel", 84.0, 34.0)).clicked() {
                        act = Act::Cancel;
                    }
                },
            );
        });
        if scrim_clicked && matches!(act, Act::None) {
            act = Act::Cancel;
        }

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


