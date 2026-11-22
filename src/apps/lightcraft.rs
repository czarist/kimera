//! LightCraft two ways: headless on one file (`lightcraft-cli run --import <file> …`), and the
//! control channel of a running app, JSON lines on a loopback TCP port
//! (`lightcraft --control 7980`; see LightCraft's docs/control-protocol.md).

use super::StageFailed;
use crate::procrun;
use crate::recipe::{FileExport, Step};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// LightCraft caps a reply at 64 MiB before the newline.
const MAX_REPLY: u64 = 64 * 1024 * 1024 + 1;

/// Characters of arguments beyond which the steps go to `--script -` on stdin instead: a Windows
/// command line holds 32 767, and a stored SAM 3 segmentation alone is tens of thousands.
const ARGS_BUDGET: usize = 24_000;

pub struct Control {
    addr: SocketAddr,
    next_id: u64,
}

impl Control {
    pub fn new(addr: &str) -> Result<Self> {
        let addr = addr
            .to_socket_addrs()
            .with_context(|| format!("bad LightCraft control address {addr}"))?
            .next()
            .ok_or_else(|| anyhow!("LightCraft control address {addr} resolves to nothing"))?;
        if !addr.ip().is_loopback() {
            bail!("LightCraft's control channel is loopback-only; {addr} is not");
        }
        Ok(Self { addr, next_id: 1 })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Whether something listens on the control port (a running `lightcraft --control`).
    pub fn answers(&self) -> bool {
        TcpStream::connect_timeout(&self.addr, Duration::from_millis(500)).is_ok()
    }

    /// One request, one reply. Exports can take minutes, hence the long read timeout.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let mut s =
            TcpStream::connect_timeout(&self.addr, Duration::from_secs(3)).with_context(|| {
                format!(
                    "LightCraft is not answering on {} (start it with `lightcraft --control {}`)",
                    self.addr,
                    self.addr.port()
                )
            })?;
        s.set_read_timeout(Some(Duration::from_secs(30 * 60)))?;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut line = serde_json::to_vec(&json!({"id": id, "method": method, "params": params}))?;
        line.push(b'\n');
        s.write_all(&line)?;
        let mut reply = String::new();
        BufReader::new(s.take(MAX_REPLY)).read_line(&mut reply)?;
        parse_reply(&reply, id)
    }

    pub fn execute(&mut self, command: &str, params: Value) -> Result<Value> {
        self.call(
            "engine.execute",
            json!({"command": command, "params": params}),
        )
    }

    /// Ids of the photos selected in the running app.
    pub fn selection(&mut self) -> Result<Vec<u64>> {
        let ui = self.call("ui.inspect", json!({}))?;
        let ids: Vec<u64> = ui
            .get("selection")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        Ok(ids)
    }

    /// Runs `app.export` and returns the files it wrote.
    pub fn export(&mut self, params: Value) -> Result<Vec<PathBuf>> {
        let r = self.execute("app.export", params)?;
        exported_files(&r)
    }
}

fn parse_reply(reply: &str, id: u64) -> Result<Value> {
    if !reply.ends_with('\n') {
        bail!("LightCraft closed the connection or sent an incomplete reply");
    }
    let v: Value = serde_json::from_str(reply.trim_end())
        .context("LightCraft sent a reply that is not JSON")?;
    if let Some(got) = v.get("id").and_then(Value::as_u64)
        && got != id
    {
        bail!("LightCraft answered request {got}, expected {id}");
    }
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        Some(false) => {
            let e = v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("LightCraft: {e}")
        }
        None => bail!("LightCraft sent a reply without `ok`"),
    }
}

fn exported_files(r: &Value) -> Result<Vec<PathBuf>> {
    let files = r
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("app.export returned no `files` list: {r}"))?;
    Ok(files
        .iter()
        .filter_map(|f| f.get("path").and_then(Value::as_str))
        .map(PathBuf::from)
        .collect())
}

/// `lightcraft-cli run --import <input> <cmd> <json>… app.export <json>`: a throwaway headless
/// session, so the user's library is never touched.
pub fn run_args(
    input: &Path,
    out: &Path,
    steps: &[&Step],
    export: &FileExport,
) -> Result<Vec<OsString>> {
    if input == out {
        bail!("refusing to overwrite the input {}", input.display());
    }
    let mut a: Vec<OsString> = vec!["run".into(), "--import".into(), input.into()];
    for s in steps {
        a.push((&s.cmd).into());
        a.push(s.params_json()?.into());
    }
    a.push("app.export".into());
    a.push(serde_json::to_string(&export.params(out)?)?.into());
    Ok(a)
}

/// Runs `steps` on `input` and exports `out`. A failure is a [`StageFailed`] naming the step
/// that stopped it (`None`: the import or the export).
pub fn run_file(
    cli: &Path,
    input: &Path,
    out: &Path,
    steps: &[&Step],
    export: &FileExport,
    cancel: &AtomicBool,
) -> Result<()> {
    let args = run_args(input, out, steps, export)?;
    let mut cmd = Command::new(cli);
    let stdin = if args.iter().map(|a| a.len()).sum::<usize>() > ARGS_BUDGET {
        cmd.args(["run", "--import"])
            .arg(input)
            .args(["--script", "-"]);
        Some(script_of(out, steps, export)?)
    } else {
        cmd.args(args);
        None
    };
    let o = procrun::run(cmd, stdin, cancel)?;
    if let Some((i, msg)) = first_failure(&o.stdout) {
        return Err(StageFailed {
            step: (i < steps.len()).then_some(i),
            message: format!("LightCraft failed on {}: {msg}", input.display()),
        }
        .into());
    }
    if !o.status.success() {
        return Err(StageFailed {
            step: None,
            message: format!(
                "LightCraft failed on {} ({}): {}",
                input.display(),
                o.status,
                o.stderr.trim()
            ),
        }
        .into());
    }
    if !out.is_file() {
        bail!("LightCraft reported success but wrote no {}", out.display());
    }
    Ok(())
}

/// The same commands as [`run_args`], as `--script` JSON lines.
fn script_of(out: &Path, steps: &[&Step], export: &FileExport) -> Result<String> {
    let mut s = String::new();
    for st in steps {
        let params: Value = serde_json::from_str(&st.params_json()?)?;
        s.push_str(&serde_json::to_string(
            &json!({"command": st.cmd, "params": params}),
        )?);
        s.push('\n');
    }
    s.push_str(&serde_json::to_string(
        &json!({"command": "app.export", "params": export.params(out)?}),
    )?);
    s.push('\n');
    Ok(s)
}

/// `run` prints one `{"command", "ok", "result" | "error"}` line per command, in order, and
/// stops at the first failure: its index and message.
fn first_failure(stdout: &str) -> Option<(usize, String)> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v.get("command").is_some())
        .enumerate()
        .find(|(_, v)| v.get("ok").and_then(Value::as_bool) == Some(false))
        .map(|(i, v)| {
            let cmd = v.get("command").and_then(Value::as_str).unwrap_or("?");
            let err = v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            (i, format!("{cmd}: {err}"))
        })
}

/// Runs control-protocol methods or commands headless on LightCraft's demo library (no files,
/// no user library) and returns each command's `result`; used for status queries.
pub fn query(cli: &Path, commands: &[(&str, Value)], cancel: &AtomicBool) -> Result<Vec<Value>> {
    let mut cmd = Command::new(cli);
    cmd.args(["run", "--demo", "--keep-going"]);
    for (c, p) in commands {
        cmd.arg(c).arg(serde_json::to_string(p)?);
    }
    let o = procrun::run(cmd, None, cancel)?;
    let lines: Vec<Value> = o
        .stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v.get("command").is_some())
        .collect();
    if lines.len() != commands.len() {
        bail!(
            "LightCraft answered {} of {} queries: {}",
            lines.len(),
            commands.len(),
            o.stderr.trim()
        );
    }
    Ok(lines
        .into_iter()
        .map(|v| {
            if v.get("ok").and_then(Value::as_bool) == Some(true) {
                v.get("result").cloned().unwrap_or(Value::Null)
            } else {
                json!({"error": v.get("error").cloned().unwrap_or(Value::Null)})
            }
        })
        .collect())
}

/// `lightcraft-cli --version`, first line.
pub fn version(cli: &Path) -> Result<String> {
    super::version(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_reply_gives_result() -> Result<()> {
        assert_eq!(
            parse_reply("{\"id\":3,\"ok\":true,\"result\":{\"a\":1}}\n", 3)?,
            json!({"a": 1})
        );
        Ok(())
    }

    #[test]
    fn error_reply_is_an_error() {
        let e = parse_reply("{\"id\":1,\"ok\":false,\"error\":\"unknown command\"}\n", 1);
        assert!(
            format!("{:#}", e.err().map(|e| e.to_string()).unwrap_or_default())
                .contains("unknown command")
        );
    }

    #[test]
    fn hostile_replies_are_errors_not_panics() {
        for bad in [
            "",
            "{\"ok\":true}",
            "not json\n",
            "{\"id\":9,\"ok\":true}\n",
            "{\"id\":1}\n",
            "[1,2]\n",
        ] {
            assert!(parse_reply(bad, 1).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn non_loopback_address_is_refused() {
        assert!(Control::new("8.8.8.8:7980").is_err());
        assert!(Control::new("127.0.0.1:7980").is_ok());
    }

    #[test]
    fn headless_args_import_run_then_export() -> Result<()> {
        let st: crate::recipe::LightCraftStage =
            toml::from_str("steps = [{ cmd = \"mask.add\", params = { kind = \"subject\" } }]\n")?;
        let steps: Vec<&Step> = st.steps.iter().collect();
        let a = run_args(
            Path::new("in.tif"),
            Path::new("out.jpg"),
            &steps,
            &st.export,
        )?;
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(
            a,
            [
                "run",
                "--import",
                "in.tif",
                "mask.add",
                r#"{"kind":"subject"}"#,
                "app.export",
                r#"{"longEdge":0,"path":"out.jpg"}"#
            ]
        );
        assert!(run_args(Path::new("a.jpg"), Path::new("a.jpg"), &steps, &st.export).is_err());
        Ok(())
    }

    #[test]
    fn failed_command_line_is_reported_with_its_index() {
        let out = "{\"command\":\"develop.set\",\"ok\":true}\nnoise\n{\"command\":\"nope\",\"error\":\"unknown command\",\"ok\":false}\n";
        let (i, msg) = first_failure(out).unwrap_or_default();
        assert_eq!(i, 1);
        assert!(msg.contains("nope: unknown command"), "{msg}");
        assert!(first_failure("{\"command\":\"a\",\"ok\":true}\n").is_none());
    }

    #[test]
    fn export_files_are_read() -> Result<()> {
        let r = json!({"files": [{"path": "a.tif", "width": 1}, {"nope": 1}]});
        assert_eq!(exported_files(&r)?, vec![PathBuf::from("a.tif")]);
        assert!(exported_files(&json!({"background": true})).is_err());
        Ok(())
    }
}
