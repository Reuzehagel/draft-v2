// Resampling wrapper. Rubato's SincFixedIn gives high quality at modest cost
// (~0.3% CPU on a 5800X for 48k→16k mono per the rubato readme). We process
// chunk-by-chunk from inside the cpal callback, so we need a streaming
// resampler that tolerates arbitrary input lengths.

use anyhow::Result;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

const CHUNK_IN: usize = 1024;

pub struct StreamingResampler {
    inner: Option<SincFixedIn<f32>>, // None when input_sr == output_sr (passthrough)
    input_buffer: Vec<f32>,
    output_scratch: Vec<f32>,
    output_buffer: Vec<f32>,
}

impl StreamingResampler {
    pub fn new(input_sr: u32, output_sr: u32) -> Result<Self> {
        if input_sr == output_sr {
            return Ok(Self {
                inner: None,
                input_buffer: Vec::new(),
                output_scratch: Vec::new(),
                output_buffer: Vec::new(),
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
            let chunk: Vec<f32> = self.input_buffer.drain(..CHUNK_IN).collect();
            let input_slice = [chunk.as_slice()];
            let mut output_slice = [self.output_scratch.as_mut_slice()];
            match resampler.process_into_buffer(&input_slice, &mut output_slice, None) {
                Ok((_, out_frames)) => {
                    self.output_buffer
                        .extend_from_slice(&self.output_scratch[..out_frames]);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "resampler chunk failed");
                }
            }
        }
        &self.output_buffer
    }
}
