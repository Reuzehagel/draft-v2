pub mod capture;
pub mod level;
pub mod ring;
pub mod resample;

pub const TARGET_SR: u32 = 16_000;
pub const MAX_SECONDS: usize = 10 * 60;
pub const MAX_SAMPLES: usize = TARGET_SR as usize * MAX_SECONDS;
