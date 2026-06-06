// Pill window using a Win32 layered window with per-pixel alpha
// (UpdateLayeredWindow + AC_SRC_ALPHA). winit owns the HWND and the event
// loop; we hijack rendering — instead of WM_PAINT we push a DIB section
// through UpdateLayeredWindow each frame.
//
// This sidesteps the chroma-key bleed we got from softbuffer + LWA_COLORKEY.

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
        let byte_count = (self.w * self.h * 4) as usize;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, byte_count) };
        crate::pill::render::pixmap_to_premul_bgra(pm, dst);

        // Normally we just blit. Re-arming WS_EX_LAYERED briefly drops the
        // window out of per-pixel-alpha mode, so Windows flashes it as a plain
        // square for a frame — doing that every frame made the pill visibly
        // flicker between its rounded shape and a bare rectangle.
        //
        // winit's message pump can still re-enter `SetLayeredWindowAttributes`
        // on window state changes (visibility, focus, DPI), which is mutually
        // exclusive with UpdateLayeredWindow's per-pixel-alpha mode and makes
        // it fail with E_INVALIDARG. So re-arm ONLY on failure, then retry.
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

    unsafe fn destroy_gdi(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteDC, DeleteObject};
        if !self.dib.is_invalid() {
            let _ = DeleteObject(self.dib);
        }
        if !self.mem_dc.is_invalid() {
            let _ = DeleteDC(self.mem_dc);
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

#[cfg(windows)]
unsafe fn rearm_layered(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED,
    };
    let layered = WS_EX_LAYERED.0 as isize;
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !layered);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | layered);
}

#[cfg(windows)]
fn apply_layered_styles(hwnd: windows::Win32::Foundation::HWND) -> Result<()> {
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
    Ok(())
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
