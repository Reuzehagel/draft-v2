// The tray is where you look things up — the pill is what you act with
// (settled in issue #28; see ADR-0003 for the pill core's standing). The tooltip answers "what will my next dictation use?" and the menu
// only offers what is actually available. So the `TrayIcon` and the `MenuItem`
// handles are retained — the tooltip and the enabled states change over the
// app's life, driven by [`Tray::apply`] from events the adapter already handles.
//
// The icon itself is deliberately static: a second glanceable state indicator
// would duplicate the pill, and it's the one you can't see when the tray is
// collapsed.

use anyhow::Result;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::config::Provider;

pub struct Tray {
    icon: TrayIcon,
    /// Retained so its enabled state can follow the history.
    copy_last: MenuItem,
    pub menu_ids: MenuIds,
}

pub struct MenuIds {
    pub copy_last: tray_icon::menu::MenuId,
    pub settings: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
}

/// Everything the tray displays, gathered by the adapter. Rebuilt (cheaply) at
/// each of the events that can change it rather than polled.
pub struct Status {
    pub hotkey: String,
    pub provider: Provider,
    /// Version of an available update, when the check found one. `None` while
    /// the check is dormant, in flight, or already on the latest release.
    pub update: Option<String>,
    /// Whether there is a transcript to copy.
    pub has_history: bool,
}

/// The tooltip text for a status. Pure — the formatting is the part worth
/// testing, and it can't touch the shell.
fn tooltip(status: &Status) -> String {
    let mut s = format!("Draft — {} · {}", status.hotkey, status.provider.label());
    if let Some(version) = &status.update {
        s.push_str(&format!("\nUpdate available: {version}"));
    }
    s
}

impl Tray {
    /// Push a fresh status to the shell: tooltip text, and whether "Copy last
    /// transcription" is offered at all. Failures are logged, never fatal —
    /// a stale tooltip is not worth taking the app down for.
    pub fn apply(&self, status: &Status) {
        if let Err(e) = self.icon.set_tooltip(Some(tooltip(status))) {
            tracing::warn!(error = %e, "failed to set tray tooltip");
        }
        self.copy_last.set_enabled(status.has_history);
    }
}

fn make_icon() -> Icon {
    // 16x16 solid black circle on transparent; quick placeholder.
    let size = 16u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let r = (size as f32 / 2.0) - 1.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let i = ((y * size + x) * 4) as usize;
            if d <= r {
                rgba[i] = 20;
                rgba[i + 1] = 20;
                rgba[i + 2] = 20;
                rgba[i + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("icon")
}

pub fn build(status: &Status) -> Result<Tray> {
    let menu = Menu::new();
    let copy_last = MenuItem::new("Copy last transcription", status.has_history, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let copy_last_id = copy_last.id().clone();
    let settings_id = settings.id().clone();
    let quit_id = quit.id().clone();
    menu.append(&copy_last)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&settings)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let icon = TrayIconBuilder::new()
        .with_tooltip(tooltip(status))
        .with_icon(make_icon())
        .with_menu(Box::new(menu))
        .build()?;

    Ok(Tray {
        icon,
        copy_last,
        menu_ids: MenuIds {
            copy_last: copy_last_id,
            settings: settings_id,
            quit: quit_id,
        },
    })
}

pub fn menu_event_receiver() -> crossbeam_channel::Receiver<MenuEvent> {
    let (tx, rx) = crossbeam_channel::unbounded();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = tx.send(e);
    }));
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;

    fn status() -> Status {
        Status {
            hotkey: "Ctrl+Backslash".into(),
            provider: Provider::LocalParakeet,
            update: None,
            has_history: false,
        }
    }

    #[test]
    fn tooltip_names_the_hotkey_and_the_active_provider() {
        let t = tooltip(&status());
        assert!(t.contains("Ctrl+Backslash"), "{t}");
        assert!(t.contains("Local (Parakeet)"), "{t}");
    }

    #[test]
    fn tooltip_follows_the_configured_provider() {
        let t = tooltip(&Status {
            provider: Provider::Groq,
            ..status()
        });
        assert!(t.contains("Groq"), "{t}");
        assert!(!t.contains("Parakeet"), "{t}");
    }

    #[test]
    fn tooltip_says_nothing_about_updates_when_none_is_known() {
        let t = tooltip(&status());
        assert!(!t.to_lowercase().contains("update"), "{t}");
        assert_eq!(t.lines().count(), 1, "{t}");
    }

    #[test]
    fn tooltip_mentions_an_available_update_with_its_version() {
        let t = tooltip(&Status {
            update: Some("9.9.9".into()),
            ..status()
        });
        assert!(t.contains("9.9.9"), "{t}");
        assert!(t.to_lowercase().contains("update"), "{t}");
        // The provider line survives — the update is an addition, not a swap.
        assert!(t.contains("Local (Parakeet)"), "{t}");
    }
}
