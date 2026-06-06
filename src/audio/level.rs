// Per-band voice visualizer for the pill.
//
// Recipe (cribbed from LiveKit's useMultibandTrackVolume and Daniel Beer's
// FFT visualization article):
//   1. 1024-sample Hann-windowed FFT of the most recent audio.
//   2. Restrict bins to ~80 Hz – 4 kHz (speech range).
//   3. Log-space those bins into N bands. Each band's magnitude = sqrt of
//      the mean magnitude across its bins (sqrt for a "punchier" curve).
//   4. Map dB to [0, 1] over the speech range.
//   5. Per-bar exponential easing: attack ~50 ms, release ~150 ms,
//      independent per bar — this is the smooth-but-alive motion.
//   6. Apply a center-tall envelope as a *multiplier* in the renderer.

use crate::audio::ring::Buffer;
use crate::audio::TARGET_SR;
use realfft::RealFftPlanner;
use realfft::num_complex::Complex;
use std::sync::Arc;

const FFT_SIZE: usize = 1024;
const LO_HZ: f32 = 80.0;
const HI_HZ: f32 = 4000.0;
const NOISE_FLOOR_DB: f32 = -65.0;
const FULL_SCALE_DB: f32 = -15.0;
const ATTACK_MS: f32 = 40.0;
const RELEASE_MS: f32 = 110.0;

// "Listening" idle motion: a gentle traveling wave so the bars keep breathing
// during pauses instead of sitting still. The real signal overrides it the
// moment you actually speak (we take the max of the two).
const IDLE_BASE: f32 = 0.06; // resting height
const IDLE_WOBBLE: f32 = 0.10; // breathing amplitude
const IDLE_SPEED: f32 = 3.0; // radians/sec
const IDLE_BAR_OFFSET: f32 = 0.7; // phase shift per bar → wave travels across

pub struct BandMeter {
    n_bands: usize,
    bars: Vec<f32>,
    out: Vec<f32>,
    phase: f32,
    bin_ranges: Vec<(usize, usize)>,
    window: Vec<f32>,
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    input_scratch: Vec<f32>,
    output_scratch: Vec<Complex<f32>>,
    last_tick: Option<std::time::Instant>,
}

impl BandMeter {
    pub fn new(n_bands: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let input_scratch = vec![0.0; FFT_SIZE];
        let output_scratch = fft.make_output_vec();

        // Hann window — concentrates spectral energy, reduces leakage.
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let t = i as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
            })
            .collect();

        let bin_hz = TARGET_SR as f32 / FFT_SIZE as f32;
        let lo_bin = (LO_HZ / bin_hz).max(1.0) as usize;
        let hi_bin = ((HI_HZ / bin_hz) as usize).min(FFT_SIZE / 2);

        // Log-space bin boundaries.
        let mut bin_ranges = Vec::with_capacity(n_bands);
        let lo = lo_bin as f32;
        let hi = hi_bin as f32;
        for i in 0..n_bands {
            let t0 = i as f32 / n_bands as f32;
            let t1 = (i + 1) as f32 / n_bands as f32;
            let b0 = (lo * (hi / lo).powf(t0)).round() as usize;
            let b1 = (lo * (hi / lo).powf(t1)).round() as usize;
            let b0 = b0.max(lo_bin);
            let b1 = b1.max(b0 + 1);
            bin_ranges.push((b0, b1));
        }

        Self {
            n_bands,
            bars: vec![0.0; n_bands],
            out: vec![0.0; n_bands],
            phase: 0.0,
            bin_ranges,
            window,
            fft,
            input_scratch,
            output_scratch,
            last_tick: None,
        }
    }

    pub fn reset(&mut self) {
        for v in &mut self.bars {
            *v = 0.0;
        }
        for v in &mut self.out {
            *v = 0.0;
        }
        self.phase = 0.0;
        self.last_tick = None;
    }

    pub fn tick(&mut self, buffer: &Buffer) -> &[f32] {
        let now = std::time::Instant::now();
        let dt_ms = self
            .last_tick
            .map(|t| now.duration_since(t).as_secs_f32() * 1000.0)
            .unwrap_or(33.0);
        self.last_tick = Some(now);
        self.phase += dt_ms / 1000.0;

        let targets = self.compute_band_targets(buffer);
        let attack_alpha = 1.0 - (-dt_ms / ATTACK_MS).exp();
        let release_alpha = 1.0 - (-dt_ms / RELEASE_MS).exp();
        for (cur, &target) in self.bars.iter_mut().zip(targets.iter()) {
            let alpha = if target > *cur { attack_alpha } else { release_alpha };
            *cur += alpha * (target - *cur);
        }

        // Blend in the idle wave: each bar gets a phase-shifted sine so the
        // motion travels across the pill. max() means real speech always wins.
        for (i, &cur) in self.bars.iter().enumerate() {
            let phase_i = self.phase * IDLE_SPEED + i as f32 * IDLE_BAR_OFFSET;
            let wobble = IDLE_BASE + IDLE_WOBBLE * (phase_i.sin() * 0.5 + 0.5);
            self.out[i] = cur.max(wobble).clamp(0.0, 1.0);
        }
        &self.out
    }

    fn compute_band_targets(&mut self, buffer: &Buffer) -> Vec<f32> {
        let samples = buffer.snapshot_tail(FFT_SIZE);
        if samples.is_empty() {
            return vec![0.0; self.n_bands];
        }
        // Zero-pad short captures by leaving leading zeros in the scratch.
        let pad = FFT_SIZE - samples.len();
        for v in &mut self.input_scratch[..pad] {
            *v = 0.0;
        }
        for (i, &s) in samples.iter().enumerate() {
            self.input_scratch[pad + i] = s * self.window[pad + i];
        }
        if self.fft
            .process(&mut self.input_scratch, &mut self.output_scratch)
            .is_err()
        {
            return vec![0.0; self.n_bands];
        }

        let mags: Vec<f32> = self.output_scratch.iter().map(|c| c.norm()).collect();

        self.bin_ranges
            .iter()
            .map(|&(b0, b1)| {
                let slice = &mags[b0..b1.min(mags.len())];
                if slice.is_empty() {
                    return 0.0;
                }
                let mean: f32 = slice.iter().sum::<f32>() / slice.len() as f32;
                // Normalize: FFT magnitude scales with window sum/2 ≈ FFT_SIZE/4.
                let norm = mean / (FFT_SIZE as f32 * 0.25);
                if norm <= 1e-7 {
                    return 0.0;
                }
                let db = 20.0 * norm.log10();
                // Linear dB mapping (no sqrt). sqrt compressed the top end
                // and made loud bands all crowd toward 1.0, washing out
                // inter-band variation.
                ((db - NOISE_FLOOR_DB) / (FULL_SCALE_DB - NOISE_FLOOR_DB)).clamp(0.0, 1.0)
            })
            .collect()
    }
}

/// Center-tall envelope applied as a multiplier on top of the raw band
/// values. Returns a brand new Vec the renderer can consume.
pub fn shape_bars(raw: &[f32]) -> Vec<f32> {
    let n = raw.len();
    if n == 0 {
        return Vec::new();
    }
    raw.iter()
        .enumerate()
        .map(|(i, &v)| {
            let center = (n as f32 - 1.0) / 2.0;
            let d = (i as f32 - center).abs() / center.max(1.0);
            // Edge multiplier 0.85, center 1.0. Very subtle bias — we want
            // the per-band variation to dominate the silhouette.
            let mult = 1.0 - 0.15 * d;
            (v * mult).clamp(0.0, 1.0)
        })
        .collect()
}
