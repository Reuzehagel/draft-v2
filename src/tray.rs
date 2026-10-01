// The tray is where you look things up — the pill is what you act with
// (settled in issue #28; see ADR-0003 for the pill core's standing). The tooltip answers "what will my next dictation use?" and the menu
// only offers what is actually available. So the `TrayIcon` and the `MenuItem`
// handles are retained — the tooltip and the enabled states change over the
// app's life, driven by [`Tray::apply`] from events the adapter already handles.
//
// The icon carries no state: a second glanceable state indicator would
// duplicate the pill, and it's the one you can't see when the tray is
// collapsed. What it does follow is the *desk* (#101) — drawn in the ink the
// taskbar's theme wants, at the exact pixel size the tray shows it, so it is
// neither lost on a dark taskbar nor upscaled into a blur at 150%. Both are
// re-probed whenever Windows broadcasts a settings or display change (see
// [`desk_changes`]); neither is polled.

use anyhow::Result;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::config::Provider;

pub struct Tray {
    icon: TrayIcon,
    /// What the icon was last drawn for, so a re-probe that finds nothing new
    /// hands the shell nothing.
    look: Look,
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

    /// Re-probe the taskbar's theme and the tray's icon size, and redraw the
    /// icon if either moved. Called on a theme change and a display change —
    /// cheap enough for both, and a no-op when the answer is the same.
    pub fn refresh_icon(&mut self) {
        let look = Look::current();
        if look == self.look {
            return;
        }
        tracing::info!(?look, "tray icon redrawn");
        match self.icon.set_icon(Some(make_icon(look))) {
            Ok(()) => self.look = look,
            Err(e) => tracing::warn!(error = %e, "failed to set tray icon"),
        }
    }
}

/// Which way the taskbar is painted — the one colour fact the icon depends on.
/// Windows' *system* mode, not the apps' mode: the two are set separately, and
/// the taskbar follows the former.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Taskbar {
    Light,
    Dark,
}

/// Everything the icon is drawn from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Look {
    /// The tray's icon size in physical pixels — `SM_CXSMICON` at the primary
    /// monitor's DPI, and never under 16. Drawn at exactly this, so the shell
    /// never rescales it.
    pub px: u32,
    pub taskbar: Taskbar,
}

impl Look {
    /// The look the shell wants right now. A failed probe answers what a
    /// default Windows 11 desk has: 100% scaling and a dark taskbar.
    fn current() -> Look {
        Look {
            px: win::icon_px().unwrap_or(DEFAULT_PX).max(DEFAULT_PX),
            taskbar: win::taskbar().unwrap_or(Taskbar::Dark),
        }
    }
}

/// `SM_CXSMICON` at 96 DPI, and the smallest size the layout is drawn at.
const DEFAULT_PX: u32 = 16;

/// The glyph's colour on each taskbar: the inks Windows' own tray glyphs use,
/// white on dark and near-black on light.
fn ink(taskbar: Taskbar) -> [u8; 3] {
    match taskbar {
        Taskbar::Dark => [255, 255, 255],
        Taskbar::Light => [26, 26, 26],
    }
}

/// The icon as straight RGBA, `look.px` square: the mark (`crate::mark`) in
/// the one ink, drawn at exactly the size the tray shows it.
fn icon_rgba(look: Look) -> Vec<u8> {
    // The ink goes under the coverage unpremultiplied, which is what
    // `Icon::from_rgba` takes.
    let [r, g, b] = ink(look.taskbar);
    crate::mark::tray(look.px)
        .into_iter()
        .flat_map(|a| match a {
            0 => [0; 4],
            a => [r, g, b, a],
        })
        .collect()
}

fn make_icon(look: Look) -> Icon {
    Icon::from_rgba(icon_rgba(look), look.px, look.px).expect("icon")
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

    let look = Look::current();
    tracing::info!(?look, "tray icon");
    let icon = TrayIconBuilder::new()
        .with_tooltip(tooltip(status))
        .with_icon(make_icon(look))
        .with_menu(Box::new(menu))
        .build()?;

    Ok(Tray {
        icon,
        look,
        copy_last,
        menu_ids: MenuIds {
            copy_last: copy_last_id,
            settings: settings_id,
            quit: quit_id,
        },
    })
}

/// The menu's events, as a channel the app loop drains.
///
/// The `waker` is what makes that draining happen: the loop rests at
/// `ControlFlow::Wait`, and a channel send is not a message it can wake for.
pub fn menu_event_receiver(waker: crate::wake::Waker) -> crossbeam_channel::Receiver<MenuEvent> {
    let (tx, rx) = crossbeam_channel::unbounded();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = tx.send(e);
        waker.wake();
    }));
    rx
}

/// A message each time the desk may have changed under the icon: a theme
/// switch, a scaling change, a display change.
///
/// Heard by a hidden top-level window, not the pill's hook: the pill hears DPI
/// only for its own monitor and may not exist at all, while the tray lives on
/// the primary. And a broadcast, not a registry watch, because the broadcast
/// arrives once the change has *taken effect* — Settings writes the apps' and
/// the system's theme values back to back, and a watch that re-arms between
/// them can wake on the first and miss the second.
///
/// `WM_SETTINGCHANGE` also fires for things that don't touch the icon; those
/// arrive too, and [`Tray::refresh_icon`] finds nothing to redraw. The window
/// is created on the event-loop thread, so its messages are what wakes the
/// loop — no waker needed. Call once, from that thread; if the window can't be
/// made, the icon keeps the look it launched with, which is logged.
pub fn desk_changes() -> crossbeam_channel::Receiver<()> {
    let (tx, rx) = crossbeam_channel::unbounded();
    if let Err(e) = win::listen(tx) {
        tracing::warn!(error = %e, "tray icon won't follow theme or scale changes");
    }
    rx
}

#[cfg(windows)]
mod win {
    use super::Taskbar;
    use anyhow::{anyhow, Result};
    use std::sync::OnceLock;
    use windows::core::w;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, SM_CXSMICON, WINDOW_EX_STYLE,
        WM_DISPLAYCHANGE, WM_DPICHANGED, WM_SETTINGCHANGE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    /// Where the listening window reports. A static because a wndproc has no
    /// closure to carry it in, and there is only ever the one window.
    static CHANGED: OnceLock<crossbeam_channel::Sender<()>> = OnceLock::new();

    const PERSONALIZE: windows::core::PCWSTR =
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

    /// `SystemUsesLightTheme`. Absent on builds that predate the light
    /// taskbar, which were dark.
    pub fn taskbar() -> Option<Taskbar> {
        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                w!("SystemUsesLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut value as *mut u32 as *mut _),
                Some(&mut size),
            )
        };
        (status == ERROR_SUCCESS).then_some(if value != 0 {
            Taskbar::Light
        } else {
            Taskbar::Dark
        })
    }

    /// The small-icon size at the primary monitor's DPI, which is where the
    /// notification area lives. Per-monitor DPI, not the process's system DPI:
    /// the latter is frozen at launch and goes stale when the scale changes.
    pub fn icon_px() -> Option<u32> {
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let (mut x, mut y) = (0u32, 0u32);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.ok()?;
        let px = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, x) };
        u32::try_from(px).ok().filter(|&px| px > 0)
    }

    /// Create the hidden window that hears the desk change. Top-level and never
    /// shown: a message-only window would be tidier, but broadcasts skip those.
    /// It lives as long as the process, so its handle is not kept.
    pub fn listen(tx: crossbeam_channel::Sender<()>) -> Result<()> {
        CHANGED
            .set(tx)
            .map_err(|_| anyhow!("desk listener already created"))?;
        let class = w!("DraftTrayDesk");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: class,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&wc) } == 0 {
            return Err(anyhow!("RegisterClassW failed"));
        }
        unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WINDOW_EX_STYLE::default(),
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
        }?;
        Ok(())
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if matches!(msg, WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_DPICHANGED) {
            if let Some(tx) = CHANGED.get() {
                let _ = tx.send(());
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;
    use tiny_skia::Pixmap;

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

    /// `SM_CXSMICON` at 100, 125, 150, 175, 200 and 250%.
    const SIZES: [u32; 6] = [16, 20, 24, 28, 32, 40];
    const TASKBARS: [Taskbar; 2] = [Taskbar::Light, Taskbar::Dark];

    /// Windows 11's taskbar fills, near enough: the mica tint over a plain
    /// wallpaper in each mode.
    fn taskbar_fill(t: Taskbar) -> [u8; 3] {
        match t {
            Taskbar::Light => [238, 238, 238],
            Taskbar::Dark => [32, 32, 32],
        }
    }

    /// WCAG relative luminance.
    fn luminance([r, g, b]: [u8; 3]) -> f64 {
        let c = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b)
    }

    fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn the_ink_stands_out_from_either_taskbar() {
        for t in TASKBARS {
            let c = contrast(ink(t), taskbar_fill(t));
            // WCAG's 3:1 for graphics would pass a mid-grey; this asks for
            // what Windows' own glyphs get.
            assert!(c >= 12.0, "{t:?}: {c:.1}:1");
        }
    }

    #[test]
    fn every_drawn_pixel_is_the_taskbars_ink() {
        for px in SIZES {
            for t in TASKBARS {
                let rgba = icon_rgba(Look { px, taskbar: t });
                for p in rgba.chunks_exact(4) {
                    if p[3] > 0 {
                        assert_eq!(&p[..3], &ink(t), "{px}px {t:?}");
                    } else {
                        assert_eq!(p, [0; 4], "{px}px {t:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn it_is_drawn_at_exactly_the_size_the_tray_shows_it() {
        for px in SIZES {
            let rgba = icon_rgba(Look {
                px,
                taskbar: Taskbar::Dark,
            });
            assert_eq!(rgba.len(), (px * px * 4) as usize, "{px}px");
        }
    }

    #[test]
    fn the_probes_answer_on_a_real_desk() {
        // The fallback would hide a probe that never works: 16px is also what a
        // broken one draws. So ask the real system. (Not the taskbar probe:
        // its value is legitimately absent on older builds.)
        let px = win::icon_px().expect("icon size probe");
        assert!((16..=64).contains(&px), "{px}px");
    }

    /// Writes each size on each taskbar to `target/tray-preview/`, zoomed, for
    /// looking at. Asserts nothing; run with
    /// `cargo test -- --ignored tray::tests::preview`.
    #[test]
    #[ignore]
    fn preview() {
        const ZOOM: u32 = 8;
        const PAD: u32 = 4;
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("tray-preview");
        std::fs::create_dir_all(&dir).expect("create preview dir");
        let width: u32 = SIZES.iter().map(|px| px + PAD).sum::<u32>() + PAD;
        let height = 40 + 2 * PAD;
        for t in TASKBARS {
            let bg = taskbar_fill(t);
            let mut sheet = vec![[bg[0], bg[1], bg[2]]; (width * height) as usize];
            let mut left = PAD;
            for px in SIZES {
                let rgba = icon_rgba(Look { px, taskbar: t });
                for y in 0..px {
                    for x in 0..px {
                        let i = ((y * px + x) * 4) as usize;
                        let a = rgba[i + 3] as u32;
                        let o = &mut sheet[((PAD + y) * width + left + x) as usize];
                        for c in 0..3 {
                            o[c] = ((rgba[i + c] as u32 * a + o[c] as u32 * (255 - a)) / 255) as u8;
                        }
                    }
                }
                left += px + PAD;
            }
            let mut out = Pixmap::new(width * ZOOM, height * ZOOM).unwrap();
            for (i, p) in out.pixels_mut().iter_mut().enumerate() {
                let (x, y) = (i as u32 % (width * ZOOM), i as u32 / (width * ZOOM));
                let [r, g, b] = sheet[((y / ZOOM) * width + x / ZOOM) as usize];
                *p = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, 255).unwrap();
            }
            out.save_png(dir.join(format!("{t:?}.png").to_lowercase()))
                .expect("write preview png");
        }
    }
}
