// Resampling wrapper. Rubato's SincFixedIn gives high quality at modest cost
// (~0.3% CPU on a 5800X for 48k→16k mono per the rubato readme). We process
// chunk-by-chunk from inside the cpal callback, so we need a streaming
// resampler that tolerates arbitrary input lengths.

use anyhow::Result;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

const CHUNK_IN: usize = 1024;

pub struct StreamingResampler {
    inner: Option<SincFixedIn<f32>>, // None when input_sr == output_sr (passthrough)
    input_buffer: Vec<f32>,
    output_scratch: Vec<f32>,
    output_buffer: Vec<f32>,
    ratio: f64,
}

impl StreamingResampler {
    pub fn new(input_sr: u32, output_sr: u32) -> Result<Self> {
        if input_sr == output_sr {
            return Ok(Self {
                inner: None,
                input_buffer: Vec::new(),
                output_scratch: Vec::new(),
                output_buffer: Vec::new(),
                ratio: 1.0,
            });
        }
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = output_sr as f64 / input_sr as f64;
        let inner = SincFixedIn::<f32>::new(ratio, 1.0, params, CHUNK_IN, 1)?;
        Ok(Self {
            inner: Some(inner),
            input_buffer: Vec::with_capacity(CHUNK_IN * 2),
            output_scratch: vec![0.0; (CHUNK_IN as f64 * ratio).ceil() as usize + 256],
            output_buffer: Vec::with_capacity(CHUNK_IN * 2),
            ratio,
        })
    }

    pub fn process(&mut self, mono_in: &[f32]) -> &[f32] {
        self.output_buffer.clear();
        let Some(resampler) = self.inner.as_mut() else {
            self.output_buffer.extend_from_slice(mono_in);
            return &self.output_buffer;
        };

        self.input_buffer.extend_from_slice(mono_in);
        while self.input_buffer.len() >= CHUNK_IN {
            // Borrow the chunk in place — no per-iteration heap allocation in
            // this RT-sensitive path. We drain afterward, once the borrow ends.
            let out_frames = {
                let input_slice = [&self.input_buffer[..CHUNK_IN]];
                let mut output_slice = [self.output_scratch.as_mut_slice()];
                match resampler.process_into_buffer(&input_slice, &mut output_slice, None) {
                    Ok((_, out_frames)) => Some(out_frames),
                    Err(e) => {
                        tracing::warn!(error = %e, "resampler chunk failed");
                        None
                    }
                }
            };
            if let Some(out_frames) = out_frames {
                self.output_buffer
                    .extend_from_slice(&self.output_scratch[..out_frames]);
            }
            self.input_buffer.drain(..CHUNK_IN);
        }
        &self.output_buffer
    }

    /// Drain the sub-chunk residual at end of stream.
    ///
    /// `process` only emits once a full `CHUNK_IN` has accumulated, so up to
    /// 1023 source frames are still held when the input ends. Live capture can
    /// ignore that — its tail is the silence between the last word and the
    /// hotkey release. A **Transcription run** cannot: a recording ends the
    /// instant the media does, so those frames are the end of the last word.
    ///
    /// The residual is padded with silence to make the fixed-size chunk the
    /// resampler demands, then the output is cut back to the residual's own
    /// share so the padding doesn't come back as appended silence.
    pub fn flush(&mut self) -> &[f32] {
        self.output_buffer.clear();
        let Some(resampler) = self.inner.as_mut() else {
            // Passthrough buffers nothing — `process` returned it all already.
            return &self.output_buffer;
        };
        let residual = self.input_buffer.len();
        if residual == 0 {
            return &self.output_buffer;
        }
        self.input_buffer.resize(CHUNK_IN, 0.0);
        let keep = (residual as f64 * self.ratio).round() as usize;
        let out_frames = {
            let input_slice = [&self.input_buffer[..CHUNK_IN]];
            let mut output_slice = [self.output_scratch.as_mut_slice()];
            match resampler.process_into_buffer(&input_slice, &mut output_slice, None) {
                Ok((_, out_frames)) => Some(out_frames),
                Err(e) => {
                    tracing::warn!(error = %e, "resampler flush failed");
                    None
                }
            }
        };
        if let Some(out_frames) = out_frames {
            self.output_buffer
                .extend_from_slice(&self.output_scratch[..out_frames.min(keep)]);
        }
        self.input_buffer.clear();
        &self.output_buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Input shorter than one chunk produces nothing until it is flushed —
    /// the case that silently truncated the end of a decoded file.
    #[test]
    fn flush_emits_the_sub_chunk_residual() {
        let mut r = StreamingResampler::new(32_000, 16_000).unwrap();
        let input = vec![0.25f32; 600];
        assert!(
            r.process(&input).is_empty(),
            "a partial chunk should stay buffered"
        );
        let flushed = r.flush().len();
        // 600 source frames at 2:1 is 300 out, give or take the sinc tail.
        assert!((295..=305).contains(&flushed), "flushed {flushed}");
    }

    /// Flushing twice must not replay the residual.
    #[test]
    fn flush_is_idempotent() {
        let mut r = StreamingResampler::new(32_000, 16_000).unwrap();
        r.process(&vec![0.25f32; 600]);
        assert!(!r.flush().is_empty());
        assert!(r.flush().is_empty());
    }

    /// Nothing buffered, nothing to drain — including the passthrough case,
    /// where `process` already returned everything.
    #[test]
    fn flush_of_an_empty_or_passthrough_resampler_is_empty() {
        let mut r = StreamingResampler::new(32_000, 16_000).unwrap();
        assert!(r.flush().is_empty());

        let mut passthrough = StreamingResampler::new(16_000, 16_000).unwrap();
        assert_eq!(passthrough.process(&[0.1, 0.2]), &[0.1, 0.2]);
        assert!(passthrough.flush().is_empty());
    }

    /// The whole point: the frames that didn't fill the last chunk come back
    /// out instead of being dropped. Rubato's sinc filter has its own latency,
    /// so the totals are approximate — what must be exact is that the
    /// half-chunk is *accounted for* rather than silently lost.
    #[test]
    fn process_plus_flush_covers_the_whole_input() {
        let mut r = StreamingResampler::new(32_000, 16_000).unwrap();
        // 2.5 chunks: two get processed, the half-chunk needs the flush.
        let input = vec![0.1f32; CHUNK_IN * 2 + 512];
        let processed = r.process(&input).len();
        let flushed = r.flush().len();

        // Before `flush` existed, this half-chunk was the silently truncated
        // tail — 512 source frames at 2:1, so ~256 out.
        assert!(
            (250..=262).contains(&flushed),
            "the residual contributed {flushed} samples, expected about 256"
        );
        let expected = input.len() / 2;
        let shortfall = expected - (processed + flushed);
        assert!(
            shortfall * 100 / expected < 5,
            "lost {shortfall} of {expected} samples"
        );
    }
}
