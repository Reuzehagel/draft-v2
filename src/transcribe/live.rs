// Every Provider on a real speech clip: what each one heard, its word error
// rate against the known transcript, and how long it took — once without the
// vocabulary hint and once with it. This is how a Provider is measured against
// the provider bar, and how a model bump is judged before it ships:
//
//     cargo test -- --ignored transcribe::live --nocapture
//
// `#[ignore]`d, like `pill::bench`: it spends API credit and its numbers depend
// on the network. A Provider with no key (or Parakeet with no model) is
// skipped; one with a key that errors fails the test, which is how a retired
// pinned model id shows up.
//
// The clip is `tests/fixtures/speech.flac`, synthetic English read by Windows
// TTS, with its text beside it in `speech.txt`. Synthetic speech is easy:
// real voices, accents and other languages are where Providers differ most.

use std::path::Path;
use std::time::Instant;

use crate::config::{Config, Provider};

const PROVIDERS: &[Provider] = &[
    Provider::LocalParakeet,
    Provider::Mistral,
    Provider::Groq,
    Provider::Openai,
    Provider::Elevenlabs,
    Provider::Reson8,
];

/// The clip's jargon, as a user would list it in their vocabulary.
const VOCABULARY: &[&str] = &["cargo", "clippy", "Parakeet", "tiny-skia", "Reuzehagel"];

#[test]
#[ignore = "live: spends API credit; run with -- --ignored transcribe::live --nocapture"]
fn live() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let samples = crate::decode::to_16k_mono(&fixtures.join("speech.flac")).unwrap();
    let truth = std::fs::read_to_string(fixtures.join("speech.txt")).unwrap();
    println!("truth: {}", truth.trim());

    let mut failures = Vec::new();
    for &provider in PROVIDERS {
        for vocabulary in [&[][..], VOCABULARY] {
            let cfg = Config {
                provider,
                vocabulary: vocabulary.iter().map(|t| t.to_string()).collect(),
                ..Config::default()
            };
            let hint = if vocabulary.is_empty() {
                "no hint"
            } else {
                "hint"
            };
            let label = format!("{:<18} {hint:<7}", provider.label());
            let Some(t) = super::build_primary(&cfg, super::DEFAULT_TIMEOUT) else {
                println!("{label} skipped: no key or model");
                break;
            };
            let start = Instant::now();
            match t.transcribe(&samples) {
                Ok(text) => println!(
                    "{label} WER {:>5.1}%  {:>5.2}s  {text}",
                    wer(&truth, &text) * 100.0,
                    start.elapsed().as_secs_f64(),
                ),
                Err(e) => {
                    println!("{label} ERROR {e:#}");
                    failures.push(format!("{label}: {e:#}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "Providers failed:\n{}",
        failures.join("\n")
    );
}

/// Word error rate: word-level edit distance over the truth's word count,
/// after lowercasing and splitting on anything not alphanumeric, so "Clippy,"
/// matches "clippy" and "tiny-skia" matches "tiny skia".
fn wer(truth: &str, heard: &str) -> f64 {
    fn words(s: &str) -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    }
    let (t, h) = (words(truth), words(heard));
    let mut row: Vec<usize> = (0..=h.len()).collect();
    for (i, tw) in t.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, hw) in h.iter().enumerate() {
            let next = (diag + usize::from(tw != hw))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[h.len()] as f64 / t.len().max(1) as f64
}

#[test]
fn wer_ignores_case_and_punctuation_and_counts_edits() {
    assert_eq!(wer("Run cargo clippy.", "run Cargo, Clippy"), 0.0);
    assert_eq!(wer("the tiny skia renderer", "the tiny-skia renderer"), 0.0);
    assert_eq!(wer("a b c d", "a x c"), 0.5);
}
