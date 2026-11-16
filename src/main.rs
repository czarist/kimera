//! Kimera: a CLI tool for Craft apps.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod apps;
mod config;
mod engine;
mod recipe;

use anyhow::{Context, Result, bail};
use apps::{lightcraft::Control, photocraft};
use engine::Event;
use std::sync::atomic::AtomicBool;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(
    name = "kimera",
    version,
    about = "Kimera: a CLI tool for Craft apps (LightCraft, PhotoCraft, VectorCraft).\nNot affiliated with or endorsed by the ArtCraft team."
)]
struct Cli {
    /// kimera.toml (default: ./kimera.toml, then next to kimera.exe)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Find the Craft apps, print their versions and check LightCraft's control channel
    Doctor,
    /// Run a recipe's stages on photos: folders and/or files; originals are never changed
    Run {
        /// Recipe file (see recipes/)
        #[arg(long)]
        recipe: PathBuf,
        /// Photo files or folders (a folder gives its top-level images)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Export photos from the running LightCraft, then run the recipe's stages on each file
    Pipeline {
        /// Recipe file (see recipes/)
        #[arg(long)]
        recipe: PathBuf,
        /// LightCraft photo ids (default: the photos selected in the app)
        #[arg(long, value_delimiter = ',')]
        ids: Vec<u64>,
    },
}

fn main() -> std::process::ExitCode {
    match run(Cli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kimera: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let cfg_path = config::locate(cli.config.as_deref())?;
    let cfg = config::load(&cfg_path)?;
    match cli.cmd {
        Cmd::Doctor => doctor(&cfg),
        Cmd::Run { recipe, paths } => {
            let r = recipe::load(&recipe)?;
            let files = engine::collect(&paths)?;
            if files.is_empty() {
                bail!("no images found in the given paths");
            }
            stages(&cfg, &r, &files, &run_dir(&cfg.work.dir)?)
        }
        Cmd::Pipeline { recipe, ids } => pipeline(&cfg, &recipe::load(&recipe)?, ids),
    }
}

fn doctor(cfg: &config::Config) -> Result<()> {
    let mut ok = true;
    let mut line = |name: &str, r: Result<String>| match r {
        Ok(v) => println!("  ok   {name:<12} {v}"),
        Err(e) => {
            ok = false;
            println!("  FAIL {name:<12} {e:#}");
        }
    };
    line("LightCraft", apps::lightcraft::version(&cfg.lightcraft.cli));
    line("PhotoCraft", photocraft::version(&cfg.photocraft.cli));
    if let Some(vc) = &cfg.vectorcraft {
        line("VectorCraft", apps::version(&vc.cli));
    }
    line("work dir", Ok(cfg.work.dir.display().to_string()));
    // Only `pipeline` needs a running LightCraft; `run` works headless.
    match Control::new(&cfg.lightcraft.control).and_then(|mut c| c.selection()) {
        Ok(s) => println!(
            "  ok   {:<12} {} answers, {} photo(s) selected",
            "LC control",
            cfg.lightcraft.control,
            s.len()
        ),
        Err(_) => println!(
            "  --   {:<12} not running (only `kimera pipeline` needs `lightcraft --control`)",
            "LC control"
        ),
    }
    if ok {
        Ok(())
    } else {
        bail!("some checks failed (see above)")
    }
}

fn pipeline(cfg: &config::Config, r: &recipe::Recipe, ids: Vec<u64>) -> Result<()> {
    let mut lc = Control::new(&cfg.lightcraft.control)?;
    let ids = if ids.is_empty() { lc.selection()? } else { ids };
    if ids.is_empty() {
        bail!("no photos: select some in LightCraft or pass --ids");
    }
    let dir = run_dir(&cfg.work.dir)?;
    let export_dir = dir.join("0-export");
    std::fs::create_dir_all(&export_dir)
        .with_context(|| format!("creating {}", export_dir.display()))?;
    println!(
        "LightCraft: exporting {} photo(s) to {}",
        ids.len(),
        export_dir.display()
    );
    let files = lc.export(r.export.params(&ids, &export_dir))?;
    if files.is_empty() {
        bail!("LightCraft exported nothing");
    }
    stages(cfg, r, &files, &dir)
}

/// Runs the recipe's stages on `files` into `dir`, printing progress.
fn stages(cfg: &config::Config, r: &recipe::Recipe, files: &[PathBuf], dir: &Path) -> Result<()> {
    let apps = engine::Apps {
        photocraft: cfg.photocraft.cli.clone(),
        lightcraft: cfg.lightcraft.cli.clone(),
    };
    let total = files.len();
    let flow: Vec<&str> = r.stages.iter().map(|s| s.app()).collect();
    println!("{total} photo(s): {} -> {}", flow.join(" -> "), dir.display());
    let name = |i: usize| {
        files
            .get(i)
            .and_then(|f| f.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let app = |i: usize| flow.get(i).copied().unwrap_or("?");
    let cancel = AtomicBool::new(false);
    let sum = engine::run(&apps, r, files, dir, &cancel, &mut |e| match e {
        Event::StageStarted { photo, stage } => {
            println!("[{}/{total}] {} : {}", photo.saturating_add(1), name(photo), app(stage));
        }
        Event::StageDone {
            photo,
            stage,
            out,
            ms,
        } => println!(
            "        ok   {} {} ({:.1}s) -> {}",
            app(stage),
            name(photo),
            ms as f64 / 1000.0,
            out.display()
        ),
        Event::PhotoFailed {
            photo,
            stage,
            error,
        } => eprintln!("        FAIL {} {}: {error}", app(stage), name(photo)),
        Event::PhotoDone { photo, out } => {
            println!("        done {} => {}", name(photo), out.display());
        }
    })?;
    println!(
        "done: {} ok, {} failed{} -> {}",
        sum.done.len(),
        sum.failed,
        if sum.cancelled { ", cancelled" } else { "" },
        dir.display()
    );
    if sum.failed > 0 {
        bail!("{} of {total} photo(s) failed", sum.failed);
    }
    Ok(())
}

/// `<work>/run-<unix seconds>`: every run keeps its own files, nothing is overwritten.
fn run_dir(work: &Path) -> Result<PathBuf> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut dir = work.join(format!("run-{secs}"));
    let mut n = 1u32;
    while dir.exists() {
        n = n.saturating_add(1);
        dir = work.join(format!("run-{secs}-{n}"));
    }
    Ok(dir)
}
