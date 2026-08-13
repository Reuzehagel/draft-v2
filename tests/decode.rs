// Decode-seam tests against real container files.
//
// The fixtures in `tests/fixtures/` are one second of a 440 Hz tone, stereo,
// 22.05 kHz, encoded once per format with ffmpeg and committed — decoding is
// the one part of a **Transcription run** that can be tested end to end
// without a provider or a network, and it is exactly the part Draft had never
// done before. `tone.mp4` deliberately carries a video track as well: the
// motivating case is "transcribe this video", so picking the audio track out
// of a muxed file is the behaviour under test, not an incidental detail.

use draft::decode::{self, DecodeError};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// One second in, one second out at 16 kHz — allowing for the resampler's
/// tail and the encoder delay/padding a lossy format carries.
fn assert_one_second_of_tone(samples: &[f32], what: &str) {
    // A lossy encoder adds priming and padding frames, so the band is wide
    // enough to cover them and the resampler's tail without hiding a
    // wrong-sample-rate bug, which would be off by a factor, not a percent.
    assert!(
        (14_000..=18_000).contains(&samples.len()),
        "{what}: expected ~16000 samples at 16 kHz, got {}",
        samples.len()
    );
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    assert!(rms > 0.1, "{what}: decoded to near-silence (rms {rms})");
    assert!(
        samples.iter().all(|s| (-1.0..=1.0).contains(s)),
        "{what}: samples outside [-1.0, 1.0]"
    );
}

#[test]
fn decodes_wav() {
    let samples = decode::to_16k_mono(&fixture("tone.wav")).unwrap();
    assert_one_second_of_tone(&samples, "wav");
}

#[test]
fn decodes_mp3() {
    let samples = decode::to_16k_mono(&fixture("tone.mp3")).unwrap();
    assert_one_second_of_tone(&samples, "mp3");
}

/// m4a is AAC in an MP4 container — the reason `aac` and `isomp4` are both
/// non-default `symphonia` features Draft has to opt into.
#[test]
fn decodes_m4a() {
    let samples = decode::to_16k_mono(&fixture("tone.m4a")).unwrap();
    assert_one_second_of_tone(&samples, "m4a");
}

/// The motivating case: a video file, whose audio track has to be found among
/// the others.
#[test]
fn decodes_mp4_with_a_video_track() {
    let samples = decode::to_16k_mono(&fixture("tone.mp4")).unwrap();
    assert_one_second_of_tone(&samples, "mp4");
}

#[test]
fn decodes_flac() {
    let samples = decode::to_16k_mono(&fixture("tone.flac")).unwrap();
    assert_one_second_of_tone(&samples, "flac");
}

/// ALAC is also an .m4a, so this is the format the extension hint can't
/// distinguish from `tone.m4a` — it has to be the codec that decides.
#[test]
fn decodes_alac() {
    let samples = decode::to_16k_mono(&fixture("tone-alac.m4a")).unwrap();
    assert_one_second_of_tone(&samples, "alac");
}

/// An unreadable file has to say what Draft *can* read, or the user is left
/// guessing which of their files to convert.
#[test]
fn unreadable_file_names_the_supported_formats() {
    let err = decode::to_16k_mono(&fixture("not-media.txt")).unwrap_err();
    assert!(matches!(err, DecodeError::Unreadable(_)), "{err:?}");
    let msg = err.to_string();
    for fmt in ["mp3", "mp4", "m4a", "wav"] {
        assert!(msg.contains(fmt), "{msg} is missing {fmt}");
    }
}

#[test]
fn missing_file_is_unreadable_not_a_panic() {
    let err = decode::to_16k_mono(&fixture("no-such-file.mp3")).unwrap_err();
    assert!(matches!(err, DecodeError::Unreadable(_)), "{err:?}");
}

/// Over the cap fails, and fails from the container's own frame count rather
/// than after decoding the audio: the fixture is written at 1 kHz so eleven
/// minutes is a megabyte, and the assertion below is that the whole thing is
/// rejected long before a two-hour video would have finished decoding.
#[test]
fn over_the_cap_is_rejected() {
    let dir = std::env::temp_dir().join("draft-decode-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("eleven-minutes.wav");

    let sample_rate = 1_000;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for _ in 0..(11 * 60 * sample_rate) {
        w.write_sample(0i16).unwrap();
    }
    w.finalize().unwrap();

    let err = decode::to_16k_mono(&path).unwrap_err();
    let DecodeError::TooLong(too_long) = err else {
        panic!("expected TooLong, got {err:?}");
    };
    assert!(
        (too_long.seconds - 660.0).abs() < 1.0,
        "reported {}s",
        too_long.seconds
    );
    // The message is what the user acts on, so it must name the cap in
    // minutes rather than in samples or bytes.
    let msg = too_long.to_string();
    assert!(msg.contains("10 minute limit"), "{msg}");

    let _ = std::fs::remove_file(&path);
}
