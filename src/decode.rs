// The decode seam: a media file in, 16 kHz mono `f32` out — the same shape
// cpal capture hands the rest of Draft, so everything downstream of a
// **Transcription run** is identical to a **Session**'s.
//
// Draft could not read a file before this: `hound` is WAV-only and used to
// *write* uploads, and every sample came from live capture. `symphonia` was
// chosen over Media Foundation for determinism across Windows installs and
// testability without COM — see
// docs/adr/0001-console-subcommand-in-a-second-binary.md, which also records
// that binary size was *not* the deciding factor.
//
// The duration cap is enforced twice on purpose. Container metadata is checked
// first so a two-hour video fails in milliseconds instead of after a full
// decode; the decoded frame count is checked again as packets arrive, because
// a header can lie (or be absent — a raw mp3 stream has no frame count).

use anyhow::{anyhow, Context};
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;
use symphonia::core::units::Timestamp;

use crate::audio::resample::StreamingResampler;
use crate::audio::TARGET_SR;

/// The longest media file a **Transcription run** will accept. Fixed, not
/// per-**Provider** and not configurable.
///
/// Draft uploads 16 kHz mono 16-bit audio — exactly 32,000 bytes/sec — so ten
/// minutes is ~19 MB, inside OpenAI's 25 MB ceiling with headroom. Anything
/// past this is the chunked long-form problem, which is deliberately a
/// separate project.
pub const MAX_DURATION: Duration = Duration::from_secs(10 * 60);

/// The container formats a **Transcription run** can read, for the message an
/// unreadable file earns. Mirrors the `symphonia` features in Cargo.toml.
pub const SUPPORTED_FORMATS: &str = "mp3, mp4, m4a, wav, flac, alac";

/// The file is longer than [`MAX_DURATION`]. Its own type rather than a
/// `DecodeError` variant so a caller that has already established which case
/// it is holding — `transcription_run::RunError::TooLong` — cannot end up
/// holding any other one.
#[derive(Debug)]
pub struct TooLong {
    /// A lower bound: the in-decode check stops counting the moment it passes
    /// the cap, so this is "at least this long", not a measurement.
    pub seconds: f64,
}

impl std::fmt::Display for TooLong {
    /// State the cap in minutes — a byte count or a sample count is not
    /// something the user can compare against their file.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "file is over the {} minute limit (about {:.1} minutes)",
            MAX_DURATION.as_secs() / 60,
            self.seconds / 60.0,
        )
    }
}

impl std::error::Error for TooLong {}

/// Why a file could not be turned into samples. The two variants exist
/// because they mean different things to a caller: over-length is a legible
/// limit the user can act on (trim the file), everything else is a failure.
#[derive(Debug)]
pub enum DecodeError {
    TooLong(TooLong),
    /// Unreadable, unsupported, or corrupt.
    Unreadable(anyhow::Error),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooLong(e) => write!(f, "{e}"),
            DecodeError::Unreadable(e) => {
                write!(f, "{e:#}\nsupported formats: {SUPPORTED_FORMATS}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode `path` to 16 kHz mono `f32` samples in [-1.0, 1.0].
///
/// Channels are averaged rather than dropped, so a file whose speech sits on
/// one side of a stereo mix still transcribes.
pub fn to_16k_mono(path: &Path) -> Result<Vec<f32>, DecodeError> {
    decode(path).map_err(|e| match e.downcast::<DecodeError>() {
        Ok(d) => d,
        Err(e) => DecodeError::Unreadable(e),
    })
}

fn decode(path: &Path) -> anyhow::Result<Vec<f32>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // The extension is only a hint — `symphonia` sniffs the actual container,
    // so a mislabelled file still reads and a `.txt` still fails.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .with_context(|| format!("read {}", path.display()))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("no audio track in {}", path.display()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("no audio codec parameters in {}", path.display()))?
        .clone();
    let source_sr = params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate in {}", path.display()))?;

    // First cap check, from the container. Free, and it is what makes a
    // two-hour video fail immediately rather than after a full decode.
    if let Some(seconds) = declared_seconds(track, source_sr) {
        if seconds > MAX_DURATION.as_secs_f64() {
            return Err(DecodeError::TooLong(TooLong { seconds }).into());
        }
    }

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .with_context(|| format!("no decoder for the audio in {}", path.display()))?;

    // The same resampler the capture path uses — arbitrary input rate in,
    // TARGET_SR out. There is exactly one resampler in Draft.
    let mut resampler = StreamingResampler::new(source_sr, TARGET_SR).context("build resampler")?;

    // Source frames, counted at the *source* rate: the cap is about the media,
    // not about how many samples we happen to have produced so far.
    let max_source_frames = (MAX_DURATION.as_secs_f64() * source_sr as f64) as u64;
    let mut source_frames: u64 = 0;

    let mut out: Vec<f32> = Vec::new();
    let mut interleaved: Vec<f32> = Vec::new();
    let mut mono: Vec<f32> = Vec::new();

    while let Some(packet) = next_packet(&mut *format)? {
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A single bad packet is not a bad file — players skip them, and
            // dropping one frame of audio beats refusing the transcript.
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::warn!(error = %e, "skipping undecodable packet");
                continue;
            }
            Err(e) => return Err(anyhow::Error::new(e).context("decode audio")),
        };

        source_frames += decoded.frames() as u64;
        // Second cap check: the header may have lied, or said nothing at all.
        if source_frames > max_source_frames {
            return Err(DecodeError::TooLong(TooLong {
                seconds: source_frames as f64 / source_sr as f64,
            })
            .into());
        }

        let channels = decoded.num_planes().max(1);
        decoded.copy_to_vec_interleaved(&mut interleaved);
        downmix(&interleaved, channels, &mut mono);
        out.extend_from_slice(resampler.process(&mono));
    }

    // A recording ends where the media ends, so the resampler's sub-chunk
    // residual is the end of the last word — not the release-latency silence
    // that live capture can afford to drop.
    out.extend_from_slice(resampler.flush());

    Ok(out)
}

/// `next_packet` returns `Ok(None)` at end of stream, but some readers signal
/// it as an unexpected-EOF io error instead. Both mean "done".
fn next_packet(
    format: &mut dyn symphonia::core::formats::FormatReader,
) -> anyhow::Result<Option<Packet>> {
    match format.next_packet() {
        Ok(p) => Ok(p),
        Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            Ok(None)
        }
        Err(e) => Err(anyhow::Error::new(e).context("read packet")),
    }
}

/// The track's length in seconds as the container states it, or `None` when it
/// doesn't say (a raw mp3 stream, for instance).
fn declared_seconds(track: &symphonia::core::formats::Track, sample_rate: u32) -> Option<f64> {
    if let Some(frames) = track.num_frames {
        if sample_rate > 0 {
            return Some(frames as f64 / sample_rate as f64);
        }
    }
    let duration = track.duration?;
    let time_base = track.time_base?;
    let ts = Timestamp::new(i64::try_from(duration.get()).ok()?);
    Some(time_base.calc_time(ts)?.as_secs() as f64)
}

/// Average interleaved frames down to mono, into a caller-owned buffer so a
/// long file doesn't allocate once per packet.
///
/// Averaging rather than taking the first channel: a file whose speech sits on
/// one side of a stereo mix would otherwise come back silent.
fn downmix(interleaved: &[f32], channels: usize, mono: &mut Vec<f32>) {
    mono.clear();
    if channels <= 1 {
        mono.extend_from_slice(interleaved);
        return;
    }
    mono.reserve(interleaved.len() / channels);
    for frame in interleaved.chunks_exact(channels) {
        mono.push(frame.iter().sum::<f32>() / channels as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message has to name the cap in a unit the user can compare their
    /// file against — minutes, not samples or bytes.
    #[test]
    fn too_long_names_the_cap_in_minutes() {
        let msg = TooLong { seconds: 754.0 }.to_string();
        assert!(msg.contains("12.6 minutes"), "{msg}");
        assert!(msg.contains("10 minute limit"), "{msg}");
    }

    #[test]
    fn unreadable_lists_the_supported_formats() {
        let msg = DecodeError::Unreadable(anyhow!("not a media file")).to_string();
        assert!(msg.contains("not a media file"), "{msg}");
        for fmt in ["mp3", "mp4", "m4a", "wav"] {
            assert!(msg.contains(fmt), "{msg} is missing {fmt}");
        }
    }

    /// Speech panned hard to one channel must survive the downmix — taking
    /// the first channel instead of averaging would return silence here.
    #[test]
    fn downmix_averages_rather_than_dropping_channels() {
        let mut mono = Vec::new();
        downmix(&[0.0, 1.0, 0.0, 0.5], 2, &mut mono);
        assert_eq!(mono, vec![0.5, 0.25]);
    }

    #[test]
    fn downmix_passes_mono_through() {
        let mut mono = vec![9.0; 3];
        downmix(&[0.1, 0.2], 1, &mut mono);
        assert_eq!(mono, vec![0.1, 0.2]);
    }
}
