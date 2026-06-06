// Transcriber trait + WAV helpers for providers that need an encoded
// payload (cloud uploaders). Local providers can take the raw f32 slice
// directly via the trait's `transcribe` entry point.

pub mod mistral;
pub mod parakeet;
pub mod parakeet_download;
pub mod reson8;

use anyhow::Result;
use std::io::Cursor;

pub trait Transcriber: Send + Sync + 'static {
    /// 16 kHz mono f32 PCM in [-1.0, 1.0].
    fn transcribe(&self, samples: &[f32]) -> Result<String>;
    fn name(&self) -> &'static str;

    /// Release any heavy resident state (e.g. an on-device model held in RAM)
    /// if it has gone unused for at least `timeout`. Called periodically from
    /// the main loop. Default: no-op — cloud providers hold nothing resident.
    fn unload_if_idle(&self, _timeout: std::time::Duration) {}
}

pub fn samples_to_wav_bytes(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(samples.len() * 2 + 44);
    {
        let cursor = Cursor::new(&mut buf);
        let mut w = hound::WavWriter::new(cursor, spec)?;
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            let v = (clamped * i16::MAX as f32) as i16;
            w.write_sample(v)?;
        }
        w.finalize()?;
    }
    Ok(buf)
}
