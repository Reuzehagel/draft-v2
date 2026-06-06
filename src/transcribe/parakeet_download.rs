// First-time download of the Parakeet ONNX bundle from HuggingFace.
//
// Four files, ~670 MB total. We stream each to disk with a .partial
// suffix and rename on success so a half-finished download never looks
// "ready". A caller-supplied progress callback fires for every chunk.

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;

const BASE_URL: &str =
    "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";

pub const REQUIRED_FILES: &[&str] = &[
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "nemo128.onnx",
    "vocab.txt",
];

pub fn model_dir() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("parakeet-tdt-0.6b-v3-int8"))
}

pub fn is_present() -> bool {
    let Ok(dir) = model_dir() else { return false };
    REQUIRED_FILES.iter().all(|f| dir.join(f).is_file())
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub file_index: usize,
    pub file_count: usize,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

/// Synchronous download. Run on a worker thread. `cb` is called frequently
/// so it should be cheap (e.g. update an Arc<Mutex<Progress>> or atomic).
pub fn download(cb: impl Fn(Progress)) -> Result<()> {
    let dir = model_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;

    let client = super::http_client(std::time::Duration::from_secs(60 * 30))?;

    for (i, name) in REQUIRED_FILES.iter().enumerate() {
        let progress = |bytes_done, bytes_total| Progress {
            file_index: i,
            file_count: REQUIRED_FILES.len(),
            bytes_done,
            bytes_total,
        };
        let final_path = dir.join(name);
        if final_path.is_file() {
            let len = final_path.metadata().ok().map(|m| m.len());
            cb(progress(len.unwrap_or(0), len));
            continue;
        }
        let url = format!("{}/{}", BASE_URL, name);
        let partial = dir.join(format!("{}.partial", name));

        let mut resp = client.get(&url).send().with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("download {url} failed: HTTP {}", resp.status()));
        }
        let total = resp.content_length();
        let mut file = std::fs::File::create(&partial)
            .with_context(|| format!("create {}", partial.display()))?;

        let mut buf = [0u8; 64 * 1024];
        let mut done: u64 = 0;
        let mut last_report = std::time::Instant::now();
        loop {
            let n = resp.read(&mut buf).context("read response chunk")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).context("write chunk")?;
            done += n as u64;
            if last_report.elapsed() > std::time::Duration::from_millis(100) {
                cb(progress(done, total));
                last_report = std::time::Instant::now();
            }
        }
        file.flush().ok();
        drop(file);

        // Reject a truncated download: if the server told us the length and we
        // got fewer bytes, the connection dropped mid-stream. Leaving the
        // .partial in place (not renaming to final) means is_present() stays
        // false and the next run retries instead of loading a corrupt model.
        if let Some(total) = total {
            if done != total {
                let _ = std::fs::remove_file(&partial);
                return Err(anyhow!(
                    "download {name} truncated: got {done} of {total} bytes"
                ));
            }
        }

        std::fs::rename(&partial, &final_path).with_context(|| {
            format!("rename {} -> {}", partial.display(), final_path.display())
        })?;
        cb(progress(done, total.or(Some(done))));
    }
    Ok(())
}
