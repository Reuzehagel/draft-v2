pub mod core;
pub mod geom;
pub mod hook;
pub mod render;
pub mod window;

// The pill's every size lives in `geom`, which owns the shape of each mode.
pub const PILL_BOTTOM_MARGIN: u32 = 80;
pub const BAR_COUNT: usize = 7;
