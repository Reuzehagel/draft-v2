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
use realfft::num_complex::Complex;
use realfft::RealFftPlanner;
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
    bars: Vec<f32>,
    out: Vec<f32>,
    phase: f32,
    bin_ranges: Vec<(usize, usize)>,
    window: Vec<f32>,
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    input_scratch: Vec<f32>,
    output_scratch: Vec<Complex<f32>>,
    /// The FFT's own working space, owned so a transform allocates nothing.
    fft_scratch: Vec<Complex<f32>>,
    /// Each band's level this tick, before easing.
    targets: Vec<f32>,
    last_tick: Option<std::time::Instant>,
}

impl BandMeter {
    pub fn new(n_bands: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let input_scratch = vec![0.0; FFT_SIZE];
        let output_scratch = fft.make_output_vec();
        let fft_scratch = fft.make_scratch_vec();

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
            bars: vec![0.0; n_bands],
            out: vec![0.0; n_bands],
            phase: 0.0,
            bin_ranges,
            window,
            fft,
            input_scratch,
            output_scratch,
            fft_scratch,
            targets: vec![0.0; n_bands],
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

        self.compute_band_targets(buffer);
        let attack_alpha = 1.0 - (-dt_ms / ATTACK_MS).exp();
        let release_alpha = 1.0 - (-dt_ms / RELEASE_MS).exp();
        for (cur, &target) in self.bars.iter_mut().zip(&self.targets) {
            let alpha = if target > *cur {
                attack_alpha
            } else {
                release_alpha
            };
            *cur += alpha * (target - *cur);
        }

        self.blend_idle();
        &self.out
    }

    /// Advance the meter with no new audio, holding the band levels where the
    /// last real audio left them.
    ///
    /// This is what the pill's handoff runs on. The capture's ring is drained
    /// into the worker the instant recording stops, so `tick` from there on
    /// would be easing toward the silence of an empty buffer — which collapses
    /// the row to the idle floor inside a third of the handoff and leaves the
    /// handoff's own ease nothing left to drain. Holding keeps the waveform the
    /// user was just watching on screen, and lets that ease be the only thing
    /// taking it down.
    ///
    /// The clock still advances, so the idle wave keeps travelling across the
    /// bars and the row reads as losing energy rather than stopping dead.
    pub fn hold(&mut self) -> &[f32] {
        let now = std::time::Instant::now();
        let dt_ms = self
            .last_tick
            .map(|t| now.duration_since(t).as_secs_f32() * 1000.0)
            .unwrap_or(33.0);
        self.last_tick = Some(now);
        self.phase += dt_ms / 1000.0;
        self.blend_idle();
        &self.out
    }

    /// Blend the idle wave into `out`: each bar gets a phase-shifted sine so the
    /// motion travels across the pill. max() means real speech always wins.
    fn blend_idle(&mut self) {
        for (i, &cur) in self.bars.iter().enumerate() {
            let phase_i = self.phase * IDLE_SPEED + i as f32 * IDLE_BAR_OFFSET;
            let wobble = IDLE_BASE + IDLE_WOBBLE * (phase_i.sin() * 0.5 + 0.5);
            self.out[i] = cur.max(wobble).clamp(0.0, 1.0);
        }
    }

    /// Fill `targets` from the tail of `buffer`. Into buffers the meter already
    /// owns, every one of them: this runs once per pill frame for as long as
    /// anyone is talking, so it allocates nothing (#106).
    fn compute_band_targets(&mut self, buffer: &Buffer) {
        // Zero-pad short captures by leaving leading zeros in the scratch.
        let (input, window) = (&mut self.input_scratch, &self.window);
        let have = buffer.with_tail(FFT_SIZE, |samples| {
            let pad = FFT_SIZE - samples.len();
            input[..pad].fill(0.0);
            for (i, &s) in samples.iter().enumerate() {
                input[pad + i] = s * window[pad + i];
            }
            samples.len()
        });
        if have == 0
            || self
                .fft
                .process_with_scratch(
                    &mut self.input_scratch,
                    &mut self.output_scratch,
                    &mut self.fft_scratch,
                )
                .is_err()
        {
            self.targets.fill(0.0);
            return;
        }

        let mags = &self.output_scratch;
        for (target, &(b0, b1)) in self.targets.iter_mut().zip(&self.bin_ranges) {
            let bins = &mags[b0.min(mags.len())..b1.min(mags.len())];
            *target = if bins.is_empty() {
                0.0
            } else {
                band_level(bins)
            };
        }
    }
}

/// One band's level, 0..1, from the FFT bins it spans.
fn band_level(bins: &[Complex<f32>]) -> f32 {
    let mean: f32 = bins.iter().map(|c| c.norm()).sum::<f32>() / bins.len() as f32;
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
}

/// Center-tall envelope applied as a multiplier on top of the raw band
/// values, from `raw` into `out` — the caller's buffer, so a frame's bars cost
/// no allocation. `out` takes as many bars as both have.
pub fn shape_bars(raw: &[f32], out: &mut [f32]) {
    let n = raw.len();
    for (i, (o, &v)) in out.iter_mut().zip(raw).enumerate() {
        let center = (n as f32 - 1.0) / 2.0;
        let d = (i as f32 - center).abs() / center.max(1.0);
        // Edge multiplier 0.85, center 1.0. Very subtle bias — we want
        // the per-band variation to dominate the silhouette.
        let mult = 1.0 - 0.15 * d;
        *o = (v * mult).clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BANDS: usize = 9;

    fn tone(hz: f32, amp: f32) -> Buffer {
        let b = Buffer::new(FFT_SIZE, FFT_SIZE * 4);
        let samples: Vec<f32> = (0..FFT_SIZE)
            .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / TARGET_SR as f32).sin())
            .collect();
        b.extend(&samples);
        b
    }

    fn targets(buffer: &Buffer) -> Vec<f32> {
        let mut m = BandMeter::new(BANDS);
        m.compute_band_targets(buffer);
        m.targets.clone()
    }

    fn loudest(levels: &[f32]) -> usize {
        (0..levels.len())
            .max_by(|&a, &b| levels[a].total_cmp(&levels[b]))
            .unwrap()
    }

    /// The bars are a spectrum, low on the left: a low voice lights the low
    /// bands, a high one the high bands, and each tone lights a band of its own.
    #[test]
    fn a_tone_lights_the_band_its_pitch_falls_in() {
        let low = targets(&tone(150.0, 0.3));
        let mid = targets(&tone(700.0, 0.3));
        let high = targets(&tone(3_000.0, 0.3));
        let (l, m, h) = (loudest(&low), loudest(&mid), loudest(&high));
        assert!(l < m && m < h, "bands {l}, {m}, {h} should rise with pitch");
        assert!(low[l] > 0.5, "a speaking-level tone reads well up the bar");
    }

    /// Silence, or no audio yet, raises no band — what the pill shows then is
    /// the idle wave alone.
    #[test]
    fn silence_raises_no_band() {
        assert!(targets(&tone(700.0, 0.0)).iter().all(|&t| t == 0.0));
        assert!(targets(&Buffer::new(0, 16)).iter().all(|&t| t == 0.0));
    }

    /// Louder speech, taller bars — up to the top and no further.
    #[test]
    fn a_louder_tone_reads_taller_and_never_past_full() {
        let quiet = targets(&tone(700.0, 0.01));
        let loud = targets(&tone(700.0, 0.3));
        let band = loudest(&loud);
        assert!(loud[band] > quiet[band]);
        let clipped = targets(&tone(700.0, 1.0));
        assert!(clipped.iter().all(|&t| (0.0..=1.0).contains(&t)));
    }
}
