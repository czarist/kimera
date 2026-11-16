//! `kimera.toml`: where the Craft apps live and where the pipeline writes.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub lightcraft: LightCraft,
    pub photocraft: PhotoCraft,
    #[serde(default)]
    pub vectorcraft: Option<VectorCraft>,
    pub work: Work,
}

#[derive(Debug, Deserialize)]
pub struct LightCraft {
    /// `lightcraft-cli` executable.
    pub cli: PathBuf,
    /// Control channel of a running `lightcraft --control <port>`.
    #[serde(default = "default_lc_control")]
    pub control: String,
}

#[derive(Debug, Deserialize)]
pub struct PhotoCraft {
    /// `photocraft-cli` executable.
    pub cli: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct VectorCraft {
    /// `vectorcraft-cli` executable.
    pub cli: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct Work {
    /// Each pipeline run gets its own folder in here.
    pub dir: PathBuf,
}

fn default_lc_control() -> String {
    "127.0.0.1:7980".to_owned()
}

/// `--config`, else `./kimera.toml`, else `kimera.toml` next to the executable.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let here = PathBuf::from("kimera.toml");
    if here.is_file() {
        return Ok(here);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        let beside = dir.join("kimera.toml");
        if beside.is_file() {
            return Ok(beside);
        }
    }
    anyhow::bail!(
        "no kimera.toml found: copy kimera.toml.example to kimera.toml (here or next to kimera.exe), or pass --config"
    )
}

pub fn load(path: &Path) -> Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("in {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    Ok(toml::from_str(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() -> Result<()> {
        let c = parse(include_str!("../kimera.toml.example"))?;
        assert_eq!(c.lightcraft.control, "127.0.0.1:7980");
        assert!(c.vectorcraft.is_some());
        Ok(())
    }

    #[test]
    fn missing_section_is_an_error_not_a_panic() {
        assert!(parse("[lightcraft]\ncli = 'x'\n").is_err());
    }
}
