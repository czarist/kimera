//! PhotoCraft through `photocraft-cli run <file> --cmd <id> --params <json> … --out <file>`.

use crate::recipe::PhotoCraftStage;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// The arguments for one file (kept separate so they can be tested without PhotoCraft).
pub fn run_args(input: &Path, out: &Path, stage: &PhotoCraftStage) -> Result<Vec<OsString>> {
    if input == out {
        bail!("refusing to overwrite the input {}", input.display());
    }
    let mut a: Vec<OsString> = vec!["run".into(), input.into()];
    for s in &stage.steps {
        a.push("--cmd".into());
        a.push((&s.cmd).into());
        a.push("--params".into());
        a.push(s.params_json()?.into());
    }
    a.push("--out".into());
    a.push(out.into());
    Ok(a)
}

/// Runs the stage on `input` and saves the result to `out`.
pub fn run(cli: &Path, input: &Path, out: &Path, stage: &PhotoCraftStage) -> Result<()> {
    let args = run_args(input, out, stage)?;
    let o = Command::new(cli)
        .args(&args)
        .output()
        .with_context(|| format!("starting {}", cli.display()))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        bail!(
            "PhotoCraft failed on {} ({}): {}",
            input.display(),
            o.status,
            err.trim()
        );
    }
    Ok(())
}

pub fn version(cli: &Path) -> Result<String> {
    super::version(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(toml_text: &str) -> Result<PhotoCraftStage> {
        Ok(toml::from_str(toml_text)?)
    }

    #[test]
    fn args_run_steps_in_order() -> Result<()> {
        let st = stage(
            "steps = [{ cmd = \"select.subject\" }, { cmd = \"x\", params = { a = true } }]\n",
        )?;
        let a = run_args(Path::new("in.tif"), Path::new("out.pcraft"), &st)?;
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(
            a,
            [
                "run",
                "in.tif",
                "--cmd",
                "select.subject",
                "--params",
                "{}",
                "--cmd",
                "x",
                "--params",
                r#"{"a":true}"#,
                "--out",
                "out.pcraft"
            ]
        );
        Ok(())
    }

    #[test]
    fn never_overwrites_the_input() -> Result<()> {
        let st = stage("")?;
        assert!(run_args(Path::new("a.tif"), Path::new("a.tif"), &st).is_err());
        Ok(())
    }
}
