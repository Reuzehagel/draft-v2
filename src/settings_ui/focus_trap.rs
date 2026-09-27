// Keeps Tab inside an open dialog.
//
// egui 0.29 has no modal focus scope: a dialog is an Area over the panels, and
// Tab walks every focusable widget in the order it was added, panels included.
// `widgets::inert_if` puts the panels out of reach — a disabled widget can't
// keep focus, so Space/Enter never land there — but not out of the Tab order:
// egui still hands the next Tab to a disabled widget and then takes it straight
// back, and the press is lost.
//
// So a dialog's card is bracketed by two invisible focus stops (`ends`,
// placed by `stop`), and after each frame `FocusTrap` looks at where Tab left
// focus. On a bracket — or nowhere, after a press — it parks focus on the
// bracket the move comes *from* and replays the press into the next frame's
// input, where egui's own Tab takes the one step onto the card's first control
// going forward, or its last going back. Nothing ever rests on a bracket.

/// Which way a Tab press moves focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    Forward,
    Back,
}

impl Dir {
    /// The last Tab pressed this frame, if any.
    fn pressed(ctx: &egui::Context) -> Option<Self> {
        ctx.input(|i| {
            i.events.iter().rev().find_map(|e| match e {
                egui::Event::Key {
                    key: egui::Key::Tab,
                    pressed: true,
                    modifiers,
                    ..
                } => Some(if modifiers.shift {
                    Self::Back
                } else {
                    Self::Forward
                }),
                _ => None,
            })
        })
    }

    fn event(self) -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: if self == Self::Back {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            },
        }
    }
}

/// The two focus stops bracketing dialog `dialog`'s card: before its first
/// control and after its last.
pub(super) fn ends(dialog: &str) -> [egui::Id; 2] {
    [
        egui::Id::new((dialog, "focus_trap", "start")),
        egui::Id::new((dialog, "focus_trap", "end")),
    ]
}

/// A bracket at the cursor: focusable, but zero-sized, unpainted, and deaf to
/// clicks — it allocates nothing, so the card lays out as if it weren't there.
pub(super) fn stop(ui: &egui::Ui, id: egui::Id) {
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::ZERO);
    ui.interact(rect, id, egui::Sense::focusable_noninteractive());
}

#[derive(Debug, Default)]
pub(super) struct FocusTrap {
    /// The latest Tab press while a dialog has been open. A Shift+Tab moves
    /// focus a frame late, so a bracket can be reached a frame after the
    /// press that sent focus there.
    last: Option<Dir>,
    /// A press to feed into the next frame.
    replay: Option<Dir>,
    /// This frame's input carries a replayed press.
    replaying: bool,
}

impl FocusTrap {
    /// Before a frame: feed in the press the last frame asked to replay.
    pub(super) fn before_frame(&mut self, input: &mut egui::RawInput) {
        if let Some(dir) = self.replay.take() {
            input.events.push(dir.event());
            self.replaying = true;
        }
    }

    /// After a frame, dialogs drawn: `dialog` is the open one, if any.
    pub(super) fn after_frame(&mut self, ctx: &egui::Context, dialog: Option<&str>) {
        let replayed = std::mem::take(&mut self.replaying);
        let Some(dialog) = dialog else {
            self.last = None;
            return;
        };
        let pressed = Dir::pressed(ctx);
        if pressed.is_some() {
            // A move egui defers to the next frame (Shift+Tab) needs one.
            ctx.request_repaint();
        }
        self.last = pressed.or(self.last);
        // A replay that falls short doesn't get another: a card with nothing
        // focusable in it would bounce between its brackets for ever.
        let (Some(dir), false) = (self.last, replayed) else {
            return;
        };
        let [start, end] = ends(dialog);
        match ctx.memory(|m| m.focused()) {
            Some(f) if f == start || f == end => {}
            None if pressed.is_some() => {}
            _ => return,
        }
        let park = match dir {
            Dir::Forward => start,
            Dir::Back => end,
        };
        ctx.memory_mut(|m| m.request_focus(park));
        self.replay = Some(dir);
        ctx.request_repaint();
    }
}
