// On-device Parakeet-TDT via transcribe-rs (ONNX Runtime backend).
// Loads four model files from a directory. ~700 MB - 1 GB resident RAM while
// loaded, CPU inference roughly 5-15x real-time on a typical Windows laptop.
//
// The model is loaded lazily on the first `transcribe` call and released by
// `unload_if_idle` after a stretch of inactivity, so an idle tray app doesn't
// hold the model resident. The first dictation after an unload pays a one-time
// reload cost.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};
use transcribe_rs::onnx::Quantization;

use super::Transcriber;

pub struct ParakeetTranscriber {
    model_dir: PathBuf,
    /// `None` while the model is not resident in RAM.
    model: Mutex<Option<ParakeetModel>>,
    /// When the model was last used for inference; drives idle unloading.
    last_used: Mutex<Instant>,
}

impl ParakeetTranscriber {
    /// Prepare a transcriber without touching the model files. The model is
    /// pulled into RAM on the first `transcribe` call.
    pub fn new(model_dir: &Path) -> Self {
        Self {
            model_dir: model_dir.to_owned(),
            model: Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
        }
    }

    fn load_model(model_dir: &Path) -> Result<ParakeetModel> {
        ParakeetModel::load(model_dir, &Quantization::Int8)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("loading Parakeet from {}", model_dir.display()))
    }
}

impl Transcriber for ParakeetTranscriber {
    fn name(&self) -> &'static str {
        "parakeet-local"
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let mut guard = self.model.lock();
        if guard.is_none() {
            let started = Instant::now();
            tracing::info!("Parakeet model not resident; loading");
            *guard = Some(Self::load_model(&self.model_dir)?);
            tracing::info!(
                load_ms = started.elapsed().as_millis(),
                "Parakeet model loaded"
            );
        }
        let model = guard.as_mut().expect("model loaded above");
        let out = model
            .transcribe_with(samples, &ParakeetParams::default())
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("Parakeet inference failed")?;
        *self.last_used.lock() = Instant::now();
        Ok(out.text)
    }

    fn unload_if_idle(&self, timeout: Duration) {
        // try_lock: if a transcription is in flight it holds the model lock,
        // which by definition means the model isn't idle — skip this tick
        // rather than stalling the caller (the main event loop).
        let Some(mut guard) = self.model.try_lock() else {
            return;
        };
        if guard.is_none() {
            return;
        }
        if self.last_used.lock().elapsed() >= timeout {
            tracing::info!("Parakeet idle past timeout; unloading model to free memory");
            *guard = None; // drops ParakeetModel, releasing its resident RAM
        }
    }
}
