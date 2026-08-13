// cpal capture stream. Opened at session START, dropped (= stopped) at STOP.
//
// The callback does three things, in order: downmix to mono, resample to 16k,
// append to the shared buffer. We allocate inside the callback (Vec from the
// downmix, scratch grows in the resampler) — not strictly RT-safe but the
// allocations are small and amortized, and audio glitches under transient
// load are tolerable in a PTT app since transcription is post-hoc.

use crate::audio::{resample::StreamingResampler, ring::Buffer, MAX_SAMPLES, TARGET_SR};
use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct Capture {
    _stream: cpal::Stream,
    pub buffer: Buffer,
    pub input_sr: u32,
    pub input_channels: u16,
    pub device_name: String,
}

/// Names of the available input devices, for the Settings picker. Best-effort:
/// returns an empty list if the host can't enumerate.
pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate input devices");
            Vec::new()
        }
    }
}

/// Resolve the device to record from: the one whose name matches `preferred`,
/// else the system default. A configured-but-absent device (unplugged mic)
/// falls back to default rather than failing the session.
fn resolve_device(host: &cpal::Host, preferred: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = preferred {
        if let Ok(mut devices) = host.input_devices() {
            if let Some(d) = devices.find(|d| d.name().is_ok_and(|n| n == name)) {
                return Ok(d);
            }
        }
        tracing::warn!(device = %name, "preferred input device not found; using default");
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))
}

impl Capture {
    pub fn start(preferred: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = resolve_device(&host, preferred)?;
        let device_name = device.name().unwrap_or_else(|_| "unknown".into());
        let cfg = device.default_input_config()?;
        let input_sr = cfg.sample_rate().0;
        let input_channels = cfg.channels();
        let sample_format = cfg.sample_format();
        let stream_cfg: cpal::StreamConfig = cfg.into();

        tracing::info!(
            device = %device_name,
            input_sr,
            input_channels,
            ?sample_format,
            "opening input stream"
        );

        let buffer = Buffer::new(TARGET_SR as usize * 5, MAX_SAMPLES);
        let mut resampler = StreamingResampler::new(input_sr, TARGET_SR)?;
        let channels = input_channels as usize;
        let buffer_cb = buffer.clone();

        let err_fn = |err| tracing::error!(?err, "cpal stream error");

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &stream_cfg,
                move |data: &[f32], _: &_| {
                    let mono = downmix(data, channels);
                    let out = resampler.process(&mono);
                    buffer_cb.extend(out);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &stream_cfg,
                move |data: &[i16], _: &_| {
                    // Divide by 32768 (not i16::MAX = 32767) so the most-negative
                    // sample i16::MIN maps to exactly -1.0 and stays in [-1, 1].
                    let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                    let mono = downmix(&f, channels);
                    let out = resampler.process(&mono);
                    buffer_cb.extend(out);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_input_stream(
                &stream_cfg,
                move |data: &[u16], _: &_| {
                    let f: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                        .collect();
                    let mono = downmix(&f, channels);
                    let out = resampler.process(&mono);
                    buffer_cb.extend(out);
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("unsupported sample format: {other:?}")),
        };

        stream.play()?;
        Ok(Self {
            _stream: stream,
            buffer,
            input_sr,
            input_channels,
            device_name,
        })
    }
}

fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}
