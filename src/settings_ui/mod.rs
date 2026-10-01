// egui-based settings window. Runs as a subprocess (`draft.exe --settings`)
// so it lives in its own event loop and can't deadlock the main pill /
// hotkey loop. On Save: writes config.toml, writes the API keys edited in
// this window to the OS keyring, toggles autostart in the registry. The main
// process polls the subprocess; when it exits, it reloads config +
// re-registers the hotkey if it changed.
//
// Layout is a two-pane "app settings" shell: a left rail navigates between
// panes (`Tab::ALL` is the rail order) and the right pane shows that pane's
// rows as flush groups separated by hairline rules.
//
// The module splits along what-changes-together lines:
// - `theme`    — every colour, metric, and the egui style install.
// - `widgets`  — reusable stateless widgets (rows, toggles, buttons, modal).
// - `state`    — what the window edits, the dirty check and Save; no egui.
// - `format`   — worked-out text: relative times, download progress.
// - `download` — the model download's shared state and worker.
// - `focus_trap` — keeps Tab inside an open dialog.
// - here       — the app: panes, dialogs, and the frame that draws them.
// Read the header comments of `theme` and `widgets` before adding UI; they
// document the layout invariants (measure-then-allocate, bounded
// right_to_left, shared control metrics) this window depends on.

mod download;
mod focus_trap;
mod format;
mod state;
mod theme;
mod widgets;

use crate::config::{Activation, Config, MonitorPolicy, PasteMode, PillBodyStyle, Provider};
use crate::secrets;
use crate::transcribe::parakeet_download;
use download::DownloadState;
use egui::{Frame, Margin, RichText, Rounding, Vec2};
use focus_trap::FocusTrap;
use format::{progress_fraction, progress_label, relative_time};
use state::{Form, SystemStore, ALL_PROVIDERS};
use std::sync::{Arc, Mutex};
use theme::*;
use widgets::*;

pub fn run() -> anyhow::Result<()> {
    // An unreadable config refuses to open rather than offering defaults to
    // Save — see ADR 0002 (#93).
    let cfg = match Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "config unreadable; settings not opened");
            refuse_to_open(&e);
            return Ok(());
        }
    };
    let app = SettingsApp {
        tab: Tab::Recording,
        form: Form::load(cfg),
        key_dialog: None,
        save_status: None,
        download_state: Arc::new(Mutex::new(DownloadState::new(
            parakeet_download::is_present(),
        ))),
        history: crate::history::load(),
        history_filter: String::new(),
        confirm_clear_history: false,
        unsaved_prompt: false,
        close_confirmed: false,
        input_devices: crate::audio::capture::input_device_names(),
        displays: pinnable_displays(),
        rule_keys: RuleKeys::default(),
        focus_trap: FocusTrap::default(),
    };

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([760.0, 560.0])
        .with_min_inner_size([680.0, 460.0])
        .with_title("Draft — Settings")
        // Without one eframe shows egui's logo in the title bar and taskbar.
        .with_icon(egui::IconData {
            rgba: crate::mark::app_rgba(64),
            width: 64,
            height: 64,
        });

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

/// Tell the user why Settings didn't open. A plain message box rather than an
/// egui window: there is nothing to lay out, and it blocks until dismissed, so
/// the subprocess exits (and the main process reloads) only once it's been read.
fn refuse_to_open(e: &anyhow::Error) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
    };

    // `{e:#}` prints the whole chain, which names the file.
    let text = format!(
        "Draft couldn't read its settings file, so Settings won't open \
         — saving from it would replace your settings with the defaults.\n\n\
         {e:#}"
    );
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("Draft — Settings"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

/// The displays the pill can be pinned to, as (device path, label).
///
/// A monitor whose EDID device path could not be read is left out rather than
/// listed unpinnable: there is nothing to store for it. It is still a perfectly
/// good home monitor under the other three policies.
fn pinnable_displays() -> Vec<(String, String)> {
    crate::pill::monitor::enumerate()
        .all()
        .iter()
        .filter_map(|m| {
            let path = m.device_path.clone()?;
            let label = m.friendly_name.clone().unwrap_or_else(|| {
                if m.primary {
                    "Primary display".to_string()
                } else {
                    "Unnamed display".to_string()
                }
            });
            Some((path, label))
        })
        .collect()
}

/// Picker order, which is also the order they get more specific in: follow me,
/// follow my mouse, always here, always *that* one.
const ALL_MONITOR_POLICIES: &[MonitorPolicy] = &[
    MonitorPolicy::Focused,
    MonitorPolicy::Cursor,
    MonitorPolicy::Primary,
    MonitorPolicy::Pinned,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Recording,
    Pill,
    Transcription,
    Replacements,
    Output,
    History,
    System,
}

impl Tab {
    /// Rail order, which is the order the panes are read in: what Draft
    /// listens with, what it shows you while it does, then where the words go.
    const ALL: &'static [Tab] = &[
        Tab::Recording,
        Tab::Pill,
        Tab::Transcription,
        Tab::Replacements,
        Tab::Output,
        Tab::History,
        Tab::System,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Recording => "Recording",
            Tab::Pill => "Pill",
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
            Tab::Pill => "The overlay that shows what Draft is doing.",
            Tab::Transcription => "Where your speech becomes text.",
            Tab::Replacements => "Fix misheard words and expand shorthand before pasting.",
            Tab::Output => "How the transcript reaches your cursor.",
            Tab::History => "Recent transcripts — recover anything a paste missed.",
            Tab::System => "Startup and app behaviour.",
        }
    }
}

/// Why Save is off while there are edits, said wherever Save is offered.
const HOTKEY_BLOCKS_SAVE: &str = "A hotkey needs fixing before you can save.";

/// What one Esc closes: the topmost surface only — a **popup**, else a
/// **modal card**, else the window.
#[derive(Debug, PartialEq, Eq)]
enum EscCloses {
    Popup,
    KeyDialog,
    ConfirmClear,
    UnsavedPrompt,
    Window,
}

/// egui 0.29's popup closes itself on Esc but reads the key without consuming
/// it, so the popup has to be asked about here or the same press closes the
/// window too (#94).
fn esc_closes(
    popup_open: bool,
    key_dialog_open: bool,
    confirm_clear_open: bool,
    unsaved_prompt_open: bool,
) -> EscCloses {
    if popup_open {
        EscCloses::Popup
    } else if key_dialog_open {
        EscCloses::KeyDialog
    } else if confirm_clear_open {
        EscCloses::ConfirmClear
    } else if unsaved_prompt_open {
        EscCloses::UnsavedPrompt
    } else {
        EscCloses::Window
    }
}

/// The one gate every way out of the window passes: the title bar's close
/// button raises a close request, and so do Esc and the footer's Close, which
/// send `ViewportCommand::Close` rather than exiting. A request made with
/// unsaved changes is cancelled, and `true` says to ask about them first —
/// unless `close_confirmed`, which is the prompt already answered.
fn hold_close(ctx: &egui::Context, dirty: bool, close_confirmed: bool) -> bool {
    let requested = ctx.input(|i| i.viewport().close_requested());
    let hold = requested && dirty && !close_confirmed;
    if hold {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    }
    hold
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
    form: Form,
    key_dialog: Option<KeyDialog>,
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
    /// True while the "Save changes?" modal is open: a close was asked for
    /// with edits that would be lost, and is held until the user picks.
    unsaved_prompt: bool,
    /// Set once that prompt is answered with Save or Discard, so the close it
    /// sends is let through rather than held again.
    close_confirmed: bool,
    /// Input device names enumerated at window open, for the microphone picker.
    input_devices: Vec<String>,
    /// Connected displays as (device path, label), enumerated at window open,
    /// for the pinned-display picker. The path is what gets stored; the label
    /// is only ever shown.
    displays: Vec<(String, String)>,
    rule_keys: RuleKeys,
    focus_trap: FocusTrap,
}

/// A stable key for each replacement rule's row, parallel to
/// `cfg.replacements`. The row's widgets take their ids from its key, not its
/// index, so removing a rule doesn't hand the rules below it their
/// predecessors' switch animations and text-edit state (#96). A key is never
/// reused, so a new rule can't inherit a removed one's state either.
#[derive(Debug, Default)]
struct RuleKeys {
    keys: Vec<u64>,
    next: u64,
}

impl RuleKeys {
    /// Match `len` rules: rules appended since the last call get fresh keys.
    fn fit(&mut self, len: usize) {
        while self.keys.len() < len {
            self.keys.push(self.next);
            self.next += 1;
        }
        self.keys.truncate(len);
    }

    fn key(&self, i: usize) -> u64 {
        self.keys[i]
    }

    /// Call alongside removing rule `i`.
    fn remove(&mut self, i: usize) {
        self.keys.remove(i);
    }
}

/// The editable list of replacement rules, each row under its rule's key.
fn replacement_rows(
    ui: &mut egui::Ui,
    rules: &mut Vec<crate::config::Replacement>,
    keys: &mut RuleKeys,
) {
    keys.fit(rules.len());
    // Edit in place; defer the structural removal until after the
    // borrow ends so we don't mutate the Vec mid-iteration.
    let mut remove: Option<usize> = None;
    for (i, rule) in rules.iter_mut().enumerate() {
        if i > 0 {
            divider(ui);
        }
        let key = keys.key(i);
        if ui
            .push_id(("repl_rule", key), |ui| replacement_editor(ui, rule, i + 1))
            .inner
        {
            remove = Some(i);
        }
    }
    if let Some(i) = remove {
        rules.remove(i);
        keys.remove(i);
    }
}

impl SettingsApp {
    fn save(&mut self) {
        self.save_status = Some(match self.form.save(&mut SystemStore) {
            Ok(()) => (true, "Saved. Applies when this window closes.".into()),
            Err(msg) => (false, msg),
        });
    }
}

// Dialog ids: each `modal_card`'s, and its focus trap's.
const KEY_DIALOG: &str = "key_dialog";
const CONFIRM_CLEAR: &str = "confirm_clear";
const UNSAVED_PROMPT: &str = "unsaved_prompt";

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.focus_trap.before_frame(raw_input);
    }
}

impl SettingsApp {
    /// The open dialog's id, if any — the one drawn last, should two be up.
    fn open_dialog(&self) -> Option<&'static str> {
        if self.unsaved_prompt {
            Some(UNSAVED_PROMPT)
        } else if self.confirm_clear_history {
            Some(CONFIRM_CLEAR)
        } else if self.key_dialog.is_some() {
            Some(KEY_DIALOG)
        } else {
            None
        }
    }

    /// One frame of the whole window.
    fn ui(&mut self, ctx: &egui::Context) {
        let (save_shortcut, close_shortcut) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if save_shortcut && self.open_dialog().is_none() && self.form.can_save() {
            self.save();
        }
        if close_shortcut {
            let popup_open = ctx.memory(|m| m.any_popup_open());
            match esc_closes(
                popup_open,
                self.key_dialog.is_some(),
                self.confirm_clear_history,
                self.unsaved_prompt,
            ) {
                EscCloses::Popup => ctx.memory_mut(|m| m.close_popup()),
                EscCloses::KeyDialog => self.key_dialog = None,
                EscCloses::ConfirmClear => self.confirm_clear_history = false,
                EscCloses::UnsavedPrompt => self.unsaved_prompt = false,
                EscCloses::Window => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
        if hold_close(ctx, self.form.is_dirty(), self.close_confirmed) {
            // The prompt replaces whatever modal was up: the window is on its
            // way out, and one question at a time.
            self.key_dialog = None;
            self.confirm_clear_history = false;
            self.unsaved_prompt = true;
        }

        // Behind an open dialog the window is inert — nothing there keeps
        // focus or hears Space/Enter — and `focus_trap` keeps Tab going round
        // the dialog's own controls.
        let inert = self.open_dialog().is_some();
        egui::SidePanel::left("rail")
            .resizable(false)
            .exact_width(RAIL_W)
            .frame(Frame::default().fill(SIDEBAR_BG).inner_margin(Margin {
                left: 12.0,
                right: 12.0,
                top: 18.0,
                bottom: 14.0,
            }))
            .show(ctx, |ui| inert_if(ui, inert, |ui| self.rail(ui)));

        // The footer is a strip of the central panel, laid out *after* the
        // pane, rather than a bottom panel of its own. egui's Tab order is the
        // order widgets are added, and a bottom panel has to be added before
        // the central one — which put Save and Close ahead of every control in
        // the pane (#99).
        egui::CentralPanel::default()
            .frame(Frame::default().fill(BG))
            .show(ctx, |ui| {
                inert_if(ui, inert, |ui| {
                    let full = ui.max_rect();
                    let (pane, footer) = full.split_top_bottom_at_y(full.bottom() - FOOTER_H);
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(pane), |ui| {
                        Frame::default()
                            .inner_margin(Margin {
                                left: 28.0,
                                // Small on purpose: the scroll bar should sit
                                // near the window edge, not float in the
                                // middle — `bar_inner_margin` already keeps it
                                // clear of the content.
                                right: 10.0,
                                top: 24.0,
                                bottom: 8.0,
                            })
                            .show(ui, |ui| self.pane(ui, ctx));
                    });
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(footer), |ui| {
                        Frame::default()
                            .inner_margin(Margin::symmetric(28.0, 0.0))
                            .show(ui, |ui| self.footer(ui, ctx));
                    });
                })
            });

        // Modals sit above everything when open.
        self.key_dialog_view(ctx);
        self.confirm_clear_view(ctx);
        self.unsaved_prompt_view(ctx);
        self.focus_trap.after_frame(ctx, self.open_dialog());
    }

    /// The selected tab's header and its scrolling rows.
    fn pane(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        pane_header(ui, self.tab);
        ui.add_space(20.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| match self.tab {
                Tab::Recording => self.tab_recording(ui),
                Tab::Pill => self.tab_pill(ui),
                Tab::Transcription => self.tab_transcription(ui, ctx),
                Tab::Replacements => self.tab_replacements(ui),
                Tab::Output => self.tab_output(ui),
                Tab::History => self.tab_history(ui),
                Tab::System => self.tab_system(ui),
            });
    }

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
        let errs = self.form.hotkey_errors();
        group(ui, |ui| {
            hotkey_row(
                ui,
                "Hotkey",
                "Push-to-talk key combination.",
                &mut self.form.cfg.hotkey,
                "Ctrl+Backslash",
                errs.dictate.as_deref(),
            );
            divider(ui);
            row(
                ui,
                "Microphone",
                "Which input device Draft records from.",
                |ui| {
                    let selected = self
                        .form
                        .cfg
                        .input_device
                        .clone()
                        .unwrap_or_else(|| "System default".into());
                    // "System default" (None) plus one option per enumerated device.
                    let mut options: Vec<(Option<String>, &str)> = vec![(None, "System default")];
                    options.extend(
                        self.input_devices
                            .iter()
                            .map(|n| (Some(n.clone()), n.as_str())),
                    );
                    combo(
                        ui,
                        "input_device",
                        &mut self.form.cfg.input_device,
                        &selected,
                        &options,
                    );
                },
            );
            divider(ui);
            row(ui, "Activation", "Hold the key, or tap to toggle.", |ui| {
                let sel = match self.form.cfg.activation {
                    Activation::Hold => "Hold",
                    Activation::Toggle => "Toggle",
                };
                combo(
                    ui,
                    "activation",
                    &mut self.form.cfg.activation,
                    sel,
                    &[(Activation::Hold, "Hold"), (Activation::Toggle, "Toggle")],
                );
            });
            // The double-press lock only applies in Hold mode — reveal it
            // progressively rather than showing a dead control in Toggle.
            if matches!(self.form.cfg.activation, Activation::Hold) {
                divider(ui);
                toggle_row(
                    ui,
                    &mut self.form.cfg.double_press_lock,
                    "Double-press to lock",
                    "Tap the hotkey twice quickly to keep recording hands-free.",
                );
            }
        });

        ui.add_space(14.0);

        group(ui, |ui| {
            toggle_row(
                ui,
                &mut self.form.cfg.push_to_command,
                "Push-to-command",
                "Hold a second hotkey and speak an instruction instead of \
                 dictating — the AI's answer is pasted at your cursor. \
                 Uses Groq; add its API key under Transcription.",
            );
            if self.form.cfg.push_to_command {
                divider(ui);
                hotkey_row(
                    ui,
                    "Command hotkey",
                    "Same syntax as the main hotkey.",
                    &mut self.form.cfg.command_hotkey,
                    "Ctrl+Shift+Backslash",
                    errs.command.as_deref(),
                );
            }
        });
    }

    /// The pill pane.
    ///
    /// Fullscreen suppression (#45) deliberately has no row: getting out of the
    /// way of a game is not a preference, and a pill kept over one would be
    /// paying compositor watts for a setting nobody wants.
    fn tab_pill(&mut self, ui: &mut egui::Ui) {
        group(ui, |ui| {
            toggle_row(
                ui,
                &mut self.form.cfg.pill.resident,
                "Keep the pill on screen",
                "A small marker sits at the bottom of your screen whenever Draft \
                 is running, so you can tell at a glance that it's alive. Turn \
                 this off and the pill only appears while you're dictating — the \
                 hotkey works exactly the same either way.",
            );
            // Gated on residency, where the monitor row below is deliberately
            // not: the button bar only ever appears on hover, and there is
            // nothing to hover with the pill off. Same test, opposite answer,
            // because the question is whether the setting is still in force.
            if self.form.cfg.pill.resident {
                divider(ui);
                self.body_style_row(ui);
            }
            // Deliberately *not* gated on residency: the same policy places the
            // session-only pill, so hiding this row when the pill is off would
            // hide a setting that is still in force.
            divider(ui);
            row(
                ui,
                "Show it on",
                "Which display the pill appears on.",
                |ui| {
                    let sel = self.form.cfg.pill.monitor.label();
                    let options: Vec<(MonitorPolicy, &str)> = ALL_MONITOR_POLICIES
                        .iter()
                        .map(|&p| (p, p.label()))
                        .collect();
                    combo(
                        ui,
                        "pill_monitor",
                        &mut self.form.cfg.pill.monitor,
                        sel,
                        &options,
                    );
                },
            );
            // Progressive reveal, as with the double-press lock: the picker
            // means nothing under the other three policies.
            if self.form.cfg.pill.monitor == MonitorPolicy::Pinned {
                divider(ui);
                self.pinned_display_row(ui);
            }
        });
    }

    /// The body-style toggle: three shapes, or one bar.
    ///
    /// A toggle over a `PillBodyStyle`, via a local `bool` — `toggle_row` binds
    /// a flag and this is a two-state choice, so the round trip is the whole of
    /// the adaptation. The config keeps the named states because a file that
    /// says `body_style = "unified"` reads better than one that says a flag is
    /// true.
    ///
    /// Off is islands, and off is the default: the toggle adds the reduced
    /// option rather than choosing between two peers.
    fn body_style_row(&mut self, ui: &mut egui::Ui) {
        let mut unified = self.form.cfg.pill.body_style == PillBodyStyle::Unified;
        toggle_row(
            ui,
            &mut unified,
            "Put the buttons in one bar",
            "When you point at the pill it opens into buttons — normally three \
             separate shapes with your desktop showing between them. Turn this \
             on to put them in a single bar instead. Same buttons, same \
             actions, just a different shape.",
        );
        self.form.cfg.pill.body_style = if unified {
            PillBodyStyle::Unified
        } else {
            PillBodyStyle::Islands
        };
    }

    /// The display picker, shown only when the policy is "a specific display".
    ///
    /// Values are EDID-derived device paths, never `\\.\DISPLAY1` — a GDI
    /// adapter slot is reassigned on replug, so a pin stored that way rots
    /// silently. The trade is that the label goes stale visibly instead: a
    /// pinned display that isn't connected right now says so, and the pill
    /// falls back to the primary until it comes back.
    fn pinned_display_row(&mut self, ui: &mut egui::Ui) {
        let pinned = self.form.cfg.pill.monitor_pinned_path.clone();
        let connected = self
            .displays
            .iter()
            .find(|(path, _)| Some(path.as_str()) == pinned.as_deref());
        let selected = match (&pinned, connected) {
            (_, Some((_, label))) => label.clone(),
            (Some(_), None) => "Not connected".to_string(),
            (None, None) => "Choose a display…".to_string(),
        };
        row(ui, "Display", "Pinned by the monitor itself, not by its slot — reordering your displays won't move the pill.", |ui| {
            let options: Vec<(Option<String>, &str)> = self
                .displays
                .iter()
                .map(|(path, label)| (Some(path.clone()), label.as_str()))
                .collect();
            combo(
                ui,
                "pill_monitor_pinned",
                &mut self.form.cfg.pill.monitor_pinned_path,
                &selected,
                &options,
            );
        });
    }

    fn tab_transcription(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        group(ui, |ui| {
            row(ui, "Provider", "Where your audio is transcribed.", |ui| {
                let sel = self.form.cfg.provider.label();
                let options: Vec<(Provider, &str)> =
                    ALL_PROVIDERS.iter().map(|&p| (p, p.label())).collect();
                combo(ui, "provider", &mut self.form.cfg.provider, sel, &options);
            });

            if secrets::slot_name(self.form.cfg.provider).is_some() {
                divider(ui);
                let configured = !self.form.keys.get(self.form.cfg.provider).trim().is_empty();
                let provider = self.form.cfg.provider;
                row(
                    ui,
                    "API key",
                    "Stored in Windows Credential Manager.",
                    |ui| {
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
                    },
                );
            }

            if matches!(self.form.cfg.provider, Provider::LocalParakeet) {
                divider(ui);
                self.parakeet_row(ui, ctx);
            } else {
                divider(ui);
                toggle_row(
                    ui,
                    &mut self.form.cfg.fallback_to_local,
                    "Offline fallback",
                    "If the cloud call fails, the local model transcribes \
                     instead, so the dictation isn't lost. Requires the \
                     downloaded Parakeet model.",
                );
            }
        });
    }

    fn parakeet_row(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut state = download::lock(&self.download_state);
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
                        .map(|p| (progress_label(p), progress_fraction(p)))
                        .unwrap_or_else(|| ("Starting download…".into(), 0.0));
                    let bar = ui.add(
                        egui::ProgressBar::new(pct)
                            .desired_width(CONTROL_W)
                            .desired_height(6.0)
                            .rounding(Rounding::same(3.0))
                            .fill(PRIMARY),
                    );
                    announce_progress(&bar, "Downloading local model", pct, &label);
                    ui.add_space(5.0);
                    ui.label(RichText::new(label).size(11.5).color(MUTED_FG).monospace());
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                } else {
                    if ui
                        .add(ghost_button(
                            "Download model (~670 MB)",
                            CONTROL_W,
                            CONTROL_H,
                        ))
                        .clicked()
                    {
                        state.start();
                        let repaint_ctx = ctx.clone();
                        download::spawn(self.download_state.clone(), move || {
                            repaint_ctx.request_repaint()
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

            if self.form.cfg.replacements.is_empty() {
                ui.label(
                    RichText::new("No rules yet.")
                        .size(12.5)
                        .color(MUTED_FG)
                        .italics(),
                );
                ui.add_space(12.0);
            }

            replacement_rows(ui, &mut self.form.cfg.replacements, &mut self.rule_keys);

            if !self.form.cfg.replacements.is_empty() {
                ui.add_space(16.0);
            }
            if ui
                .add(ghost_button("Add replacement", 160.0, CONTROL_H))
                .clicked()
            {
                self.form
                    .cfg
                    .replacements
                    .push(crate::config::Replacement::default());
            }
        });

        ui.add_space(14.0);

        group(ui, |ui| {
            ui.label(RichText::new("Custom vocabulary").size(13.5).color(FG));
            ui.add_space(2.0);
            ui.label(
                RichText::new(
                    "Words your provider keeps mishearing — names, jargon, \
                     product terms. One per line, up to 100; put the ones \
                     that matter most first. The local model ignores these, \
                     so use a replacement rule there instead.",
                )
                .size(11.5)
                .color(MUTED_FG),
            );
            ui.add_space(10.0);
            let resp = ui.add(
                egui::TextEdit::multiline(&mut self.form.vocab_buffer)
                    .desired_rows(5)
                    .desired_width(f32::INFINITY)
                    .hint_text(hint("e.g.  Janssen\n      kubectl\n      Reson8")),
            );
            name_control(&resp, "Custom vocabulary");
            if resp.changed() {
                self.form.vocabulary_edited();
            }
            if let Some(caption) = self.form.vocabulary_count().caption() {
                ui.add_space(6.0);
                ui.label(RichText::new(caption).size(11.5).color(MUTED_FG));
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
                    let sel = match self.form.cfg.paste_mode {
                        PasteMode::Clipboard => "Clipboard (Ctrl+V)",
                        PasteMode::Unicode => "Type (Unicode)",
                    };
                    combo(
                        ui,
                        "paste_mode",
                        &mut self.form.cfg.paste_mode,
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
                &mut self.form.cfg.append_trailing_space,
                "Append trailing space",
                "Adds one space after each transcript so the next word doesn't smash into it.",
            );
            divider(ui);
            toggle_row(
                ui,
                &mut self.form.cfg.restore_clipboard,
                "Restore clipboard",
                "Put your previous clipboard back after pasting.",
            );
            divider(ui);
            toggle_row(
                ui,
                &mut self.form.cfg.voice_commands,
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
            let search = ui.add_sized(
                [ui.available_width(), CONTROL_H],
                egui::TextEdit::singleline(&mut self.history_filter)
                    .hint_text(hint("Search transcripts…"))
                    .vertical_align(egui::Align::Center),
            );
            name_control(&search, "Search transcripts");
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
                        let resp = ui.add(ghost_button("Copy", copy_w, 28.0));
                        // Every entry's button reads "Copy"; say whose it is.
                        name_control(
                            &resp,
                            &format!("Copy transcript, {}", relative_time(now, entry.ts)),
                        );
                        if resp.clicked() {
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
                &mut self.form.autostart_enabled,
                "Start with Windows",
                "Launches Draft automatically on sign-in (HKCU registry entry).",
            );
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Top hairline so the footer reads as a distinct bar.
        ui.painter()
            .hline(ui.max_rect().x_range(), ui.max_rect().top(), border());
        let dirty = self.form.is_dirty();
        let can_save = self.form.can_save();
        let size = Vec2::new(ui.available_width(), ui.available_height());
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let save = ui.add(primary_button("Save", can_save));
                if save.clicked() && can_save {
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
                    // Dirty but unsaveable: the field at fault may be on a
                    // pane the user has since left, so say why Save is off.
                    (_, true) if !can_save => {
                        ui.label(
                            RichText::new(HOTKEY_BLOCKS_SAVE)
                                .size(12.0)
                                .color(DESTRUCTIVE),
                        );
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
            let configured = !self.form.keys.get(dlg.provider).trim().is_empty();
            let scrim_clicked = modal_card(ctx, KEY_DIALOG, |ui| {
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
                    let field = ui.add_sized(
                        [field_w, CONTROL_H],
                        egui::TextEdit::singleline(&mut dlg.buffer)
                            .password(!dlg.reveal)
                            .hint_text(hint("paste key…"))
                            .vertical_align(egui::Align::Center),
                    );
                    name_control(&field, &format!("{} API key", dlg.provider.label()));
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
                self.form.keys.set(p, buf);
                self.key_dialog = None;
            }
            Act::Remove(p) => {
                self.form.keys.set(p, String::new());
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
        let scrim_clicked = modal_card(ctx, CONFIRM_CLEAR, |ui| {
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

    /// "Save changes?" on the way out: save and close, discard and close, or
    /// stay. The scrim, Cancel, and Esc all stay. Save is off for the same
    /// reason the footer's is, and the message says so.
    fn unsaved_prompt_view(&mut self, ctx: &egui::Context) {
        if !self.unsaved_prompt {
            return;
        }
        enum Act {
            None,
            Save,
            Discard,
            Cancel,
        }
        let mut act = Act::None;
        let can_save = self.form.can_save();
        let scrim_clicked = modal_card(ctx, UNSAVED_PROMPT, |ui| {
            ui.label(
                RichText::new("Save changes before closing?")
                    .size(15.0)
                    .strong()
                    .color(FG),
            );
            ui.add_space(4.0);
            let msg = if can_save {
                "Your changes will be lost if you don't save them.".to_string()
            } else {
                format!("{HOTKEY_BLOCKS_SAVE} Discard closes without your changes.")
            };
            ui.label(RichText::new(msg).size(12.0).color(MUTED_FG));
            ui.add_space(18.0);
            // Pin the row height, as in the clear-history confirmation.
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), 34.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if ui.add(primary_button("Save", can_save)).clicked() && can_save {
                        act = Act::Save;
                    }
                    ui.add_space(8.0);
                    if ui.add(ghost_button("Discard", 84.0, 34.0)).clicked() {
                        act = Act::Discard;
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
            Act::Save => {
                self.unsaved_prompt = false;
                self.save();
                // A failed save stays open with the footer saying why, rather
                // than closing on edits that never reached disk.
                if !self.form.is_dirty() {
                    self.close_confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Act::Discard => {
                self.unsaved_prompt = false;
                self.close_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Act::Cancel => self.unsaved_prompt = false,
            Act::None => {}
        }
    }
}

// ---- pane chrome -------------------------------------------------------

/// A hotkey row, flagged against the parser the main process registers with:
/// the field turns red and the message hangs beneath the row. `error` is as
/// of the start of this frame, so an edit asks for one more — the flag then
/// catches up with what was just typed.
fn hotkey_row(
    ui: &mut egui::Ui,
    label: &str,
    caption: &str,
    spec: &mut String,
    placeholder: &str,
    error: Option<&str>,
) {
    row(ui, label, caption, |ui| {
        if text_input(ui, spec, placeholder, CONTROL_W, error).changed() {
            ui.ctx().request_repaint();
        }
    });
    if let Some(msg) = error {
        field_error(ui, msg);
    }
}

fn pane_header(ui: &mut egui::Ui, tab: Tab) {
    ui.label(RichText::new(tab.label()).size(21.0).strong().color(FG));
    ui.add_space(3.0);
    ui.label(RichText::new(tab.subtitle()).size(12.5).color(MUTED_FG));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_with_a_modal_card_open_closes_only_the_modal() {
        assert_eq!(esc_closes(false, true, false, false), EscCloses::KeyDialog);
        assert_eq!(
            esc_closes(false, false, true, false),
            EscCloses::ConfirmClear
        );
    }

    #[test]
    fn esc_with_nothing_open_closes_the_window() {
        assert_eq!(esc_closes(false, false, false, false), EscCloses::Window);
    }

    #[test]
    fn esc_with_the_unsaved_changes_prompt_open_stays() {
        assert_eq!(
            esc_closes(false, false, false, true),
            EscCloses::UnsavedPrompt
        );
    }

    /// A frame whose input carries a close request for the window — what both
    /// the title bar's close button and our own `ViewportCommand::Close` produce.
    fn close_request() -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                events: vec![egui::ViewportEvent::Close],
                ..Default::default()
            },
        );
        input
    }

    /// Runs one frame of `hold_close` and reports whether it held, and whether
    /// the window was told to stay open.
    fn run_hold_close(input: egui::RawInput, dirty: bool, close_confirmed: bool) -> (bool, bool) {
        let ctx = egui::Context::default();
        let mut held = false;
        let out = ctx.run(input, |ctx| held = hold_close(ctx, dirty, close_confirmed));
        let cancelled = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|v| v.commands.contains(&egui::ViewportCommand::CancelClose));
        (held, cancelled)
    }

    #[test]
    fn closing_with_no_unsaved_changes_closes_immediately() {
        assert_eq!(
            run_hold_close(close_request(), false, false),
            (false, false)
        );
    }

    #[test]
    fn closing_with_unsaved_changes_is_held_for_the_prompt() {
        assert_eq!(run_hold_close(close_request(), true, false), (true, true));
    }

    /// Save-and-close and Discard both answer the prompt by closing again; that
    /// second request must go through even though the edits are still there
    /// (Discard) or a save failed to clear them.
    #[test]
    fn closing_after_the_prompt_is_answered_is_not_held_again() {
        assert_eq!(run_hold_close(close_request(), true, true), (false, false));
    }

    #[test]
    fn no_close_request_holds_nothing() {
        assert_eq!(
            run_hold_close(egui::RawInput::default(), true, false),
            (false, false)
        );
    }

    /// The bug behind #94: egui 0.29's popup reads Esc without consuming it,
    /// so the popup opened on an earlier frame must still read as open at the
    /// top of the frame that carries the Esc — which is where `update` asks.
    /// This pins an egui assumption; revisit it on an egui bump.
    #[test]
    fn a_popup_opened_last_frame_is_still_open_when_esc_arrives() {
        let ctx = egui::Context::default();
        let popup = egui::Id::new("popup");
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.memory_mut(|m| m.open_popup(popup));
        });
        let esc = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut seen = None;
        let _ = ctx.run(esc, |ctx| {
            let popup_open = ctx.memory(|m| m.any_popup_open());
            seen = Some(esc_closes(popup_open, false, false, false));
        });
        assert_eq!(seen, Some(EscCloses::Popup));
    }

    fn rule(from: &str, enabled: bool) -> crate::config::Replacement {
        crate::config::Replacement {
            from: from.into(),
            enabled,
            ..Default::default()
        }
    }

    /// A rule added after a removal must not inherit the removed rule's
    /// state, so its key is new rather than the one that was freed.
    #[test]
    fn a_new_rule_never_reuses_a_removed_rules_key() {
        let mut keys = RuleKeys::default();
        keys.fit(2);
        let gone = keys.key(1);
        keys.remove(1);
        keys.fit(2);
        assert_ne!(keys.key(1), gone);
    }

    /// Runs one frame of the rule list with `events` as its input, and says
    /// whether anything on it is still animating — which is what a switch
    /// inheriting another rule's on/off state looks like.
    fn rules_frame_animates(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        rules: &mut Vec<crate::config::Replacement>,
        keys: &mut RuleKeys,
    ) -> bool {
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| replacement_rows(ui, rules, keys));
        });
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay == std::time::Duration::ZERO
    }

    /// #96: the rules below a removed one used to take over its row's ids,
    /// so an off rule's switch slid on when an on rule moved into its place.
    #[test]
    fn removing_a_rule_leaves_the_switches_below_it_still() {
        let ctx = egui::Context::default();
        let mut rules = vec![rule("a", false), rule("b", true), rule("c", false)];
        let mut keys = RuleKeys::default();
        rules_frame_animates(&ctx, vec![], &mut rules, &mut keys);
        assert!(
            !rules_frame_animates(&ctx, vec![], &mut rules, &mut keys),
            "settled"
        );

        rules.remove(0);
        keys.remove(0);
        assert!(!rules_frame_animates(&ctx, vec![], &mut rules, &mut keys));
    }

    /// Focus and cursor live in egui memory under the field's id, so typing
    /// lands in the right rule only if that id stays with its rule.
    #[test]
    fn a_focused_field_stays_with_its_rule_when_an_earlier_rule_is_removed() {
        let ctx = egui::Context::default();
        let mut rules = vec![rule("a", true), rule("b", true)];
        let mut keys = RuleKeys::default();
        rules_frame_animates(&ctx, vec![], &mut rules, &mut keys);

        // Tab to the third text field: a's "hears", a's "writes", b's "hears".
        let tab = egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let is_field = |id: egui::Id| egui::TextEdit::load_state(&ctx, id).is_some();
        let (mut fields, mut last) = (0, None);
        for _ in 0..32 {
            rules_frame_animates(&ctx, vec![tab.clone()], &mut rules, &mut keys);
            let now = ctx.memory(|m| m.focused());
            if now != last && now.is_some_and(is_field) {
                fields += 1;
                if fields == 3 {
                    break;
                }
            }
            last = now;
        }
        assert_eq!(fields, 3, "tabbing reached b's field");

        let typed = |s: &str| vec![egui::Event::Text(s.into())];
        rules_frame_animates(&ctx, typed("1"), &mut rules, &mut keys);
        assert_eq!(rules[1].from, "b1");

        rules.remove(0);
        keys.remove(0);
        rules_frame_animates(&ctx, vec![], &mut rules, &mut keys);
        rules_frame_animates(&ctx, typed("2"), &mut rules, &mut keys);
        assert_eq!(rules[0].from, "b12");
    }

    /// A window with nothing loaded from the machine: default config, no
    /// keys, no history, no devices.
    fn blank_app(tab: Tab) -> SettingsApp {
        SettingsApp {
            tab,
            form: Form::new(Config::default(), [], false),
            key_dialog: None,
            save_status: None,
            download_state: Arc::new(Mutex::new(DownloadState::new(false))),
            history: Vec::new(),
            history_filter: String::new(),
            confirm_clear_history: false,
            unsaved_prompt: false,
            close_confirmed: false,
            input_devices: Vec::new(),
            displays: Vec::new(),
            rule_keys: RuleKeys::default(),
            focus_trap: FocusTrap::default(),
        }
    }

    const SCREEN: egui::Rect = egui::Rect {
        min: egui::Pos2::ZERO,
        max: egui::pos2(760.0, 560.0),
    };

    /// One frame of the whole window at its default size.
    fn frame(
        ctx: &egui::Context,
        app: &mut SettingsApp,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut input = egui::RawInput {
            screen_rect: Some(SCREEN),
            max_texture_side: Some(2048),
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(app, ctx, &mut input);
        ctx.run(input, |ctx| app.ui(ctx))
    }

    /// Tab (Shift+Tab with `back`), then the frames the app asks for straight
    /// after — a deferred focus move, a replayed press — as eframe would run
    /// them.
    fn tab(ctx: &egui::Context, app: &mut SettingsApp, back: bool) {
        let modifiers = if back {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        let press = egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        frame(ctx, app, vec![press]);
        for _ in 0..3 {
            frame(ctx, app, vec![]);
        }
    }

    fn press(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    fn styled_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        install_style(&ctx);
        ctx
    }

    fn focused_rect(ctx: &egui::Context) -> Option<egui::Rect> {
        let id = ctx.memory(|m| m.focused())?;
        Some(ctx.read_response(id).expect("focused widget").rect)
    }

    /// #99: Tab reads the window the way a person does — down the rail, then
    /// the pane, then the footer — not rail, footer, pane, which is the order
    /// the panels have to be *laid out* in.
    #[test]
    fn tab_reads_the_window_rail_then_pane_then_footer() {
        let ctx = styled_ctx();
        let mut app = blank_app(Tab::System);
        frame(&ctx, &mut app, vec![]);

        let mut regions = Vec::new();
        // Seven rail items, System's one toggle, then Close — Save is off,
        // with nothing to save.
        for _ in 0..9 {
            tab(&ctx, &mut app, false);
            let r = focused_rect(&ctx).expect("tab focuses something");
            regions.push(if r.right() <= RAIL_W {
                "rail"
            } else if r.top() >= SCREEN.bottom() - FOOTER_H {
                "footer"
            } else {
                "pane"
            });
        }
        let mut want = vec!["rail"; 7];
        want.extend(["pane", "footer"]);
        assert_eq!(regions, want);
    }

    /// While a dialog is up, Tab goes round its own buttons and nothing else:
    /// the rail, pane and footer behind the scrim are out of reach, and every
    /// press lands on a control — none is lost on the way round.
    #[test]
    fn tab_stays_inside_an_open_dialog() {
        let card = SCREEN.shrink2(Vec2::new(180.0, 150.0));
        for back in [false, true] {
            let ctx = styled_ctx();
            let mut app = blank_app(Tab::System);
            app.confirm_clear_history = true;
            frame(&ctx, &mut app, vec![]);
            frame(&ctx, &mut app, vec![]);

            let mut stops = Vec::new();
            for press in 1..=6 {
                tab(&ctx, &mut app, back);
                let r = focused_rect(&ctx)
                    .unwrap_or_else(|| panic!("press {press} (back: {back}) focused nothing"));
                assert!(
                    card.contains_rect(r) && r.area() > 0.0,
                    "press {press} (back: {back}) left the dialog's controls for {r:?}"
                );
                stops.push(r);
            }
            // Clear and Cancel, turn about, whichever way round.
            assert_ne!(stops[0], stops[1], "back: {back}");
            for i in 2..stops.len() {
                assert_eq!(stops[i], stops[i - 2], "back: {back}, press {}", i + 1);
            }
        }
    }

    /// The API-key dialog's text field is in the round too, and typing lands
    /// in it — the brackets either side of the card never keep focus.
    #[test]
    fn tab_goes_round_the_key_dialog_field_included() {
        let ctx = styled_ctx();
        let mut app = blank_app(Tab::Transcription);
        app.key_dialog = Some(KeyDialog {
            provider: Provider::Groq,
            buffer: String::new(),
            reveal: false,
        });
        frame(&ctx, &mut app, vec![]);
        frame(&ctx, &mut app, vec![]);

        // The field, Show, Cancel — Save is off with the field empty, and
        // there's no Remove with no key saved.
        let mut stops = Vec::new();
        for _ in 0..6 {
            tab(&ctx, &mut app, false);
            stops.push(focused_rect(&ctx).expect("every Tab lands on something"));
        }
        assert!(stops[..3].iter().all(|r| r.area() > 0.0));
        assert_ne!(stops[0], stops[1]);
        assert_ne!(stops[1], stops[2]);
        assert_eq!(stops[..3], stops[3..]);

        // Round to the field again, and type.
        tab(&ctx, &mut app, false);
        frame(&ctx, &mut app, vec![egui::Event::Text("gsk_".into())]);
        let dlg = app.key_dialog.as_ref().expect("still open");
        assert_eq!(dlg.buffer, "gsk_");
    }

    /// A control that had focus when the dialog opened loses it: Space then
    /// does nothing behind the scrim.
    #[test]
    fn space_does_nothing_behind_an_open_dialog() {
        let ctx = styled_ctx();
        let mut app = blank_app(Tab::System);
        frame(&ctx, &mut app, vec![]);
        // Seven rail items, then System's launch-at-login toggle.
        for _ in 0..8 {
            tab(&ctx, &mut app, false);
        }
        let r = focused_rect(&ctx).expect("the toggle has focus");
        assert!(r.left() > RAIL_W && r.bottom() < SCREEN.bottom() - FOOTER_H);

        app.confirm_clear_history = true;
        frame(&ctx, &mut app, vec![]);
        frame(&ctx, &mut app, press(egui::Key::Space));
        assert!(
            !app.form.autostart_enabled,
            "the toggle behind the dialog flipped"
        );
        assert!(app.confirm_clear_history, "the dialog is still up");
    }

    /// Making the window inert doesn't grey it out: behind the scrim it paints
    /// exactly what it painted before the dialog opened. (`Ui::disable` and
    /// `add_enabled_ui` would fade it.)
    #[test]
    fn a_dialog_leaves_the_window_behind_it_as_it_was() {
        let ctx = styled_ctx();
        let mut app = blank_app(Tab::Recording);
        frame(&ctx, &mut app, vec![]);
        let before = frame(&ctx, &mut app, vec![]).shapes;

        app.confirm_clear_history = true;
        frame(&ctx, &mut app, vec![]);
        let during = frame(&ctx, &mut app, vec![]).shapes;
        // The window's own layer paints first; the scrim and card follow.
        assert!(during.len() > before.len(), "the dialog paints something");
        assert!(
            during[..before.len()] == before[..],
            "the window behind the dialog painted differently"
        );
    }
}
