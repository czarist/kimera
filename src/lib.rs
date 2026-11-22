//! Kimera: a photographer's conveyor over the Craft apps' command-line tools. The CLI
//! (`kimera`) and the window (`kimera-gui`) share everything here.

pub mod ai;
pub mod analysis;
pub mod apps;
pub mod catalog;
pub mod config;
pub mod engine;
pub mod gui;
pub mod launch;
pub mod look;
pub mod notes;
pub mod portrait;
pub mod procrun;
pub mod recipe;
pub mod status;
pub mod writer;

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `<work>/run-<unix seconds>`: every run keeps its own files, nothing is overwritten.
pub fn run_dir(work: &Path) -> Result<PathBuf> {
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

/// The command catalogs of the configured apps (cached in the work folder).
pub fn catalogs(
    cfg: &config::Config,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<catalog::Catalog>> {
    let cache = cfg.work.dir.join(".kimera");
    let mut out = vec![
        catalog::load(apps::App::LightCraft, &cfg.lightcraft.cli, &cache, cancel)?,
        catalog::load(apps::App::PhotoCraft, &cfg.photocraft.cli, &cache, cancel)?,
    ];
    if let Some(vc) = &cfg.vectorcraft {
        out.push(catalog::load(
            apps::App::VectorCraft,
            &vc.cli,
            &cache,
            cancel,
        )?);
    }
    Ok(out)
}

pub fn apps(cfg: &config::Config) -> engine::Apps {
    engine::Apps {
        photocraft: cfg.photocraft.cli.clone(),
        lightcraft: cfg.lightcraft.cli.clone(),
        vectorcraft: cfg.vectorcraft.as_ref().map(|v| v.cli.clone()),
    }
}

pub fn ai_client(cfg: &config::Config) -> ai::Client {
    ai::Client::new(cfg.ai.clone(), cfg.work.dir.join(".kimera"))
}
