pub mod lightcraft;
pub mod photocraft;
pub mod vectorcraft;

use anyhow::{Result, bail};
use std::path::Path;
use std::process::Command;

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
