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
