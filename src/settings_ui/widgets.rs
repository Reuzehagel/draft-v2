// Reusable widgets for the settings window. Pure functions over egui — no
// app state in here; anything stateful stays in mod.rs.
//
// Layout invariants that keep this UI from regressing:
// - Measure, then allocate. Text that wraps next to other content is laid
//   out as a galley against the width that is actually free for it (see
//   `toggle_row`); never a fixed-height row with full-width wrapping text.
// - A `right_to_left` layout expands to fill whatever rect it is given.
//   Only open one inside something already bounded (a `horizontal`, or an
//   `allocate_ui_with_layout` with a pinned size) or it balloons the parent.
// - Every control is CONTROL_W wide and right-aligned, so a pane reads as
//   two clean columns.
// - A galley from `layout_no_wrap` is free to be wider than the rect it is
//   painted into, so text whose length isn't ours to control (device names)
//   is laid out with `elided_galley` against the width actually free for it.
//   The dropdown popup keeps the button's CONTROL_W at every window size;
//   it is never widened to fit content.

use super::theme::*;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, Frame, Margin, RichText, Rounding, Stroke, Vec2};

/// Lay text out on one line, elided with a trailing `…` when it doesn't fit
/// `max_width`. The galley never measures wider than `max_width`, so a caller
/// painting it into a rect of that width paints nothing outside it — which a
/// `layout_no_wrap` galley is free to do. `galley.elided` says whether a tail
/// was lost; callers use it to decide whether the full text is worth a tooltip.
fn elided_galley(
    ctx: &egui::Context,
    text: &str,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping::truncate_at_width(max_width.max(0.0));
    ctx.fonts(|f| f.layout_job(job))
}

/// A flush group of rows constrained to a readable column width.
pub(super) fn group<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
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
pub(super) fn nav_item(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
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

    let color = if selected || resp.hovered() {
        FG
    } else {
        MUTED_FG
    };
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
pub(super) fn combo<T: PartialEq + Clone>(
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

/// Left inset of an option row's label.
const COMBO_ITEM_INSET: f32 = 10.0;
/// Where the checkmark is centred, measured in from the row's right edge.
const COMBO_ITEM_CHECK_INSET: f32 = 16.0;
/// How far left of its centre the checkmark reaches.
const COMBO_ITEM_CHECK_REACH: f32 = 4.0;
/// Trailing space an option row keeps clear for the checkmark: out to the
/// check's leading edge plus a gap. Reserved on every row, selected or not, so
/// a label sits at the same width wherever the selection is and can never run
/// under the check.
const COMBO_ITEM_CHECK_W: f32 = COMBO_ITEM_CHECK_INSET + COMBO_ITEM_CHECK_REACH + 8.0;

/// The width a `row_w`-wide option row leaves for its label.
fn combo_item_text_w(row_w: f32) -> f32 {
    row_w - COMBO_ITEM_INSET - COMBO_ITEM_CHECK_W
}

/// One option row in a dropdown: hover fill + a trailing checkmark when it's
/// the current value.
///
/// Layout invariant: the popup is CONTROL_W wide whatever the labels are, so a
/// label that doesn't fit is elided — device names arrive from Windows at any
/// length and used to draw straight past the popup edge. An elided row carries
/// the full label as a hover tooltip; a row that fits carries none, so the
/// tooltip itself is the signal that there is more text.
pub(super) fn combo_item(ui: &mut egui::Ui, text: &str, selected: bool) -> bool {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, Rounding::same(6.0), SELECTED_BG);
    }
    let text_w = combo_item_text_w(rect.width());
    let galley = elided_galley(ui.ctx(), text, egui::FontId::proportional(13.0), FG, text_w);
    let elided = galley.elided;
    let pos = egui::pos2(
        rect.left() + COMBO_ITEM_INSET,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
    let resp = if elided {
        resp.on_hover_text(text)
    } else {
        resp
    };

    if selected {
        // Hand-drawn check so it doesn't depend on glyph coverage.
        let cy = rect.center().y;
        let cx = rect.right() - COMBO_ITEM_CHECK_INSET;
        ui.painter().add(egui::Shape::line(
            vec![
                egui::pos2(cx - COMBO_ITEM_CHECK_REACH, cy + 0.5),
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
pub(super) fn key_opener(ui: &mut egui::Ui, configured: bool) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(CONTROL_W, CONTROL_H), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if resp.hovered() {
        CONTROL_HOVER
    } else {
        CONTROL_FILL
    };
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
    let g =
        ui.painter()
            .layout_no_wrap(status.to_string(), egui::FontId::proportional(13.0), scolor);
    let gy = rect.center().y - g.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(x, gy), g, Color32::PLACEHOLDER);

    let action = if configured { "Change" } else { "Set" };
    let ga = ui.painter().layout_no_wrap(
        action.to_string(),
        egui::FontId::proportional(12.5),
        MUTED_FG,
    );
    let ax = rect.right() - 12.0 - ga.size().x;
    ui.painter().galley(
        egui::pos2(ax, rect.center().y - ga.size().y / 2.0),
        ga,
        Color32::PLACEHOLDER,
    );

    resp.clicked()
}

// ---- rows --------------------------------------------------------------

/// Single-line text input forced to a fixed width and height so every input
/// in the window lines up.
pub(super) fn text_input(ui: &mut egui::Ui, text: &mut String, placeholder: &str, width: f32) {
    ui.add_sized(
        [width, CONTROL_H],
        egui::TextEdit::singleline(text)
            .hint_text(hint(placeholder))
            .vertical_align(egui::Align::Center),
    );
}

/// One label/value row. Label column is capped so long captions can't slide
/// under the control on the right.
pub(super) fn row(
    ui: &mut egui::Ui,
    label: &str,
    caption: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
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
pub(super) fn split_row(
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
pub(super) fn replacement_editor(
    ui: &mut egui::Ui,
    idx: usize,
    rule: &mut crate::config::Replacement,
) -> bool {
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
                .hint_text(hint("hears…"))
                .vertical_align(egui::Align::Center),
        );
        ui.add_space(8.0);
        ui.label(RichText::new("→").size(15.0).color(MUTED_FG));
        ui.add_space(8.0);
        ui.add_sized(
            [field_w, CONTROL_H],
            egui::TextEdit::singleline(&mut rule.to)
                .hint_text(hint("writes…"))
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
pub(super) fn mini_switch(ui: &mut egui::Ui, on: bool, id: egui::Id) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(32.0, 18.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    paint_toggle(ui, rect, on, id, resp.hovered());
    resp.clicked()
}

/// Full-row clickable toggle. The whole label/caption strip is the hit area;
/// hover gives a faint highlight so the affordance reads.
///
/// Layout invariant: the text column is measured against the width that
/// remains AFTER reserving the toggle and a gutter, and the row is allocated
/// at the measured height. Never lay text across the full row width with a
/// fixed row height — long captions then slide under the toggle and overflow
/// the row.
pub(super) fn toggle_row(ui: &mut egui::Ui, value: &mut bool, label: &str, caption: &str) {
    let toggle_size = Vec2::new(36.0, 20.0);
    // Space between the text column and the toggle.
    let gutter = 24.0;

    let id = ui.make_persistent_id(("toggle_row", label));
    let total_w = ui.available_width();
    let text_w = total_w - toggle_size.x - gutter;

    // Measure first, then allocate exactly that.
    let label_galley = egui::WidgetText::from(RichText::new(label).size(13.5).color(FG))
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            text_w,
            egui::TextStyle::Body,
        );
    let caption_galley = egui::WidgetText::from(RichText::new(caption).size(11.5).color(MUTED_FG))
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            text_w,
            egui::TextStyle::Body,
        );
    let line_gap = 4.0;
    let text_h = label_galley.size().y + line_gap + caption_galley.size().y;
    let row_h = text_h.max(toggle_size.y) + 6.0;

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, row_h), egui::Sense::click());
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

    let text_top = rect.top() + 3.0;
    let label_h = label_galley.size().y;
    ui.painter().galley(
        egui::pos2(rect.left(), text_top),
        label_galley,
        Color32::PLACEHOLDER,
    );
    ui.painter().galley(
        egui::pos2(rect.left(), text_top + label_h + line_gap),
        caption_galley,
        Color32::PLACEHOLDER,
    );

    let toggle_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.right() - toggle_size.x - 2.0,
            rect.center().y - toggle_size.y / 2.0,
        ),
        toggle_size,
    );
    paint_toggle(ui, toggle_rect, *value, id, resp.hovered());
}

pub(super) fn paint_toggle(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    on: bool,
    id: egui::Id,
    hovered: bool,
) {
    let how_on = ui.ctx().animate_bool_with_time(id, on, 0.15);
    let bg = lerp_color(TOGGLE_OFF, PRIMARY, how_on);
    let bg = if hovered { lighten(bg, 0.05) } else { bg };
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(rect.height() / 2.0), bg);

    let pad = 2.5;
    let knob_r = rect.height() / 2.0 - pad;
    let knob_x = egui::lerp(
        (rect.left() + pad + knob_r)..=(rect.right() - pad - knob_r),
        how_on,
    );
    let knob_pos = egui::pos2(knob_x, rect.center().y);
    painter.circle_filled(
        knob_pos + Vec2::new(0.0, 0.6),
        knob_r,
        Color32::from_black_alpha(60),
    );
    painter.circle_filled(knob_pos, knob_r, Color32::from_rgb(245, 245, 248));
}

pub(super) fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

pub(super) fn lighten(c: Color32, t: f32) -> Color32 {
    lerp_color(c, Color32::WHITE, t)
}

pub(super) fn installed_badge(ui: &mut egui::Ui, text: &str) {
    // Caller is in a right_to_left layout: text first (lands right), then the
    // dot to its left → [●] [Installed (~670 MB)] flush right.
    ui.label(RichText::new(text).color(FG).size(13.0));
    ui.add_space(7.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, PRIMARY);
}

/// Primary (Save) button. Disabled when there's nothing to save; otherwise
/// lime fill that brightens on hover, darkens + scales down on press.
pub(super) fn primary_button(text: &str, enabled: bool) -> impl egui::Widget + '_ {
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
        ui.painter()
            .rect_filled(draw_rect, Rounding::same(RADIUS), fill);
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
pub(super) fn destructive_button(text: &str) -> impl egui::Widget + '_ {
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
        ui.painter()
            .rect_filled(draw_rect, Rounding::same(RADIUS), fill);
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
pub(super) fn ghost_button(text: &str, width: f32, height: f32) -> impl egui::Widget + '_ {
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

pub(super) fn divider(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    let avail = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.left()..=rect.right(), rect.center().y, border());
    ui.add_space(8.0);
}

/// Dimmed-scrim modal: paints a click-to-dismiss scrim above the panels and
/// one centred 360px card above that, in the house dialog frame. Returns
/// true when the scrim was clicked — callers treat that as cancel. Both
/// dialogs (and any future one) share this so the chrome can't drift apart.
pub(super) fn modal_card(ctx: &egui::Context, id: &str, body: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut scrim_clicked = false;
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new((id, "scrim")))
        .order(egui::Order::Middle)
        .fixed_pos(screen.left_top())
        .show(ctx, |ui| {
            let r = ui.allocate_rect(screen, egui::Sense::click());
            ui.painter()
                .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(160));
            if r.clicked() {
                scrim_clicked = true;
            }
        });

    egui::Window::new(id)
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
            body(ui);
        });
    scrim_clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row widths an option row is actually allocated. `combo_item` measures
    /// against `ui.available_width()` *inside* the popup, which is CONTROL_W
    /// less the popup frame's margins — so the tests span from CONTROL_W down
    /// to a comfortably narrower row rather than assuming the ideal one.
    const ROW_WIDTHS: [f32; 3] = [CONTROL_W, CONTROL_W - 8.0, CONTROL_W - 24.0];

    /// A context with fonts loaded. `RawInput::default()` leaves
    /// `max_texture_side` unset, and a zero-sized atlas lays every glyph out at
    /// zero width — every measurement then "fits" and the test proves nothing.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                max_texture_side: Some(2048),
                ..Default::default()
            },
            |_| {},
        );
        ctx
    }

    /// A Windows device name is longer than any popup we're willing to draw,
    /// so it is elided — and the galley measures within the width it was given
    /// rather than running past the popup edge the way `layout_no_wrap` did.
    #[test]
    fn a_long_option_label_is_elided_within_its_row() {
        let ctx = font_ctx();
        let long = "Microfoonmatrix (Intel® Smart Sound Technologie voor digitale microfoons)";
        for row_w in ROW_WIDTHS {
            let max = combo_item_text_w(row_w);
            let g = elided_galley(&ctx, long, egui::FontId::proportional(13.0), FG, max);
            assert!(g.elided, "a 72-char device name must not fit {max}px");
            assert!(
                g.size().x <= max,
                "galley {} wider than the {max}px it was given",
                g.size().x
            );
            let last = g.rows[0].glyphs.last().expect("a laid-out row");
            assert_eq!(last.chr, '…', "elided text ends in an ellipsis");
        }
    }

    /// …and a label that fits is untouched: no ellipsis, and `elided` is false
    /// so the caller shows no tooltip.
    #[test]
    fn a_short_option_label_is_left_alone() {
        let ctx = font_ctx();
        for row_w in ROW_WIDTHS {
            for label in ["Hold", "Toggle", "System default"] {
                let g = elided_galley(
                    &ctx,
                    label,
                    egui::FontId::proportional(13.0),
                    FG,
                    combo_item_text_w(row_w),
                );
                assert!(!g.elided, "{label:?} fits a {row_w}px row and stays whole");
                assert_eq!(g.text(), label);
                assert!(g.rows[0].glyphs.iter().all(|gl| gl.chr != '…'));
            }
        }
    }

    /// A label long enough to reach the check is cut short of it — the row
    /// reserves the check column whether or not it is the selected row, so
    /// nothing shifts as the selection moves.
    #[test]
    fn a_label_stops_short_of_the_check_column() {
        let ctx = font_ctx();
        for row_w in ROW_WIDTHS {
            let g = elided_galley(
                &ctx,
                "Microfoonmatrix (Intel® Smart Sound Technologie)",
                egui::FontId::proportional(13.0),
                FG,
                combo_item_text_w(row_w),
            );
            // The label starts at COMBO_ITEM_INSET from the row's left edge;
            // the check's leading edge sits COMBO_ITEM_CHECK_INSET +
            // COMBO_ITEM_CHECK_REACH in from its right — the same arithmetic
            // `combo_item` paints with.
            let label_right = COMBO_ITEM_INSET + g.size().x;
            let check_left = row_w - COMBO_ITEM_CHECK_INSET - COMBO_ITEM_CHECK_REACH;
            assert!(
                label_right <= check_left,
                "in a {row_w}px row the label ends at {label_right}, check starts at {check_left}"
            );
        }
    }
}
