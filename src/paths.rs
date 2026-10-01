use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn config_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("no base dirs")?;
    let p = base.config_dir().join("Draft");
    std::fs::create_dir_all(&p).ok();
    Ok(p)
}

pub fn data_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("no base dirs")?;
    let p = base.data_local_dir().join("Draft");
    std::fs::create_dir_all(&p).ok();
    Ok(p)
}

pub fn log_dir() -> Result<PathBuf> {
    let p = data_dir()?.join("logs");
    std::fs::create_dir_all(&p).ok();
    Ok(p)
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn models_dir() -> Result<PathBuf> {
    let p = data_dir()?.join("models");
    std::fs::create_dir_all(&p).ok();
    Ok(p)
}

/// Write `contents` to `path` atomically: write a sibling temp file, then
/// rename it over the target so a crash mid-write can never leave a
/// truncated/empty file in place.
pub fn atomic_write(path: &std::path::Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Config and history are both rewritten through here, over a file that
    /// already exists — the rename has to replace it, not fail on it, and the
    /// temp sibling must not be left lying next to the user's config.
    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = std::env::temp_dir().join("draft-paths-tests");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "hotkey = \"old\"\nprovider = \"groq\"\n").unwrap();

        atomic_write(&path, "hotkey = \"new\"\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hotkey = \"new\"\n"
        );
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["config.toml"]);
    }
}
