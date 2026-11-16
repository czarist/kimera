//! LightCraft two ways: headless on one file (`lightcraft-cli run --import <file> …`), and the
//! control channel of a running app, JSON lines on a loopback TCP port
//! (`lightcraft --control 7980`; see LightCraft's docs/control-protocol.md).

use crate::recipe::LightCraftStage;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// LightCraft caps a reply at 64 MiB before the newline.
const MAX_REPLY: u64 = 64 * 1024 * 1024 + 1;

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
        && got != id {
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
pub fn run_args(input: &Path, out: &Path, stage: &LightCraftStage) -> Result<Vec<OsString>> {
    if input == out {
        bail!("refusing to overwrite the input {}", input.display());
    }
    let mut a: Vec<OsString> = vec!["run".into(), "--import".into(), input.into()];
    for s in &stage.steps {
        a.push((&s.cmd).into());
        a.push(s.params_json()?.into());
    }
    a.push("app.export".into());
    a.push(serde_json::to_string(&stage.export.params(out)?)?.into());
    Ok(a)
}

/// Runs the stage on `input` and writes `out`.
pub fn run_file(cli: &Path, input: &Path, out: &Path, stage: &LightCraftStage) -> Result<()> {
    let args = run_args(input, out, stage)?;
    let o = Command::new(cli)
        .args(&args)
        .output()
        .with_context(|| format!("starting {}", cli.display()))?;
    first_failure(&String::from_utf8_lossy(&o.stdout))
        .with_context(|| format!("LightCraft failed on {}", input.display()))?;
    if !o.status.success() {
        bail!(
            "LightCraft failed on {} ({}): {}",
            input.display(),
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    if !out.is_file() {
        bail!("LightCraft reported success but wrote no {}", out.display());
    }
    Ok(())
}

/// `run` prints one `{"command", "ok", "result" | "error"}` line per command.
fn first_failure(stdout: &str) -> Result<()> {
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if v.get("ok").and_then(Value::as_bool) == Some(false) {
            let cmd = v.get("command").and_then(Value::as_str).unwrap_or("?");
            let err = v.get("error").and_then(Value::as_str).unwrap_or("unknown error");
            bail!("{cmd}: {err}");
        }
    }
    Ok(())
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
        let st: LightCraftStage = toml::from_str(
            "steps = [{ cmd = \"mask.add\", params = { kind = \"subject\" } }]
",
        )?;
        let a = run_args(Path::new("in.tif"), Path::new("out.jpg"), &st)?;
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
        assert!(run_args(Path::new("a.jpg"), Path::new("a.jpg"), &st).is_err());
        Ok(())
    }

    #[test]
    fn failed_command_line_is_reported() {
        let out = "{\"command\":\"develop.set\",\"ok\":true}
noise
{\"command\":\"nope\",\"error\":\"unknown command\",\"ok\":false}
";
        let e = first_failure(out).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("nope: unknown command"), "{e}");
        assert!(first_failure("{\"ok\":true}
").is_ok());
    }

    #[test]
    fn export_files_are_read() -> Result<()> {
        let r = json!({"files": [{"path": "a.tif", "width": 1}, {"nope": 1}]});
        assert_eq!(exported_files(&r)?, vec![PathBuf::from("a.tif")]);
        assert!(exported_files(&json!({"background": true})).is_err());
        Ok(())
    }
}
