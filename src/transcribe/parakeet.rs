// On-device Parakeet-TDT via transcribe-rs (ONNX Runtime backend).
// Loads four model files from a directory. ~1 GB resident RAM after load,
// CPU inference roughly 5-15x real-time on a typical Windows laptop.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Mutex;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};
use transcribe_rs::onnx::Quantization;

use super::Transcriber;

pub struct ParakeetTranscriber {
    model: Mutex<ParakeetModel>,
}

impl ParakeetTranscriber {
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model = ParakeetModel::load(model_dir, &Quantization::Int8)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("loading Parakeet from {}", model_dir.display()))?;
        Ok(Self { model: Mutex::new(model) })
    }
}

impl Transcriber for ParakeetTranscriber {
    fn name(&self) -> &'static str {
        "parakeet-local"
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let mut model = self.model.lock().unwrap();
        let out = model
            .transcribe_with(samples, &ParakeetParams::default())
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("Parakeet inference failed")?;
        Ok(out.text)
    }
}
