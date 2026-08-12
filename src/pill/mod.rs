pub mod core;
pub mod hook;
pub mod render;
pub mod window;

// The session pill's settled size. The mid-size silhouette read better as the
// *recording* state than anything did as idle, so recording took it and the
// idle nub will be smaller again.
pub const PILL_W: u32 = 62;
pub const PILL_H: u32 = 28;
pub const PILL_BOTTOM_MARGIN: u32 = 80;
pub const BAR_COUNT: usize = 7;
