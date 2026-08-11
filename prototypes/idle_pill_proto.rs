// PROTOTYPE — THROWAWAY. Not production code, not wired into `draft`.
//
// Answers wayfinder ticket "What the pill looks like doing nothing" (#17):
// idle dimensions, shape, opacity, and how much the resident pill shrinks
// relative to the 86x42 recording pill.
//
// Run:  cargo run --bin idle-pill-proto
// Then, in the terminal:
//   <Enter>  next variant
//   r        toggle the 86x42 recording pill for side-by-side comparison
//   q        quit
//
// It copies (rather than imports) the layered-window plumbing from
// `src/pill/window.rs` and the drawing from `src/pill/render.rs`, because the
// crate has no lib target. Delete this file once the decision is recorded.

#![cfg(windows)]

use anyhow::{anyhow, Result};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

// The window is always the size of the recording pill; every idle variant is
// drawn *inside* that box, bottom-aligned and centred, so the pill's bottom
// edge stays put at the same 80px margin no matter which variant is showing.
const BOX_W: u32 = 86;
const BOX_H: u32 = 42;
const BOTTOM_MARGIN: u32 = 80;
const SUPERSAMPLE: u32 = 4;

// The recording pill, for the shrink maths. Still 86x42 as shipped, even
// though HALF is the front-runner to replace it — the percentages stay
// comparable to round 1 that way.
const REC: Variant = Variant {
    name: "RECORDING 86x42 (today)",
    w: 86.0,
    h: 42.0,
    radius: 18.0,
    fill_a: 245,
    border_a: 64,
    bars: 7,
    note: "today's session pill, shown for comparison",
};

/// HALF, promoted from an idle candidate to the proposed *recording* size.
/// Cycled by `r` so every idle candidate can be judged against both the
/// current recording pill and the one that may replace it.
const REC_HALF: Variant = Variant {
    name: "RECORDING 62x28 (proposed: HALF)",
    w: 62.0,
    h: 28.0,
    radius: 14.0,
    fill_a: 245,
    border_a: 48,
    bars: 7,
    note: "HALF's silhouette carrying the full 7-bar recording treatment",
};

const REFERENCES: &[&Variant] = &[&REC, &REC_HALF];

struct Variant {
    name: &'static str,
    w: f32,
    h: f32,
    radius: f32,
    /// Alpha of the near-black body fill, 0-255. This is the "how much does it
    /// intrude" dial — it is what you are really judging here.
    fill_a: u8,
    /// Alpha of the hairline border, 0-255. 0 = no border at all.
    border_a: u8,
    /// Resting bars drawn inside; 0 = an empty shape.
    bars: usize,
    note: &'static str,
}

// Round 2. Round 1 narrowed the field to NUB and DOT, with a note that the nub
// wants to be less wide — so this walks the nub down in width and pits the two
// silhouettes against each other. GHOST, MINI-BARS and CAPSULE are out; HALF
// has moved up to be a recording reference.
const VARIANTS: &[Variant] = &[
    Variant {
        name: "NUB-44",
        w: 44.0,
        h: 10.0,
        radius: 5.0,
        fill_a: 140,
        border_a: 0,
        bars: 0,
        note: "round 1's nub, unchanged — the width you wanted to come down from",
    },
    Variant {
        name: "NUB-36",
        w: 36.0,
        h: 10.0,
        radius: 5.0,
        fill_a: 140,
        border_a: 0,
        bars: 0,
        note: "one step narrower; still clearly a bar, not a dash",
    },
    Variant {
        name: "NUB-30",
        w: 30.0,
        h: 10.0,
        radius: 5.0,
        fill_a: 140,
        border_a: 0,
        bars: 0,
        note: "3:1 — about where a bar stops reading as a bar",
    },
    Variant {
        name: "NUB-24",
        w: 24.0,
        h: 10.0,
        radius: 5.0,
        fill_a: 140,
        border_a: 0,
        bars: 0,
        note: "past the bar/blob line on purpose, to find where it broke",
    },
    Variant {
        name: "DOT-14",
        w: 14.0,
        h: 14.0,
        radius: 7.0,
        fill_a: 210,
        border_a: 0,
        bars: 0,
        note: "round 1's dot, unchanged",
    },
    Variant {
        name: "DOT-11",
        w: 11.0,
        h: 11.0,
        radius: 5.5,
        fill_a: 210,
        border_a: 0,
        bars: 0,
        note: "a smaller dot — is the dot's appeal its size or its shape?",
    },
];

enum Msg {
    Next,
    NextReference,
    Quit,
}

fn main() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line).is_err() {
                let _ = tx.send(Msg::Quit);
                return;
            }
            let msg = match line.trim() {
                "q" => Msg::Quit,
                "r" => Msg::NextReference,
                _ => Msg::Next,
            };
            let quit = matches!(msg, Msg::Quit);
            if tx.send(msg).is_err() || quit {
                return;
            }
        }
    });

    let el = EventLoop::new()?;
    el.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        win: None,
        rx,
        idx: 0,
        reference: None,
    };
    el.run_app(&mut app)?;
    Ok(())
}

struct App {
    win: Option<PillWindow>,
    rx: std::sync::mpsc::Receiver<Msg>,
    idx: usize,
    /// `None` = showing the idle candidate; `Some(i)` = showing REFERENCES[i].
    reference: Option<usize>,
}

impl App {
    fn current(&self) -> &'static Variant {
        match self.reference {
            Some(i) => REFERENCES[i],
            None => &VARIANTS[self.idx],
        }
    }

    /// Surface the full state on every change — the numbers are half the answer.
    fn report(&self) {
        let v = self.current();
        println!(
            "\n{}  —  {}\n  {:.0}x{:.0}  r={:.1}  fill_a={}  border_a={}  bars={}",
            v.name, v.note, v.w, v.h, v.radius, v.fill_a, v.border_a, v.bars,
        );
        if self.reference.is_none() {
            // Shrink against both recording sizes: the shipped one and the
            // proposed HALF, since which one wins changes the ratio.
            for r in REFERENCES {
                println!(
                    "  vs {}: {:.0}% width, {:.0}% height, {:.0}% area",
                    r.name,
                    v.w / r.w * 100.0,
                    v.h / r.h * 100.0,
                    (v.w * v.h) / (r.w * r.h) * 100.0,
                );
            }
        }
        println!("  [Enter] next idle   [r] cycle recording reference   [q] quit");
    }

    fn redraw(&mut self) {
        let v = self.current();
        if let Some(w) = self.win.as_mut() {
            if let Err(e) = w.render(v) {
                eprintln!("render failed: {e}");
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_none() {
            match PillWindow::create(el) {
                Ok(w) => {
                    w.show();
                    self.win = Some(w);
                    self.redraw();
                    self.report();
                }
                Err(e) => {
                    eprintln!("window creation failed: {e}");
                    el.exit();
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            el.exit();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Quit => {
                    el.exit();
                    return;
                }
                Msg::Next => {
                    self.reference = None;
                    self.idx = (self.idx + 1) % VARIANTS.len();
                }
                // none -> 86x42 -> 62x28 -> none
                Msg::NextReference => {
                    self.reference = match self.reference {
                        None => Some(0),
                        Some(i) if i + 1 < REFERENCES.len() => Some(i + 1),
                        Some(_) => None,
                    };
                }
            }
            self.redraw();
            self.report();
        }
        // Keep the layered surface alive without burning a core.
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw_variant(pm: &mut Pixmap, scale: f32, v: &Variant) {
    pm.fill(tiny_skia::Color::TRANSPARENT);

    let w = pm.width() as f32;
    let h = pm.height() as f32;
    let sw = v.w * scale;
    let sh = v.h * scale;

    // Half the stroke plus ~1px of transparent margin, so the anti-aliased
    // outer edge has somewhere to fade to and the curve doesn't stair-step.
    let border_w = (1.0 * scale).max(1.0);
    let m = border_w * 0.5 + 1.0 * scale;

    let x = (w - sw) / 2.0 + m;
    let y = h - sh + m;
    let rw = sw - 2.0 * m;
    let rh = sh - 2.0 * m;
    let r = (v.radius * scale).min(rh / 2.0);

    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, x, y, rw, rh, r);
    let Some(path) = pb.finish() else { return };

    let mut fill = Paint::default();
    fill.set_color_rgba8(13, 13, 13, v.fill_a);
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

    if v.border_a > 0 {
        let mut border = Paint::default();
        border.set_color_rgba8(170, 172, 178, v.border_a);
        border.anti_alias = true;
        let stroke = Stroke {
            width: border_w,
            ..Default::default()
        };
        pm.stroke_path(&path, &border, &stroke, Transform::identity(), None);
    }

    if v.bars > 0 {
        draw_resting_bars(pm, scale, v, x + rw / 2.0, y + rh / 2.0, rh);
    }
}

/// Idle bars are all at rest by definition — short rounded stubs, scaled down
/// with the shape so a 16px-tall variant doesn't get 42px-pill bars.
fn draw_resting_bars(pm: &mut Pixmap, scale: f32, v: &Variant, cx: f32, cy: f32, rh: f32) {
    let shrink = (v.h / REC.h).max(0.4);
    let bar_w = (2.5 * shrink * scale).max(1.5 * scale);
    let bar_gap = bar_w;
    let bh = (bar_w * 2.5).min(rh - 4.0 * scale);
    let n = v.bars as f32;
    let total = n * bar_w + (n - 1.0) * bar_gap;
    let start_x = cx - total / 2.0;

    let mut paint = Paint::default();
    // Dimmer than the recording bars: idle should not read as "listening".
    paint.set_color_rgba8(255, 255, 255, 170);
    paint.anti_alias = true;

    let mut pb = PathBuilder::new();
    for i in 0..v.bars {
        let x = start_x + i as f32 * (bar_w + bar_gap);
        rounded_rect(&mut pb, x, cy - bh / 2.0, bar_w, bh, bar_w / 2.0);
    }
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let r = r.min(w / 2.0).min(h / 2.0);
    if r <= 0.5 {
        if let Some(rect) = Rect::from_xywh(x, y, w, h) {
            pb.push_rect(rect);
        }
        return;
    }
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
}

fn pixmap_to_premul_bgra(pm: &Pixmap, dst: &mut [u8]) {
    let src = pm.data();
    let pixels = src.len().min(dst.len()) / 4;
    for i in 0..pixels {
        dst[i * 4] = src[i * 4 + 2];
        dst[i * 4 + 1] = src[i * 4 + 1];
        dst[i * 4 + 2] = src[i * 4];
        dst[i * 4 + 3] = src[i * 4 + 3];
    }
}

// ---------------------------------------------------------------------------
// Layered window (lifted from src/pill/window.rs)
// ---------------------------------------------------------------------------

struct PillWindow {
    window: Window,
    scale: f32,
    pixmap: Pixmap,
    hires: Pixmap,
    mid: Pixmap,
    layered: LayeredSurface,
}

impl PillWindow {
    fn create(el: &ActiveEventLoop) -> Result<Self> {
        let primary = el
            .primary_monitor()
            .or_else(|| el.available_monitors().next())
            .ok_or_else(|| anyhow!("no monitor available"))?;
        let scale = primary.scale_factor() as f32;
        let monitor_pos = primary.position();
        let monitor_size = primary.size();

        let phys_w = (BOX_W as f32 * scale) as i32;
        let phys_h = (BOX_H as f32 * scale) as i32;
        let margin = (BOTTOM_MARGIN as f32 * scale) as i32;
        let x = monitor_pos.x + (monitor_size.width as i32 - phys_w) / 2;
        let y = monitor_pos.y + monitor_size.height as i32 - phys_h - margin;

        let attrs = WindowAttributes::default()
            .with_title("Draft Idle Pill Prototype")
            .with_inner_size(LogicalSize::new(BOX_W, BOX_H))
            .with_position(LogicalPosition::new(
                x as f64 / scale as f64,
                y as f64 / scale as f64,
            ))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false);

        let window = el.create_window(attrs)?;
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let pixmap = Pixmap::new(w, h).ok_or_else(|| anyhow!("pixmap"))?;
        let hires =
            Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE).ok_or_else(|| anyhow!("hires"))?;
        let mid = Pixmap::new(w * 2, h * 2).ok_or_else(|| anyhow!("mid"))?;
        let layered = LayeredSurface::new(&window, w, h)?;

        Ok(Self {
            window,
            scale,
            pixmap,
            hires,
            mid,
            layered,
        })
    }

    fn show(&self) {
        self.window.set_visible(true);
    }

    fn render(&mut self, v: &Variant) -> Result<()> {
        draw_variant(&mut self.hires, self.scale * SUPERSAMPLE as f32, v);

        let paint = tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        };
        let half = tiny_skia::Transform::from_scale(0.5, 0.5);

        self.mid.fill(tiny_skia::Color::TRANSPARENT);
        self.mid
            .draw_pixmap(0, 0, self.hires.as_ref(), &paint, half, None);
        self.pixmap.fill(tiny_skia::Color::TRANSPARENT);
        self.pixmap
            .draw_pixmap(0, 0, self.mid.as_ref(), &paint, half, None);

        self.layered.present(&self.pixmap)
    }
}

struct LayeredSurface {
    hwnd: windows::Win32::Foundation::HWND,
    mem_dc: windows::Win32::Graphics::Gdi::HDC,
    dib: windows::Win32::Graphics::Gdi::HBITMAP,
    bits: *mut u8,
    w: u32,
    h: u32,
}

impl LayeredSurface {
    fn new(window: &Window, w: u32, h: u32) -> Result<Self> {
        let hwnd = hwnd_from_window(window)?;
        apply_layered_styles(hwnd);
        let (mem_dc, dib, bits) = create_dib(w, h)?;
        Ok(Self {
            hwnd,
            mem_dc,
            dib,
            bits,
            w,
            h,
        })
    }

    fn present(&mut self, pm: &Pixmap) -> Result<()> {
        let byte_count = self.w as usize * self.h as usize * 4;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, byte_count) };
        pixmap_to_premul_bgra(pm, dst);
        unsafe {
            if self.update_layered().is_err() {
                rearm_layered(self.hwnd);
                self.update_layered()?;
            }
        }
        Ok(())
    }

    unsafe fn update_layered(&self) -> Result<()> {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            GetDC, ReleaseDC, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

        let screen_dc = GetDC(None);
        let size = windows::Win32::Foundation::SIZE {
            cx: self.w as i32,
            cy: self.h as i32,
        };
        let src_pt = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let res = UpdateLayeredWindow(
            self.hwnd,
            screen_dc,
            None,
            Some(&size),
            self.mem_dc,
            Some(&src_pt),
            windows::Win32::Foundation::COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(None, screen_dc);
        res.map_err(|e| anyhow!("UpdateLayeredWindow: {e}"))
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteDC, DeleteObject};
        unsafe {
            if !self.mem_dc.is_invalid() {
                let _ = DeleteDC(self.mem_dc);
            }
            if !self.dib.is_invalid() {
                let _ = DeleteObject(self.dib);
            }
        }
    }
}

fn hwnd_from_window(window: &Window) -> Result<windows::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window
        .window_handle()
        .map_err(|e| anyhow!("window handle: {e}"))?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return Err(anyhow!("not a Win32 window"));
    };
    Ok(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _))
}

unsafe fn rearm_layered(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED,
    };
    let layered = WS_EX_LAYERED.0 as isize;
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !layered);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | layered);
}

fn apply_layered_styles(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = (cur as u32)
            | WS_EX_LAYERED.0
            | WS_EX_TRANSPARENT.0
            | WS_EX_NOACTIVATE.0
            | WS_EX_TOOLWINDOW.0
            | WS_EX_TOPMOST.0;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style as isize);
    }
}

fn create_dib(
    w: u32,
    h: u32,
) -> Result<(
    windows::Win32::Graphics::Gdi::HDC,
    windows::Win32::Graphics::Gdi::HBITMAP,
    *mut u8,
)> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, GetDC, ReleaseDC, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    unsafe {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(screen_dc);
        ReleaseDC(None, screen_dc);
        if mem_dc.is_invalid() {
            return Err(anyhow!("CreateCompatibleDC failed"));
        }
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(
            mem_dc,
            &bi as *const _,
            DIB_RGB_COLORS,
            &mut bits as *mut _,
            None,
            0,
        )
        .map_err(|e| anyhow!("CreateDIBSection: {e}"))?;
        if dib.is_invalid() || bits.is_null() {
            return Err(anyhow!("CreateDIBSection returned null"));
        }
        SelectObject(mem_dc, dib);
        Ok((mem_dc, dib, bits as *mut u8))
    }
}
