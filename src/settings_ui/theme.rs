// Theme: every colour, metric, and the egui style for the settings window,
// in one place. Monochrome, like the pill: shadcn's "neutral" DARK greys
// (oklch -> sRGB), white for the primary action, and the pill's green and
// red for status only.
//
// Rules baked into this theme that the rest of the UI relies on:
// - `override_text_color` is FG, so any text that should NOT be
//   full-brightness needs an explicit RichText colour (MUTED_FG, HINT_FG).
// - Borders are solid opaque greys: a translucent 1px stroke feathers into
//   a broken, "pixely" line.
// - All inputs share CONTROL_W x CONTROL_H so they line up column-perfect.
// - The window is dark whatever Windows is set to: the theme preference is
//   pinned and both of egui's style slots carry this theme (`install_style`).
// - Every text colour clears 4.5:1 (WCAG AA) on each surface it is drawn on;
//   `text_meets_aa_contrast` holds the pairs — add a row with a new one.

use egui::{Color32, RichText, Rounding, Stroke, Vec2};
pub(super) const BG: Color32 = Color32::from_rgb(10, 10, 10); // --background  oklch(0.145 0 0)
pub(super) const SIDEBAR_BG: Color32 = Color32::from_rgb(23, 23, 23); // --sidebar/--card  0.205
pub(super) const CONTROL_FILL: Color32 = Color32::from_rgb(32, 32, 32); // input surface
pub(super) const CONTROL_HOVER: Color32 = Color32::from_rgb(44, 44, 44);
pub(super) const SELECTED_BG: Color32 = Color32::from_rgb(38, 38, 38); // --accent  0.269 (selected nav)
pub(super) const FG: Color32 = Color32::from_rgb(250, 250, 250); // --foreground  0.985
pub(super) const MUTED_FG: Color32 = Color32::from_rgb(161, 161, 161); // --muted-foreground  0.708
/// Placeholder text inside empty inputs. Dimmer than MUTED_FG and italic at
/// the call sites, so examples can't be mistaken for typed content — the
/// global `override_text_color` would otherwise paint hints full-brightness.
/// Still at least 4.5:1 on the input fill (`text_meets_aa_contrast`).
pub(super) const HINT_FG: Color32 = Color32::from_rgb(136, 136, 136);
pub(super) const RING: Color32 = Color32::from_rgb(115, 115, 115); // --ring  0.556 (neutral focus)
/// Keyboard focus ring around the custom controls (`widgets::focus_ring`).
/// Opaque, like the borders, and at least 3:1 on every surface it sits on
/// (`focus_ring_meets_non_text_contrast`).
pub(super) const FOCUS_RING: Color32 = RING;
/// The primary action (Save): white with black ink. The window is
/// monochrome, like the pill — brightness says what matters, and colour is
/// kept for status alone.
pub(super) const PRIMARY: Color32 = Color32::from_rgb(240, 240, 240);
pub(super) const PRIMARY_HOVER: Color32 = Color32::from_rgb(255, 255, 255);
pub(super) const PRIMARY_PRESSED: Color32 = Color32::from_rgb(212, 212, 212);
pub(super) const PRIMARY_FG: Color32 = Color32::from_rgb(10, 10, 10);
/// Something is set up and working ("Configured", "Installed"): the pill's
/// own delivered green (`pill::geom::SUCCESS`), so a status reads the same
/// in both places.
pub(super) const SUCCESS: Color32 = Color32::from_rgb(74, 188, 120);
/// Errors and the destructive action: the pill's failed red
/// (`pill::geom::ERROR`).
pub(super) const DESTRUCTIVE: Color32 = Color32::from_rgb(214, 96, 96);
pub(super) const DESTRUCTIVE_HOVER: Color32 = Color32::from_rgb(226, 110, 110);
pub(super) const DESTRUCTIVE_PRESSED: Color32 = Color32::from_rgb(208, 94, 94);
/// Text on the red. Dark ink: white on this red is under 3:1.
pub(super) const DESTRUCTIVE_FG: Color32 = Color32::from_rgb(50, 8, 8);
/// A switch that is off: a dark track under a light knob.
pub(super) const TOGGLE_OFF: Color32 = Color32::from_rgb(54, 54, 54);
pub(super) const KNOB_OFF: Color32 = Color32::from_rgb(245, 245, 245);
/// A switch that is on: the same pair inverted, a light track under a dark
/// knob, so on and off differ by brightness rather than hue. Each pair holds
/// 3:1 against the page and between knob and track
/// (`switch_meets_non_text_contrast`).
pub(super) const TOGGLE_ON: Color32 = Color32::from_rgb(225, 225, 225);
pub(super) const KNOB_ON: Color32 = Color32::from_rgb(23, 23, 23);

// Borders are SOLID greys, not semi-transparent strokes. A 1px stroke of a
// translucent colour gets spread by egui's ~1px feathering, leaving gaps that
// read as a broken / "pixely" line; an opaque colour feathers into a clean,
// continuous hairline.
pub(super) const BORDER: Color32 = Color32::from_rgb(38, 38, 38); // dividers / faint seams
pub(super) const CONTROL_BORDER: Color32 = Color32::from_rgb(52, 52, 52); // input & button outlines
pub(super) const CONTROL_BORDER_HOVER: Color32 = Color32::from_rgb(80, 80, 80);

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

pub(super) const RAIL_W: f32 = 168.0;
pub(super) const CONTROL_W: f32 = 232.0;
pub(super) const CONTROL_H: f32 = 28.0;
/// Footer and dialog buttons: a touch taller than a row control, so the
/// actions that end something read as a step apart from the settings.
pub(super) const BUTTON_H: f32 = 30.0;
/// Every settings row — `row` and `toggle_row` alike — is this tall: one line
/// of label, centred on a CONTROL_H control, so a pane is an even ladder of
/// rows whatever control each one holds.
pub(super) const ROW_H: f32 = 40.0;
/// A row label's size; its caption is a tooltip, not a second line.
pub(super) const LABEL_SIZE: f32 = 13.5;
pub(super) const RADIUS: f32 = 6.0; // controls, buttons, popovers
pub(super) const RADIUS_SM: f32 = 4.0; // nav items, row hover
/// Gap between a control's edge and its focus ring's inner edge.
pub(super) const FOCUS_RING_GAP: f32 = 1.25;
/// Focus ring stroke width. Gap plus width stays inside egui's 3px clip
/// margin, so a control flush with a scroll area's edge keeps its whole ring.
pub(super) const FOCUS_RING_W: f32 = 1.5;
/// Height of the Save/Close strip along the bottom of the pane.
pub(super) const FOOTER_H: f32 = 50.0;

/// Install the theme.
///
/// This window is dark-only, so it must not follow the system theme. egui keeps
/// a style *per* theme (`dark_style` / `light_style`) and picks between them
/// every frame from the system theme; `install_style` runs in eframe's creation
/// closure, before the first frame has reported one, so a plain `set_style`
/// would land in the fallback (Dark) slot alone and a light-mode Windows would
/// then flip to an unwritten `light_style` — combos and text edits reverting to
/// egui's default light look against the dark panels.
///
/// So: pin the preference to Dark, *and* write the same style into both slots,
/// so a flip we didn't ask for still cannot reveal an unstyled one.
pub(super) fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    let style = dark_style();
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// The settings window's style, built on egui's *dark* defaults — everything
/// this theme doesn't override (extreme/faint fills, error and warning colours)
/// has to come from there, or the parts we don't name go light.
fn dark_style() -> egui::Style {
    let mut style = egui::Theme::Dark.default_style();
    // egui makes labels selectable by default, which shows the text-select
    // I-beam over our row/label text and makes controls feel un-clickable.
    style.interaction.selectable_labels = false;
    style.spacing.item_spacing = Vec2::new(10.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 4.0);
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
    v.selection.bg_fill = Color32::from_rgb(51, 51, 51);
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

    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Theme;

    /// The window is dark whatever Windows is set to: the preference is pinned,
    /// and both style slots carry the theme, so the per-frame dark/light pick
    /// cannot land on an unstyled one.
    #[test]
    fn theme_survives_a_light_system() {
        let ctx = egui::Context::default();
        install_style(&ctx);

        for theme in [Theme::Dark, Theme::Light] {
            let style = ctx.style_of(theme);
            assert_eq!(style.visuals.panel_fill, BG, "{theme:?} panel");
            assert_eq!(style.visuals.window_fill, SIDEBAR_BG, "{theme:?} popup");
            assert_eq!(
                style.visuals.override_text_color,
                Some(FG),
                "{theme:?} text"
            );
            // The two that actually broke: a ComboBox's closed button and a
            // TextEdit both paint themselves from `widgets.inactive`.
            assert_eq!(
                style.visuals.widgets.inactive.bg_fill, CONTROL_FILL,
                "{theme:?} control fill"
            );
            assert_eq!(
                style.visuals.widgets.inactive.bg_stroke,
                input_border(),
                "{theme:?} control border"
            );
        }

        let preference = ctx.options(|o| o.theme_preference);
        assert_eq!(preference, egui::ThemePreference::Dark);
        // …and once a frame reports a light Windows — the moment the bug used
        // to appear — the active style is still ours.
        let _ = ctx.run(
            egui::RawInput {
                system_theme: Some(Theme::Light),
                ..Default::default()
            },
            |_| {},
        );
        assert_eq!(ctx.theme(), Theme::Dark);
        assert_eq!(ctx.style().visuals.panel_fill, BG);
    }

    /// WCAG 2 contrast ratio between two opaque colours, 1.0..=21.0.
    fn contrast(a: Color32, b: Color32) -> f32 {
        fn luminance(c: Color32) -> f32 {
            let lin = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
        }
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// Every text colour against every surface it is drawn on — button
    /// states included — clears WCAG AA for normal text (4.5:1), in both
    /// style slots, so a colour tweak can't quietly regress one.
    #[test]
    fn text_meets_aa_contrast() {
        let ctx = egui::Context::default();
        install_style(&ctx);

        for theme in [Theme::Dark, Theme::Light] {
            let v = &ctx.style_of(theme).visuals;
            let text = v.override_text_color.expect("theme sets text colour");
            let pairs = [
                ("text on page", text, v.panel_fill),
                ("text on sidebar/popup", text, v.window_fill),
                ("text on control", text, v.widgets.inactive.bg_fill),
                ("text on hovered control", text, CONTROL_HOVER),
                ("selected text", text, v.selection.bg_fill),
                ("text on selected nav", text, SELECTED_BG),
                ("text in a field", text, v.extreme_bg_color),
                ("muted on page", MUTED_FG, BG),
                ("muted on sidebar", MUTED_FG, SIDEBAR_BG),
                ("muted on disabled button", MUTED_FG, CONTROL_FILL),
                ("muted on hovered control", MUTED_FG, CONTROL_HOVER),
                ("hint in a field", HINT_FG, v.extreme_bg_color),
                ("hint on control fill", HINT_FG, CONTROL_FILL),
                ("link on page", v.hyperlink_color, BG),
                ("error on page", DESTRUCTIVE, BG),
                ("error on sidebar/dialog", DESTRUCTIVE, SIDEBAR_BG),
                ("primary button", PRIMARY_FG, PRIMARY),
                ("primary hovered", PRIMARY_FG, PRIMARY_HOVER),
                ("primary pressed", PRIMARY_FG, PRIMARY_PRESSED),
                ("destructive button", DESTRUCTIVE_FG, DESTRUCTIVE),
                ("destructive hovered", DESTRUCTIVE_FG, DESTRUCTIVE_HOVER),
                ("destructive pressed", DESTRUCTIVE_FG, DESTRUCTIVE_PRESSED),
            ];
            let failing: Vec<String> = pairs
                .iter()
                .map(|&(name, fg, bg)| (name, contrast(fg, bg)))
                .filter(|&(_, ratio)| ratio < 4.5)
                .map(|(name, ratio)| format!("{name}: {ratio:.2}:1"))
                .collect();
            assert!(failing.is_empty(), "{theme:?} below 4.5:1 — {failing:#?}");
        }
    }

    /// The focus ring is the only sign of where Tab has landed, so it clears
    /// WCAG's 3:1 for UI components on every surface it is drawn over.
    #[test]
    fn focus_ring_meets_non_text_contrast() {
        for (name, bg) in [("page", BG), ("sidebar/dialog", SIDEBAR_BG)] {
            let ratio = contrast(FOCUS_RING, bg);
            assert!(ratio >= 3.0, "focus ring on {name}: {ratio:.2}:1");
        }
    }

    /// A switch says on or off by brightness alone, so both states must read
    /// as a control: the on track against the page, and each knob against
    /// its own track.
    #[test]
    fn switch_meets_non_text_contrast() {
        for (name, a, b) in [
            ("on track on page", TOGGLE_ON, BG),
            ("knob on on track", KNOB_ON, TOGGLE_ON),
            ("knob on off track", KNOB_OFF, TOGGLE_OFF),
        ] {
            let ratio = contrast(a, b);
            assert!(ratio >= 3.0, "{name}: {ratio:.2}:1");
        }
    }
}
