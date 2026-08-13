// Theme: every colour, metric, and the egui style for the settings window,
// in one place. shadcn "neutral + lime" DARK, oklch -> sRGB.
//
// Rules baked into this theme that the rest of the UI relies on:
// - `override_text_color` is FG, so any text that should NOT be
//   full-brightness needs an explicit RichText colour (MUTED_FG, HINT_FG).
// - Borders are solid opaque greys: a translucent 1px stroke feathers into
//   a broken, "pixely" line.
// - All inputs share CONTROL_W x CONTROL_H so they line up column-perfect.

use egui::{Color32, RichText, Rounding, Stroke, Vec2};
// shadcn "neutral + lime" DARK theme, oklch → sRGB.
pub(super) const BG: Color32 = Color32::from_rgb(10, 10, 10); // --background  oklch(0.145 0 0)
pub(super) const SIDEBAR_BG: Color32 = Color32::from_rgb(23, 23, 23); // --sidebar/--card  0.205
pub(super) const CONTROL_FILL: Color32 = Color32::from_rgb(32, 32, 35); // input surface
pub(super) const CONTROL_HOVER: Color32 = Color32::from_rgb(44, 44, 48);
pub(super) const SELECTED_BG: Color32 = Color32::from_rgb(38, 38, 38); // --accent  0.269 (selected nav)
pub(super) const FG: Color32 = Color32::from_rgb(250, 250, 250); // --foreground  0.985
pub(super) const MUTED_FG: Color32 = Color32::from_rgb(161, 161, 161); // --muted-foreground  0.708
/// Placeholder text inside empty inputs. Dimmer than MUTED_FG and italic at
/// the call sites, so examples can't be mistaken for typed content — the
/// global `override_text_color` would otherwise paint hints full-brightness.
pub(super) const HINT_FG: Color32 = Color32::from_rgb(112, 112, 115);
pub(super) const RING: Color32 = Color32::from_rgb(115, 115, 115); // --ring  0.556 (neutral focus)
pub(super) const PRIMARY: Color32 = Color32::from_rgb(132, 204, 22); // --primary (lime)
pub(super) const PRIMARY_HOVER: Color32 = Color32::from_rgb(146, 214, 40);
pub(super) const PRIMARY_PRESSED: Color32 = Color32::from_rgb(110, 172, 18);
pub(super) const PRIMARY_FG: Color32 = Color32::from_rgb(53, 84, 14); // --primary-foreground (text on lime)
pub(super) const DESTRUCTIVE: Color32 = Color32::from_rgb(235, 107, 107); // --destructive
pub(super) const TOGGLE_OFF: Color32 = Color32::from_rgb(54, 54, 58);

// Borders are SOLID greys, not semi-transparent strokes. A 1px stroke of a
// translucent colour gets spread by egui's ~1px feathering, leaving gaps that
// read as a broken / "pixely" line; an opaque colour feathers into a clean,
// continuous hairline.
pub(super) const BORDER: Color32 = Color32::from_rgb(38, 38, 42); // dividers / faint seams
pub(super) const CONTROL_BORDER: Color32 = Color32::from_rgb(52, 52, 58); // input & button outlines
pub(super) const CONTROL_BORDER_HOVER: Color32 = Color32::from_rgb(80, 80, 88);

/// Style a TextEdit placeholder so it reads as an example, not content.
pub(super) fn hint(text: &str) -> RichText {
    RichText::new(text).color(HINT_FG).italics()
}

pub(super) fn border() -> Stroke {
    Stroke::new(1.0, BORDER)
}
pub(super) fn input_border() -> Stroke {
    Stroke::new(1.0, CONTROL_BORDER)
}

pub(super) const RAIL_W: f32 = 184.0;
pub(super) const CONTROL_W: f32 = 240.0;
pub(super) const CONTROL_H: f32 = 32.0;
pub(super) const RADIUS: f32 = 10.0; // shadcn --radius (0.625rem) — controls, buttons, popovers
pub(super) const RADIUS_SM: f32 = 8.0; // nav items, row hover

pub(super) fn install_style(ctx: &egui::Context) {
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
    // Subtle dark handle. With `foreground_color` (the default) the handle is
    // painted with `widgets.*.fg_stroke` — which this theme sets to bright
    // white for text, turning the scrollbar into a glowing rod.
    style.spacing.scroll.foreground_color = false;
    // A real gutter between content and the bar — 4px reads as the bar
    // touching the controls.
    style.spacing.scroll.bar_inner_margin = 14.0;
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
