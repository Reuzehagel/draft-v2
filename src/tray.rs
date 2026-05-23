use anyhow::Result;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

pub struct Tray {
    _icon: TrayIcon,
    pub menu_ids: MenuIds,
}

pub struct MenuIds {
    pub settings: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
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

pub fn build(tooltip: &str) -> Result<Tray> {
    let menu = Menu::new();
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let settings_id = settings.id().clone();
    let quit_id = quit.id().clone();
    menu.append(&settings)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let icon = TrayIconBuilder::new()
        .with_tooltip(tooltip)
        .with_icon(make_icon())
        .with_menu(Box::new(menu))
        .build()?;

    Ok(Tray {
        _icon: icon,
        menu_ids: MenuIds {
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
