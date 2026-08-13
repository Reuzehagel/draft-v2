// Pill window using a Win32 layered window with per-pixel alpha
// (UpdateLayeredWindow + AC_SRC_ALPHA). winit owns the HWND and the event
// loop; we hijack rendering — instead of WM_PAINT we push a DIB section
// through UpdateLayeredWindow each frame.
//
// This sidesteps the chroma-key bleed we got from softbuffer + LWA_COLORKEY.
//
// Never call a winit window mutator on the pill. winit's `WindowFlags::apply_diff`
// writes GWL_EXSTYLE *absolutely*, from its own flag set — which carries neither
// WS_EX_NOACTIVATE nor WS_EX_TOOLWINDOW at all, and reaches WS_EX_LAYERED and
// WS_EX_TRANSPARENT only via `set_cursor_hittest`, which ORs the pair on. So
// `set_visible` and friends silently drop the styles the pill depends on —
// notably NOACTIVATE, which is what keeps the pill from stealing focus — and
// `set_cursor_hittest` clears the layered bit as the price of hit-testing.
// That is why the click-through flip is a raw SetWindowLongPtrW (see #20).
//
// So `window` is private: everything that touches window state goes through the
// raw HWND, and GWL_EXSTYLE is always read-modify-written.
//
// WS_EX_NOACTIVATE also has a documented hover-to-activate hole, which the
// window's wndproc subclass closes by answering WM_MOUSEACTIVATE itself — see
// `pill::hook`.

use crate::pill::geom::Geom;
use crate::pill::hook::HookEvent;
use crate::pill::monitor::HomeMonitor;
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use tiny_skia::Pixmap;

// Render the pill at this multiple of device resolution, then downscale to
// device size by halving twice (4×→2×→1×). Each halving is an exact 2×
// reduction, where bilinear sampling becomes a clean 2×2 box average — this
// avoids both bilinear's undersampling (when downscaling >2× in one shot) and
// bicubic's ringing halos at the high-contrast border edge. Must be a power of
// two so the halving chain lands exactly on device resolution.
const SUPERSAMPLE: u32 = 4;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes, WindowLevel};

pub struct PillWindow {
    /// Held only for its `Drop` — and declared first so it runs first: the
    /// subclass has to come off while the HWND is still alive, and dropping
    /// `window` is what destroys it.
    #[cfg(windows)]
    #[allow(dead_code)]
    hook: crate::pill::hook::PillHook,
    window: Window,
    /// The monitor the pill lives on, and where every number below comes from:
    /// the window's rect and the resolution it renders at. Re-derived by the
    /// app loop's [`crate::pill::monitor::Home`] and handed here — never
    /// captured once at creation, which is what made the scale latently wrong
    /// on every mixed-DPI desk (#43).
    ///
    /// The scale is read off this rather than stored beside it: two copies of
    /// one number is one hand-sync away from the bug this ticket exists to fix.
    home: HomeMonitor,
    pixmap: Pixmap,
    hires: Pixmap,
    // Intermediate 2× buffer for the halving downscale chain.
    mid: Pixmap,
    /// The last frame drawn, so it can be pushed again when the system drops
    /// the layered surface. `None` before the first frame.
    last: Option<(Geom, Vec<f32>)>,
    #[cfg(windows)]
    layered: LayeredSurface,
}

impl PillWindow {
    /// Build the pill window on `home` and install its one wndproc hook.
    /// `hook_tx` is the app loop's end of that hook — the messages winit never
    /// surfaces arrive there for as long as this window lives.
    pub fn create(
        el: &ActiveEventLoop,
        hook_tx: Sender<HookEvent>,
        home: HomeMonitor,
    ) -> Result<Self> {
        // The window is the *envelope* — the largest mode's rect — at the home
        // monitor's scale, and it changes size only when that scale does. Every
        // mode is drawn centred inside it, so a morph from the nub to the
        // recording pill moves no window and reallocates no buffer; only pixels
        // change.
        //
        // Physical, not logical: winit resolves a logical size against a
        // scale factor of its own choosing, and on a mixed-DPI desk that is not
        // reliably the home monitor's.
        let rect = home.placement();

        let attrs = WindowAttributes::default()
            .with_title("Draft Pill")
            .with_inner_size(PhysicalSize::new(
                rect.width().max(1) as u32,
                rect.height().max(1) as u32,
            ))
            .with_position(PhysicalPosition::new(rect.left, rect.top))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false);

        let window = el.create_window(attrs)?;

        // Assert the rect once more against the raw HWND. `create_window` puts
        // the window somewhere close, but the exact client size it lands on is
        // negotiated with the DPI Windows thinks the window is on — which is
        // not necessarily the home monitor's until the window is actually there.
        #[cfg(windows)]
        {
            let hwnd = hwnd_from_window(&window)?;
            unsafe { place(hwnd, &home) };
        }

        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let pixmap = Pixmap::new(w, h).ok_or_else(|| anyhow!("pixmap {w}x{h}"))?;
        let hires = Pixmap::new(w * SUPERSAMPLE, h * SUPERSAMPLE)
            .ok_or_else(|| anyhow!("hires pixmap"))?;
        let mid = Pixmap::new(w * 2, h * 2).ok_or_else(|| anyhow!("mid pixmap"))?;

        #[cfg(windows)]
        let layered = LayeredSurface::new(&window, w, h)?;
        #[cfg(windows)]
        let hook = crate::pill::hook::PillHook::install(layered.hwnd, hook_tx);
        #[cfg(not(windows))]
        let _ = hook_tx;

        Ok(Self {
            #[cfg(windows)]
            hook,
            window,
            home,
            pixmap,
            hires,
            mid,
            last: None,
            #[cfg(windows)]
            layered,
        })
    }

    /// Move the pill to a new home monitor — or re-place it on the same one
    /// after its work area or DPI moved underneath.
    ///
    /// A **hard cut**: one window move, no animation. Animating across a bezel
    /// means animating through physical space that does not exist, and under
    /// the `cursor` policy the pill would be chasing a cursor that has already
    /// arrived.
    ///
    /// The window is moved **in place, never recreated**. Recreating would
    /// re-run the layered-window setup and the show path, flash, and hand winit
    /// another chance to clobber the ex-styles.
    ///
    /// A no-op when nothing about the home monitor changed, so the ordinary
    /// case of a re-derivation landing on the same answer costs nothing.
    pub fn set_home(&mut self, home: HomeMonitor) -> Result<()> {
        if self.home == home {
            return Ok(());
        }
        self.home = home;
        #[cfg(windows)]
        unsafe {
            place(self.layered.hwnd, &home)
        };
        // The rect just changed, so the buffers may be the wrong size and the
        // surface is certainly at the wrong resolution. Re-rendering the frame
        // already on screen does both — `render` runs `ensure_size` first.
        self.repush()
    }

    pub fn show(&self) {
        #[cfg(windows)]
        unsafe {
            show_no_activate(self.layered.hwnd)
        };
        #[cfg(not(windows))]
        self.window.set_visible(true);
    }

    /// Take the pill off screen without destroying it — the window survives so
    /// a later reveal doesn't have to rebuild a layered window.
    pub fn hide(&self) {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
            let _ = ShowWindow(self.layered.hwnd, SW_HIDE);
        };
        #[cfg(not(windows))]
        self.window.set_visible(false);
    }

    /// Draw one frame: whatever geometry the adapter's motion says the pill is
    /// at right now, with `bar_heights` across it.
    ///
    /// This is the only way pixels reach the screen. Modes have no renderers of
    /// their own — a frame mid-morph belongs to no mode, and the `Geom` is what
    /// expresses that.
    pub fn render(&mut self, geom: &Geom, bar_heights: &[f32]) -> Result<()> {
        self.ensure_size()?;
        crate::pill::render::draw(
            &mut self.hires,
            self.home.scale() * SUPERSAMPLE as f32,
            geom,
            bar_heights,
        );
        self.last = Some((*geom, bar_heights.to_vec()));
        self.blit_and_present()
    }

    /// Whether anything has been drawn yet. The adapter asks before revealing:
    /// a window shown with no frame in it is a blank rectangle.
    pub fn has_frame(&self) -> bool {
        self.last.is_some()
    }

    /// Push the surface the pill is already showing again, unchanged.
    ///
    /// The layered surface is normally maintained by the system: an idle nub is
    /// one `UpdateLayeredWindow` and then nothing, forever. Four things can
    /// invalidate it out from under us — the display topology changing, the DPI
    /// changing, the compositor being torn down and rebuilt around a lock, an
    /// RDP reconnect or a wake, and [`Self::set_home`] moving the window to a
    /// monitor at another scale. This is how the pill comes back from those,
    /// and it is not called for any other reason: a re-push per frame is
    /// exactly the idle cost residency exists to avoid.
    ///
    /// A home move is on that list because it *is* one of them, not despite
    /// being frequent: `set_home` returns early unless the home monitor
    /// actually changed, so a foreground window moving around one monitor
    /// re-pushes nothing at all.
    ///
    /// A no-op before the first frame — there is nothing to re-push yet.
    pub fn repush(&mut self) -> Result<()> {
        let Some((geom, bars)) = self.last.take() else {
            return Ok(());
        };
        let res = self.render(&geom, &bars);
        // `render` restores `last` on success; put it back if it didn't get
        // that far, so a failed re-push doesn't cost us the next one.
        if self.last.is_none() {
            self.last = Some((geom, bars));
        }
        res
    }

    /// Match the buffers to the window. The window is fixed at the envelope and
    /// never resized to run an animation, so in practice this only ever fires
    /// on a DPI change — a size-changing morph must not reallocate three
    /// pixmaps and a DIB section per frame.
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
        unsafe { apply_pill_ex_styles(hwnd) };
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

/// The extended-style bits the pill owns. winit clears all of them the moment
/// one of its mutators runs, so they are re-asserted as a set rather than one
/// at a time.
///
/// WS_EX_TOPMOST is carried here only so a re-assertion doesn't *drop* it —
/// setting the bit through SetWindowLongPtrW does not restack the window. The
/// actual z-order comes from `WindowLevel::AlwaysOnTop` at creation and would
/// need a SetWindowPos(HWND_TOPMOST) to restore if it were ever lost.
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
    // Re-read rather than reusing `cur`: the write above is itself a window
    // state change, and reusing the stale value would make the second write
    // absolute again — the very hazard this is here to close.
    apply_pill_ex_styles(hwnd);
}

/// OR the pill's ex-style bits onto whatever GWL_EXSTYLE holds right now.
#[cfg(windows)]
unsafe fn apply_pill_ex_styles(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
    };
    let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, with_pill_ex_style(cur) as isize);
}

/// Put the window at its home monitor's placement, size and all, in one call.
///
/// Raw `SetWindowPos` rather than `Window::set_outer_position`: winit's mutator
/// runs `WindowState::set_window_flags` on the way past, which is
/// `WindowFlags::apply_diff` — the absolute GWL_EXSTYLE write this module's
/// header exists to warn about. It would take NOACTIVATE and LAYERED with it.
///
/// Size travels with the position because a move between monitors is usually
/// also a scale change, and the two arriving as one call means the window is
/// never briefly the old size in the new place.
#[cfg(windows)]
unsafe fn place(hwnd: windows::Win32::Foundation::HWND, home: &HomeMonitor) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
    };
    let rect = home.placement();
    // HWND_TOPMOST rather than SWP_NOZORDER: the pill is always-on-top, and
    // this is the one call in its life that could quietly restack it.
    if let Err(e) = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        rect.left,
        rect.top,
        rect.width().max(1),
        rect.height().max(1),
        SWP_NOACTIVATE | SWP_NOOWNERZORDER,
    ) {
        tracing::error!(error = %e, "could not place the pill on its home monitor");
    }
}

/// Show the pill without letting it take focus, and without going through
/// winit — `Window::set_visible` funnels into `WindowFlags::apply_diff`, which
/// rewrites GWL_EXSTYLE absolutely and would drop every bit
/// [`apply_pill_ex_styles`] just set.
///
/// The cost is that winit's cached flags still say "hidden": it was created
/// `with_visible(false)` and nothing told winit otherwise. That is only safe
/// because nothing calls a winit mutator on the pill — `apply_diff` runs off
/// winit's own flag changes, so with no such calls there is no diff to apply.
/// Adding one would both clobber the ex-styles and hide the window.
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
