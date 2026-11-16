//! VectorCraft through `vectorcraft-cli run --in <file> --cmd <id> --params <json> … --export <file>`:
//! a poster or layout stage. It opens the photo itself, or a template whose steps place the
//! photo: any string parameter equal to `{photo}` becomes the current file's path.

use super::StageFailed;
use crate::procrun;
use crate::recipe::Step;
use anyhow::{Result, bail};
use serde_json::Value;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

pub fn run_args(
    input: &Path,
    template: Option<&Path>,
    out: &Path,
    steps: &[&Step],
) -> Result<Vec<OsString>> {
    if input == out || template == Some(out) {
        bail!("refusing to overwrite the input {}", out.display());
    }
    let photo = input
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{} is not valid UTF-8", input.display()))?;
    let mut a: Vec<OsString> = vec![
        "run".into(),
        "--in".into(),
        template.unwrap_or(input).into(),
    ];
    for s in steps {
        let mut params: Value = serde_json::from_str(&s.params_json()?)?;
        substitute(&mut params, photo);
        a.push("--cmd".into());
        a.push((&s.cmd).into());
        a.push("--params".into());
        a.push(serde_json::to_string(&params)?.into());
    }
    a.push("--export".into());
    a.push(out.into());
    Ok(a)
}

fn substitute(v: &mut Value, photo: &str) {
    match v {
        Value::String(s) if s == "{photo}" => *s = photo.to_owned(),
        Value::Array(a) => a.iter_mut().for_each(|x| substitute(x, photo)),
        Value::Object(o) => o.values_mut().for_each(|x| substitute(x, photo)),
        _ => {}
    }
}

pub fn run(
    cli: &Path,
    input: &Path,
    template: Option<&Path>,
    out: &Path,
    steps: &[&Step],
    cancel: &AtomicBool,
) -> Result<()> {
    let mut cmd = Command::new(cli);
    cmd.args(run_args(input, template, out, steps)?);
    let o = procrun::run(cmd, None, cancel)?;
    if !o.status.success() {
        return Err(StageFailed {
            step: failed_step(&o.stderr).filter(|i| *i < steps.len()),
            message: format!(
                "VectorCraft failed on {} ({}): {}",
                input.display(),
                o.status,
                o.stderr.trim()
            ),
        }
        .into());
    }
    if !out.is_file() {
        bail!(
            "VectorCraft reported success but wrote no {}",
            out.display()
        );
    }
    Ok(())
}

/// `vectorcraft-cli: step 3 (cmd.id): message`, steps counted from 0.
fn failed_step(stderr: &str) -> Option<usize> {
    let rest = stderr.split("step ").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photo_placeholder_is_replaced() -> Result<()> {
        let st: crate::recipe::VectorCraftStage = toml::from_str(
            "template = 'poster.ai'\nsteps = [{ cmd = \"file.place\", params = { path = \"{photo}\", fit = true } }]\n",
        )?;
        let steps: Vec<&Step> = st.steps.iter().collect();
        let a = run_args(
            Path::new("a.tif"),
            st.template.as_deref(),
            Path::new("o.pdf"),
            &steps,
        )?;
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(a[2], "poster.ai");
        assert_eq!(a[6], r#"{"fit":true,"path":"a.tif"}"#);
        Ok(())
    }

    #[test]
    fn step_number_is_read_from_the_error() {
        assert_eq!(failed_step("vectorcraft-cli: step 2 (x.y): bad"), Some(2));
        assert_eq!(failed_step("other"), None);
    }
}
