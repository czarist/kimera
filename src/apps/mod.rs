pub mod lightcraft;
pub mod photocraft;
pub mod vectorcraft;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::process::Command;

/// One of the Craft apps Kimera drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum App {
    LightCraft,
    PhotoCraft,
    VectorCraft,
}

impl App {
    /// In the order a photo usually meets them.
    pub const ALL: [App; 3] = [App::LightCraft, App::PhotoCraft, App::VectorCraft];

    /// The name in recipes, run folders and command lists: `lightcraft`.
    pub const fn id(self) -> &'static str {
        match self {
            App::LightCraft => "lightcraft",
            App::PhotoCraft => "photocraft",
            App::VectorCraft => "vectorcraft",
        }
    }

    /// The product name: `LightCraft`.
    pub const fn label(self) -> &'static str {
        match self {
            App::LightCraft => "LightCraft",
            App::PhotoCraft => "PhotoCraft",
            App::VectorCraft => "VectorCraft",
        }
    }
}

/// Writes the id (`lightcraft`), as folders and logs show it.
impl fmt::Display for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// A stage that stopped at one of its steps (index into the steps it was given), or at
/// opening / saving the file (`step: None`).
#[derive(Debug)]
pub struct StageFailed {
    pub step: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for StageFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StageFailed {}

/// `<cli> --version`, first line of its output.
pub fn version(cli: &Path) -> Result<String> {
    if !cli.is_file() {
        bail!("not found: {}", cli.display());
    }
    let o = crate::procrun::run(
        {
            let mut c = Command::new(cli);
            c.arg("--version");
            c
        },
        None,
        &std::sync::atomic::AtomicBool::new(false),
    )?;
    let text = if o.stdout.trim().is_empty() {
        o.stderr
    } else {
        o.stdout
    };
    Ok(text.lines().next().unwrap_or("").trim().to_owned())
}
