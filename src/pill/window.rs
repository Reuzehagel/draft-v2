// Pill window using a Win32 layered window with per-pixel alpha
// (UpdateLayeredWindow + AC_SRC_ALPHA). winit owns the HWND and the event
// loop; we hijack rendering — instead of WM_PAINT we push a DIB section
// through UpdateLayeredWindow each frame.
//
// This sidesteps the chroma-key bleed we got from softbuffer + LWA_COLORKEY.
//
// Never call a winit window mutator on the pill. winit's `WindowFlags::apply_diff`
// writes GWL_EXSTYLE *absolutely*, from a flag set that knows nothing about
// WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW or WS_EX_TRANSPARENT — so `set_visible`,
// `set_cursor_hittest` and friends silently drop the styles the pill depends on
// (notably NOACTIVATE, which is what keeps the pill from stealing focus).
// Everything that touches window state here goes through the raw HWND, and
// GWL_EXSTYLE is always read-modify-written.

use crate::pill::{PILL_BOTTOM_MARGIN, PILL_H, PILL_W};
use anyhow::{anyhow, Result};
use tiny_skia::Pixmap;

// Render the pill at this multiple of device resolution, then downscale to
// device size by halving twice (4×→2×→1×). Each halving is an exact 2×
// reduction, where bilinear sampling becomes a clean 2×2 box average — this
// avoids both bilinear's undersampling (when downscaling >2× in one shot) and
// bicubic's ringing halos at the high-contrast border edge. Must be a power of
// two so the halving chain lands exactly on device resolution.
const SUPERSAMPLE: u32 = 4;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes, WindowLevel};

pub struct PillWindow {
    pub window: Window,
    pub scale: f32,
    pixmap: Pixmap,
    hires: Pixmap,
    // Intermediate 2× buffer for the halving downscale chain.
    mid: Pixmap,
    #[cfg(windows)]
    layered: LayeredSurface,
}

impl PillWindow {
    pub fn create(el: &ActiveEventLoop) -> Result<Self> {
        let primary = el
            .primary_monitor()
            .or_else(|| el.available_monitors().next())
            .ok_or_else(|| anyhow!("no monitor available"))?;
        let scale = primary.scale_factor() as f32;
        let monitor_pos = primary.position();
        let monitor_size = primary.size();

        let pill_phys_w = (PILL_W as f32 * scale) as i32;
        let pill_phys_h = (PILL_H as f32 * scale) as i32;
        let margin_phys = (PILL_BOTTOM_MARGIN as f32 * scale) as i32;
        let x = monitor_pos.x + (monitor_size.width as i32 - pill_phys_w) / 2;
        let y = monitor_pos.y + monitor_size.height as i32 - pill_phys_h - margin_phys;

        let attrs = WindowAttributes::default()
            .with_title("Draft Pill")
            .with_inner_size(LogicalSize::new(PILL_W, PILL_H))
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
        let pixmap = Pixmap::new(w, h).ok_or_else(|| anyhow!("pixmap {w}x{h}"))?;
        let hires = Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE)
            .ok_or_else(|| anyhow!("hires pixmap"))?;
        let mid = Pixmap::new(w * 2, h * 2).ok_or_else(|| anyhow!("mid pixmap"))?;

        #[cfg(windows)]
        let layered = LayeredSurface::new(&window, w, h)?;

        Ok(Self {
            window,
            scale,
            pixmap,
            hires,
            mid,
            #[cfg(windows)]
            layered,
        })
    }

    pub fn show(&self) {
        #[cfg(windows)]
        unsafe {
            show_no_activate(self.layered.hwnd)
        };
        #[cfg(not(windows))]
        self.window.set_visible(true);
    }

    pub fn render_recording(&mut self, bar_heights: &[f32]) -> Result<()> {
        self.ensure_size()?;
        crate::pill::render::draw_recording(
            &mut self.hires,
            self.scale * SUPERSAMPLE as f32,
            bar_heights,
        );
        self.blit_and_present()
    }

    /// Render the post-capture success frame: a soft-green border over the
    /// frozen waveform bars, with `alpha` fading the whole pill out at the end.
    pub fn render_success(&mut self, bar_heights: &[f32], alpha: f32) -> Result<()> {
        self.ensure_size()?;
        crate::pill::render::draw_success(
            &mut self.hires,
            self.scale * SUPERSAMPLE as f32,
            bar_heights,
            alpha,
        );
        self.blit_and_present()
    }

    /// Render the failure frame: a muted-red border over the frozen bars,
    /// `alpha` fading it out at the end. Shown when the transcript couldn't be
    /// delivered, cueing the user to recover it from History.
    pub fn render_error(&mut self, bar_heights: &[f32], alpha: f32) -> Result<()> {
        self.ensure_size()?;
        crate::pill::render::draw_error(
            &mut self.hires,
            self.scale * SUPERSAMPLE as f32,
            bar_heights,
            alpha,
        );
        self.blit_and_present()
    }

    /// Render a "working" frame while transcription/paste runs: frozen bars
    /// under a neutral border that breathes via `pulse` (0..1).
    pub fn render_processing(&mut self, bar_heights: &[f32], pulse: f32) -> Result<()> {
        self.ensure_size()?;
        crate::pill::render::draw_processing(
            &mut self.hires,
            self.scale * SUPERSAMPLE as f32,
            bar_heights,
            pulse,
        );
        self.blit_and_present()
    }

    fn ensure_size(&mut self) -> Result<()> {
        let size = self.window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        if self.pixmap.width() != w || self.pixmap.height() != h {
            self.pixmap = Pixmap::new(w, h).ok_or_else(|| anyhow!("pixmap {w}x{h}"))?;
            self.hires = Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE)
                .ok_or_else(|| anyhow!("hires pixmap"))?;
            self.mid = Pixmap::new(w * 2, h * 2).ok_or_else(|| anyhow!("mid pixmap"))?;
            #[cfg(windows)]
            self.layered.resize(&self.window, w, h)?;
        }
        Ok(())
    }

    /// Downscale the hi-res buffer to device size by halving twice (4×→2×→1×),
    /// then push it through UpdateLayeredWindow. Each halving is an exact 2×
    /// reduction so bilinear acts as a clean box average — no undersampling, no
    /// ringing.
    fn blit_and_present(&mut self) -> Result<()> {
        let paint = tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        };
        let half = tiny_skia::Transform::from_scale(0.5, 0.5);

        // 4× → 2×
        self.mid.fill(tiny_skia::Color::TRANSPARENT);
        self.mid
            .draw_pixmap(0, 0, self.hires.as_ref(), &paint, half, None);

        // 2× → 1× (device)
        self.pixmap.fill(tiny_skia::Color::TRANSPARENT);
        self.pixmap
            .draw_pixmap(0, 0, self.mid.as_ref(), &paint, half, None);

        #[cfg(windows)]
        {
            self.layered.present(&self.pixmap)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
struct LayeredSurface {
    hwnd: windows::Win32::Foundation::HWND,
    mem_dc: windows::Win32::Graphics::Gdi::HDC,
    dib: windows::Win32::Graphics::Gdi::HBITMAP,
    bits: *mut u8,
    w: u32,
    h: u32,
}

#[cfg(windows)]
impl LayeredSurface {
    fn new(window: &Window, w: u32, h: u32) -> Result<Self> {
        let hwnd = hwnd_from_window(window)?;
        apply_layered_styles(hwnd)?;
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

    fn resize(&mut self, window: &Window, w: u32, h: u32) -> Result<()> {
        if self.w == w && self.h == h {
            return Ok(());
        }
        unsafe { self.destroy_gdi() };
        let _ = window;
        let (mem_dc, dib, bits) = create_dib(w, h)?;
        self.mem_dc = mem_dc;
        self.dib = dib;
        self.bits = bits;
        self.w = w;
        self.h = h;
        Ok(())
    }

    fn present(&mut self, pm: &Pixmap) -> Result<()> {
        let byte_count = self.w as usize * self.h as usize * 4;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, byte_count) };
        crate::pill::render::pixmap_to_premul_bgra(pm, dst);

        // Normally we just blit. Re-arming WS_EX_LAYERED briefly drops the
        // window out of per-pixel-alpha mode, so Windows flashes it as a plain
        // square for a frame — doing that every frame made the pill visibly
        // flicker between its rounded shape and a bare rectangle.
        //
        // UpdateLayeredWindow fails with E_INVALIDARG when WS_EX_LAYERED is
        // absent, and winit clears it: any window-state change it processes
        // (visibility, DPI, level) runs `WindowFlags::apply_diff`, which writes
        // GWL_EXSTYLE absolutely from a flag set that has no layered bit. So
        // re-arm ONLY on failure — and re-assert the whole pill set, since the
        // same write also took NOACTIVATE, TOOLWINDOW and TRANSPARENT with it.
        unsafe {
            if self.update_layered().is_err() {
                use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_EXSTYLE};
                tracing::debug!(
                    ex_style = format_args!("{:#x}", GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE)),
                    "layered present failed; re-arming pill ex-styles"
                );
                rearm_ex_styles(self.hwnd);
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

    unsafe fn destroy_gdi(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteDC, DeleteObject};
        // Delete the DC first: the DIB is still selected into it, and GDI
        // refuses to delete a selected bitmap. DeleteDC deselects it, so the
        // subsequent DeleteObject actually frees the DIB's backing memory
        // instead of leaking it on every resize.
        if !self.mem_dc.is_invalid() {
            let _ = DeleteDC(self.mem_dc);
        }
        if !self.dib.is_invalid() {
            let _ = DeleteObject(self.dib);
        }
    }
}

#[cfg(windows)]
impl Drop for LayeredSurface {
    fn drop(&mut self) {
        unsafe { self.destroy_gdi() }
    }
}

#[cfg(windows)]
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

/// The extended-style bits the pill owns. winit sets none of them and clears
/// all of them the moment one of its mutators runs, so they are re-asserted as
/// a set rather than one at a time.
#[cfg(windows)]
const PILL_EX_STYLE: u32 = {
    use windows::Win32::UI::WindowsAndMessaging::{
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };
    WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0
};

/// The pill's bits ORed onto whatever GWL_EXSTYLE currently holds — never an
/// absolute write, so bits Windows or winit set for their own reasons survive.
#[cfg(windows)]
fn with_pill_ex_style(cur: u32) -> u32 {
    cur | PILL_EX_STYLE
}

/// The same value with WS_EX_LAYERED knocked out, for the first half of the
/// re-arm (see [`rearm_ex_styles`]).
#[cfg(windows)]
fn without_layered(cur: u32) -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED;
    cur & !WS_EX_LAYERED.0
}

/// Re-assert every pill ex-style bit, dropping WS_EX_LAYERED first so the
/// window genuinely re-enters layered mode rather than seeing a no-op write.
#[cfg(windows)]
unsafe fn rearm_ex_styles(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
    };
    let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, without_layered(cur) as isize);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, with_pill_ex_style(cur) as isize);
}

#[cfg(windows)]
fn apply_layered_styles(hwnd: windows::Win32::Foundation::HWND) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
    };
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, with_pill_ex_style(cur) as isize);
    }
    Ok(())
}

/// Show the pill without letting it take focus, and without going through
/// winit — `Window::set_visible` funnels into `WindowFlags::apply_diff`, which
/// rewrites GWL_EXSTYLE absolutely and would drop every bit
/// [`apply_layered_styles`] just set.
#[cfg(windows)]
unsafe fn show_no_activate(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
}

#[cfg(windows)]
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

        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // Negative height = top-down DIB so byte order matches our tiny-skia row order.
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
        let _ = &mut bi;
        if dib.is_invalid() || bits.is_null() {
            return Err(anyhow!("CreateDIBSection returned null"));
        }
        SelectObject(mem_dc, dib);
        Ok((mem_dc, dib, bits as *mut u8))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT,
    };

    // The bit that keeps the pill from stealing focus is the whole reason the
    // clobber matters — it must be in the set we re-assert, not just LAYERED.
    #[test]
    fn the_pill_set_covers_every_style_the_pill_depends_on() {
        for bit in [
            WS_EX_LAYERED,
            WS_EX_TRANSPARENT,
            WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW,
            WS_EX_TOPMOST,
        ] {
            assert_eq!(PILL_EX_STYLE & bit.0, bit.0, "missing {bit:?}");
        }
    }

    // Read-modify-write, never an absolute write: bits set by Windows or winit
    // for their own reasons have to survive our re-assertion.
    #[test]
    fn applying_the_pill_set_preserves_foreign_bits() {
        let cur = WS_EX_APPWINDOW.0;
        assert_eq!(with_pill_ex_style(cur), cur | PILL_EX_STYLE);
    }

    #[test]
    fn applying_the_pill_set_is_idempotent() {
        let once = with_pill_ex_style(WS_EX_APPWINDOW.0);
        assert_eq!(with_pill_ex_style(once), once);
    }

    // The re-arm's first write must drop LAYERED and nothing else — clearing
    // NOACTIVATE for even one message would let the pill take focus.
    #[test]
    fn the_rearm_clears_only_the_layered_bit() {
        let cur = with_pill_ex_style(WS_EX_APPWINDOW.0);
        let cleared = without_layered(cur);
        assert_eq!(cleared & WS_EX_LAYERED.0, 0);
        assert_eq!(cleared, cur & !WS_EX_LAYERED.0);
        assert_eq!(with_pill_ex_style(cleared), cur);
    }
}
