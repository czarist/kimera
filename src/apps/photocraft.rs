//! PhotoCraft through `photocraft-cli run <file> --cmd <id> --params <json> … --out <file>`.

use super::StageFailed;
use crate::procrun;
use crate::recipe::Step;
use anyhow::{Result, bail};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

/// The arguments for one file (kept separate so they can be tested without PhotoCraft).
pub fn run_args(input: &Path, out: &Path, steps: &[&Step]) -> Result<Vec<OsString>> {
    if input == out {
        bail!("refusing to overwrite the input {}", input.display());
    }
    let mut a: Vec<OsString> = vec!["run".into(), input.into()];
    for s in steps {
        a.push("--cmd".into());
        a.push((&s.cmd).into());
        a.push("--params".into());
        a.push(s.params_json()?.into());
    }
    a.push("--out".into());
    a.push(out.into());
    Ok(a)
}

/// Runs `steps` on `input` and saves the result to `out`. A failure is a [`StageFailed`]
/// naming the step that stopped it.
pub fn run(
    cli: &Path,
    input: &Path,
    out: &Path,
    steps: &[&Step],
    cancel: &AtomicBool,
) -> Result<()> {
    let mut cmd = Command::new(cli);
    cmd.args(run_args(input, out, steps)?);
    let o = procrun::run(cmd, None, cancel)?;
    if !o.status.success() {
        let done = succeeded_steps(&o.stdout);
        return Err(StageFailed {
            step: (done < steps.len()).then_some(done),
            message: format!(
                "PhotoCraft failed on {} ({}): {}",
                input.display(),
                o.status,
                o.stderr.trim()
            ),
        }
        .into());
    }
    if !out.is_file() {
        bail!("PhotoCraft reported success but wrote no {}", out.display());
    }
    Ok(())
}

/// `run` prints one `{"command", "result"}` line per command that succeeded.
fn succeeded_steps(stdout: &str) -> usize {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .filter(|v| v.get("command").is_some())
        .count()
}

/// An A4 page at 300 ppi, in pixels (portrait).
const A4_300PPI: (u32, u32) = (2480, 3508);

/// Client proof: A4 contact sheets (300 ppi, file names as captions), one PDF per page of
/// `columns × rows` photos, written as `<out_dir>/proof-01.pdf`, `proof-02.pdf`…
pub fn proof(
    cli: &Path,
    files: &[std::path::PathBuf],
    out_dir: &Path,
    columns: u32,
    rows: u32,
    cancel: &AtomicBool,
) -> Result<Vec<std::path::PathBuf>> {
    let per_page = (columns.max(1) * rows.max(1)) as usize;
    std::fs::create_dir_all(out_dir)?;
    let mut pages = Vec::new();
    for (i, chunk) in files.chunks(per_page).enumerate() {
        let out = out_dir.join(format!("proof-{:02}.pdf", i.saturating_add(1)));
        let params = serde_json::json!({
            "input": chunk, "units": "pixels", "width": A4_300PPI.0, "height": A4_300PPI.1, "resolution": 300,
            "columns": columns, "rows": rows, "caption": true, "flatten": true,
        });
        let mut cmd = Command::new(cli);
        cmd.args([
            "run",
            "--new",
            r#"{"width":100,"height":100}"#,
            "--cmd",
            "file.automate.contactSheetII",
            "--params",
        ])
        .arg(serde_json::to_string(&params)?)
        .arg("--out")
        .arg(&out);
        let o = procrun::run(cmd, None, cancel)?;
        procrun::check(&o, "PhotoCraft contact sheet")?;
        pages.push(out);
    }
    Ok(pages)
}

pub fn version(cli: &Path) -> Result<String> {
    super::version(cli)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::PhotoCraftStage;

    fn stage(toml_text: &str) -> Result<PhotoCraftStage> {
        Ok(toml::from_str(toml_text)?)
    }

    #[test]
    fn args_run_steps_in_order() -> Result<()> {
        let st = stage(
            "steps = [{ cmd = \"select.subject\" }, { cmd = \"x\", params = { a = true } }]\n",
        )?;
        let steps: Vec<&Step> = st.steps.iter().collect();
        let a = run_args(Path::new("in.tif"), Path::new("out.pcraft"), &steps)?;
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
        assert!(run_args(Path::new("a.tif"), Path::new("a.tif"), &[]).is_err());
        let _ = st;
        Ok(())
    }
}
