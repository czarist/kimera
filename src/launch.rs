//! Opens the Craft apps' windows when Kimera starts, so they are ready beside the conveyor.
//!
//! The conveyor itself runs the apps' CLIs headless; the windows are for looking at results,
//! and LightCraft's control channel (`lightcraft --control <port>`) is what `pipeline`,
//! `--reimport` and model installs talk to. An app that is already open is left alone.

use crate::apps::App;
use crate::apps::lightcraft::Control;
use crate::config::{Config, WindowStyle};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for a freshly started LightCraft to open its control port. A cold start
/// loads the library and the GPU backend first.
const CONTROL_WAIT: Duration = Duration::from_secs(40);
/// Pause between control-port probes while waiting.
const CONTROL_POLL: Duration = Duration::from_millis(500);
/// How long to wait for a started app's window, to hide or minimize it.
#[cfg(windows)]
const WINDOW_WAIT: Duration = Duration::from_secs(30);
/// Pause between looks for that window.
#[cfg(windows)]
const WINDOW_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Already open (for LightCraft: answering on its control port).
    Running,
    Started,
    /// LightCraft is open without the control channel; Kimera does not start a second one.
    RunningWithoutControl,
}

impl Outcome {
    pub fn describe(&self, app: App) -> String {
        match self {
            Self::Running => format!("{} is open", app.label()),
            Self::Started => format!("{} started", app.label()),
            Self::RunningWithoutControl => "LightCraft is open without its control channel; \
                close it and press Open apps (or run `lightcraft --control <port>`) for \
                pipeline and model installs"
                .to_owned(),
        }
    }
}

/// The desktop program beside a CLI: `lightcraft-cli.exe` → `lightcraft.exe`.
pub fn gui_exe(cli: &Path) -> PathBuf {
    let stem = cli.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let name = stem.strip_suffix("-cli").unwrap_or(stem);
    let mut exe = cli.with_file_name(name);
    if let Some(ext) = cli.extension() {
        exe.set_extension(ext);
    }
    exe
}

/// Opens every configured app that is not open yet. One result per app, in `App::ALL` order.
pub fn open_apps(cfg: &Config) -> Vec<(App, Result<Outcome>)> {
    let mut out = vec![
        (App::LightCraft, open_lightcraft(cfg)),
        (
            App::PhotoCraft,
            open_plain(&cfg.photocraft.cli, cfg.startup.window),
        ),
    ];
    if let Some(vc) = &cfg.vectorcraft {
        out.push((App::VectorCraft, open_plain(&vc.cli, cfg.startup.window)));
    }
    out
}

/// One line per app, for logs and the terminal.
pub fn report(results: &[(App, Result<Outcome>)]) -> Vec<String> {
    results
        .iter()
        .map(|(app, r)| match r {
            Ok(o) => o.describe(*app),
            Err(e) => format!("{} could not be opened: {e:#}", app.label()),
        })
        .collect()
}

fn open_lightcraft(cfg: &Config) -> Result<Outcome> {
    let control = Control::new(&cfg.lightcraft.control)?;
    if control.answers() {
        return Ok(Outcome::Running);
    }
    let exe = gui_exe(&cfg.lightcraft.cli);
    if is_running(&exe) {
        return Ok(Outcome::RunningWithoutControl);
    }
    spawn(
        &exe,
        &["--control".to_owned(), control.port().to_string()],
        cfg.startup.window,
    )?;
    let start = Instant::now();
    while start.elapsed() < CONTROL_WAIT {
        if control.answers() {
            return Ok(Outcome::Started);
        }
        std::thread::sleep(CONTROL_POLL);
    }
    bail!(
        "started {}, but its control port {} did not open within {} s",
        exe.display(),
        control.port(),
        CONTROL_WAIT.as_secs()
    )
}

fn open_plain(cli: &Path, window: WindowStyle) -> Result<Outcome> {
    let exe = gui_exe(cli);
    if is_running(&exe) {
        return Ok(Outcome::Running);
    }
    spawn(&exe, &[], window)?;
    Ok(Outcome::Started)
}

/// Starts a desktop program and lets it live on after Kimera exits.
fn spawn(exe: &Path, args: &[String], window: WindowStyle) -> Result<()> {
    if !exe.is_file() {
        bail!("{} not found (it should sit beside the CLI)", exe.display());
    }
    let dir = exe.parent().unwrap_or(Path::new("."));
    let mut cmd = start_command(exe, args, dir, window);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .current_dir(dir);
    let out = cmd
        .output()
        .with_context(|| format!("starting {}", exe.display()))?;
    if !out.status.success() {
        bail!(
            "starting {}: {}",
            exe.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// On Windows `launch.ps1` starts the app through WMI, so it is not Kimera's child: a child
/// would inherit Kimera's output handles and hold a terminal pipe open until the app closed.
/// The script also hides or minimizes the app's windows.
#[cfg(windows)]
fn start_command(exe: &Path, args: &[String], dir: &Path, window: WindowStyle) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // ShowWindow codes: SW_HIDE, SW_SHOWMINNOACTIVE; -1 leaves the windows alone.
    let show = match window {
        WindowStyle::Hidden => 0,
        WindowStyle::Minimized => 7,
        WindowStyle::Normal => -1,
    };
    let line = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "$line = {}; $dir = {}; $show = {show}; $tries = {}; $poll = {}
{}",
        ps_quote(&line),
        ps_quote(&dir.to_string_lossy()),
        WINDOW_WAIT.as_millis() / WINDOW_POLL.as_millis(),
        WINDOW_POLL.as_millis(),
        include_str!("launch.ps1"),
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Elsewhere a background shell job starts the app with its output going nowhere.
#[cfg(not(windows))]
fn start_command(exe: &Path, args: &[String], _dir: &Path, _window: WindowStyle) -> Command {
    let mut sh = Command::new("sh");
    sh.arg("-c")
        .arg(r#""$0" "$@" </dev/null >/dev/null 2>&1 &"#)
        .arg(exe)
        .args(args);
    sh
}

/// A PowerShell single-quoted literal: nothing inside is expanded.
#[cfg(any(windows, test))]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Whether a process with this executable's file name runs (for this user, on this computer).
fn is_running(exe: &Path) -> bool {
    let Some(name) = exe.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    process_names().iter().any(|p| p.eq_ignore_ascii_case(name))
}

#[cfg(windows)]
fn process_names() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(o) = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.split(',').next())
        .map(|n| n.trim_matches('"').to_owned())
        .collect()
}

#[cfg(not(windows))]
fn process_names() -> Vec<String> {
    let Ok(o) = Command::new("ps").args(["-A", "-o", "comm="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| Path::new(l.trim()).file_name()?.to_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_literals() {
        assert_eq!(
            ps_quote(r"C:\Program Files\A'b\x.exe"),
            r"'C:\Program Files\A''b\x.exe'"
        );
        assert_eq!(ps_quote("$env:X; calc"), "'$env:X; calc'");
    }

    #[test]
    fn gui_beside_cli() {
        assert_eq!(
            gui_exe(Path::new(r"F:\LightCraft\lightcraft-cli.exe")),
            PathBuf::from(r"F:\LightCraft\lightcraft.exe")
        );
        assert_eq!(
            gui_exe(Path::new("/opt/photocraft/photocraft-cli")),
            PathBuf::from("/opt/photocraft/photocraft")
        );
    }
}
