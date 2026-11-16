pub mod lightcraft;
pub mod photocraft;

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// `<cli> --version`, first line of its output.
pub fn version(cli: &Path) -> Result<String> {
    if !cli.is_file() {
        bail!("not found: {}", cli.display());
    }
    let o = Command::new(cli)
        .arg("--version")
        .output()
        .with_context(|| format!("starting {}", cli.display()))?;
    let text = String::from_utf8_lossy(if o.stdout.is_empty() {
        &o.stderr
    } else {
        &o.stdout
    })
    .into_owned();
    Ok(text.lines().next().unwrap_or("").trim().to_owned())
}
