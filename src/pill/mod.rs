pub mod core;
pub mod geom;
pub mod hook;
pub mod monitor;
/// Renders the pill to PNGs for eyeballing. Test-only, and `#[ignore]`d — see
/// the module header for how to run it.
#[cfg(test)]
mod preview;
pub mod render;
pub mod window;

// The pill's every size lives in `geom`, which owns the shape of each mode.

/// The gap between the pill and the bottom of its home monitor's **work area**,
/// in logical pixels. Retuned from 80 when the anchor moved from the full
/// monitor rect to `rcWork` (#43): the old number was measured through a
/// taskbar, so the same visual gap is roughly a taskbar's height smaller now.
pub const PILL_BOTTOM_MARGIN: u32 = 24;
pub const BAR_COUNT: usize = 7;
