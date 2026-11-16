//! Runs a child process to the end, or kills it as soon as `cancel` is set.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

pub struct Output {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Raised when `cancel` stopped the process; callers report it as a cancel, not a failure.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub fn is_cancelled(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Cancelled>())
}

/// Runs `cmd` with piped output (and `stdin` text when given), polling `cancel`.
pub fn run(mut cmd: Command, stdin: Option<String>, cancel: &AtomicBool) -> Result<Output> {
    let name = format!("{:?}", cmd.get_program());
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    hide_console(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("starting {name}"))?;
    if let Some(text) = stdin
        && let Some(mut w) = child.stdin.take()
    {
        thread::spawn(move || {
            use std::io::Write;
            let _ = w.write_all(text.as_bytes());
        });
    }
    let out = child.stdout.take().map(reader);
    let err = child.stderr.take().map(reader);
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Cancelled.into());
        }
        match child.try_wait()? {
            Some(s) => break s,
            None => thread::sleep(Duration::from_millis(50)),
        }
    };
    let collect =
        |h: Option<thread::JoinHandle<String>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(Output {
        status,
        stdout: collect(out),
        stderr: collect(err),
    })
}

fn reader<R: Read + Send + 'static>(mut r: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Keeps the Craft CLIs from flashing a console window when the GUI starts them.
#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}

/// Fails with the tool's stderr when it exited non-zero.
pub fn check(o: &Output, what: &str) -> Result<()> {
    if !o.status.success() {
        bail!("{what} ({}): {}", o.status, o.stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_before_start_is_reported_as_cancelled() {
        let cancel = AtomicBool::new(true);
        let cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 5 127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("5");
            c
        };
        let r = run(cmd, None, &cancel);
        assert!(r.as_ref().err().is_some_and(is_cancelled));
    }
}
