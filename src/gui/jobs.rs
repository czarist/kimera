//! Work the window hands to background threads; results come back as [`Msg`]s.

use crate::analysis::{self, Report};
use crate::config::Config;
use crate::engine::{self, Event, Extras};
use crate::notes::Note;
use crate::status::{self, Check, Models};
use crate::{recipe, writer};
use eframe::egui::{ColorImage, Context};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

pub enum Msg {
    Checks(Vec<Check>),
    Thumb(PathBuf, Option<ColorImage>),
    Draft(Result<(String, String), String>),
    RunDir(PathBuf),
    /// The stages the run really has (an app may have been turned on for notes).
    Plan(Vec<engine::Slot>),
    /// Photo index in the window's list.
    Analysed(usize, Result<Report, String>),
    /// Engine event; the slice maps the run's photo numbers to the window's list.
    Event(Arc<[usize]>, Event),
    Finished {
        test: bool,
        result: Result<String, String>,
    },
    Installed(Result<String, String>),
    /// The Craft apps' windows were opened (or found open); one line per app.
    Opened(Vec<String>),
    Proof(Result<Vec<PathBuf>, String>),
    Log(String),
}

/// Client proof PDFs of finished files, into `<run>/proof`.
pub fn proof(cfg: Config, files: Vec<PathBuf>, out: PathBuf, tx: Sender<Msg>, ctx: Context) {
    std::thread::spawn(move || {
        let r = crate::apps::photocraft::proof(
            &cfg.photocraft.cli,
            &files,
            &out,
            4,
            5,
            &AtomicBool::new(false),
        );
        send(&tx, &ctx, Msg::Proof(r.map_err(|e| format!("{e:#}"))));
    });
}

fn send(tx: &Sender<Msg>, ctx: &Context, m: Msg) {
    let _ = tx.send(m);
    ctx.request_repaint();
}

pub fn checks(cfg: Config, tx: Sender<Msg>, ctx: Context) {
    std::thread::spawn(move || {
        let c = status::all(&cfg, &AtomicBool::new(false));
        send(&tx, &ctx, Msg::Checks(c));
    });
}

/// Opens the Craft apps' windows; LightCraft may take a while to open its control port.
pub fn open_apps(cfg: Config, tx: Sender<Msg>, ctx: Context) {
    std::thread::spawn(move || {
        let lines = crate::launch::report(&crate::launch::open_apps(&cfg));
        send(&tx, &ctx, Msg::Opened(lines));
    });
}

/// Installs a model; the user accepted its licence by clicking Install.
pub fn install(cfg: Config, name: status::Model, tx: Sender<Msg>, ctx: Context) {
    std::thread::spawn(move || {
        let mut last = String::new();
        let r = status::install(&cfg, name, &mut |line| {
            if line != last {
                last = line.to_owned();
                send(&tx, &ctx, Msg::Log(line.to_owned()));
            }
        });
        send(
            &tx,
            &ctx,
            Msg::Installed(r.map(|()| name.to_string()).map_err(|e| format!("{e:#}"))),
        );
    });
}

pub fn write(
    cfg: Config,
    request: String,
    photos: Vec<PathBuf>,
    previous: Option<String>,
    cancel: Arc<AtomicBool>,
    tx: Sender<Msg>,
    ctx: Context,
) {
    std::thread::spawn(move || {
        let r = (|| {
            let cats = crate::catalogs(&cfg, &cancel)?;
            let context = Models::read(&cfg, &cancel)
                .map(|m| m.describe())
                .unwrap_or_default();
            let d = writer::write(
                &crate::ai_client(&cfg),
                &cats,
                &request,
                &photos,
                previous.as_deref(),
                &context,
                &cancel,
            )?;
            Ok::<_, anyhow::Error>((d.notes, d.toml))
        })();
        send(&tx, &ctx, Msg::Draft(r.map_err(|e| format!("{e:#}"))));
    });
}

pub struct RunJob {
    pub cfg: Config,
    pub recipe_text: String,
    pub files: Vec<PathBuf>,
    /// Window index of each file.
    pub index: Vec<usize>,
    pub analyse: bool,
    /// The user's notes, keyed by position in `files`.
    pub notes: HashMap<usize, Vec<Note>>,
    pub test: bool,
    pub cancel: Arc<AtomicBool>,
    pub tx: Sender<Msg>,
    pub ctx: Context,
}

pub fn run(job: RunJob) {
    std::thread::spawn(move || {
        let test = job.test;
        let r = run_inner(&job);
        send(
            &job.tx,
            &job.ctx,
            Msg::Finished {
                test,
                result: r.map_err(|e| format!("{e:#}")),
            },
        );
    });
}

fn run_inner(job: &RunJob) -> anyhow::Result<String> {
    let mut r = recipe::parse(&job.recipe_text)?;
    let cfg = &job.cfg;
    if let Some(m) = Models::read(cfg, &job.cancel) {
        let missing = m.missing_for(&r);
        if !missing.is_empty() {
            anyhow::bail!(
                "the recipe needs LightCraft models that are not installed: {} (install them in Setup, or mark those steps optional)",
                missing.join("; ")
            );
        }
    }
    let work = if job.test {
        cfg.work.dir.join("tests")
    } else {
        cfg.work.dir.clone()
    };
    let dir = crate::run_dir(&work)?;
    send(&job.tx, &job.ctx, Msg::RunDir(dir.clone()));
    let mut extras = Extras::new();
    if job.analyse {
        let client = crate::ai_client(cfg);
        let scratch = cfg.work.dir.join(".kimera").join("look");
        let mut reports = Vec::new();
        for (i, f) in job.files.iter().enumerate() {
            if job.cancel.load(Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            let rep = analysis::analyse(&client, &cfg.lightcraft.cli, f, &scratch, &job.cancel);
            if let Ok(rep) = &rep {
                let steps = rep.extra_steps();
                if !steps.is_empty() {
                    extras.entry(i).or_default().lightcraft.extend(steps);
                }
            }
            reports.push(rep.as_ref().ok().cloned());
            let at = job.index.get(i).copied().unwrap_or(i);
            send(
                &job.tx,
                &job.ctx,
                Msg::Analysed(at, rep.map_err(|e| format!("{e:#}"))),
            );
        }
        for (i, donors) in analysis::review(&job.files, &reports) {
            let name = |k: usize| {
                job.files
                    .get(k)
                    .and_then(|f| f.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            let line = if donors.is_empty() {
                format!("{}: closed eyes, no nearby frame with open eyes", name(i))
            } else {
                let d: Vec<String> = donors.iter().map(|&k| name(k)).collect();
                format!(
                    "{}: closed eyes; a swap could come from {}",
                    name(i),
                    d.join(", ")
                )
            };
            send(&job.tx, &job.ctx, Msg::Log(line));
        }
    }
    if !job.notes.is_empty() {
        let cats = crate::catalogs(cfg, &job.cancel)?;
        let client = crate::ai_client(cfg);
        let scratch = cfg.work.dir.join(".kimera").join("notes-look");
        crate::notes::NotesJob {
            client: &client,
            catalogs: &cats,
            lightcraft: &cfg.lightcraft.cli,
            scratch: &scratch,
            cancel: &job.cancel,
        }
        .plan_all(&job.files, &job.notes, &mut extras, &mut |l| {
            send(&job.tx, &job.ctx, Msg::Log(l));
        })?;
    }
    let added = engine::ensure_apps(&mut r, &extras, &job.files);
    if !added.is_empty() {
        send(
            &job.tx,
            &job.ctx,
            Msg::Log(format!(
                "Turned {} on for the per-photo steps",
                added
                    .iter()
                    .map(|a| a.label())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )),
        );
        send(&job.tx, &job.ctx, Msg::Plan(engine::plan(&r)));
    }
    let index: Arc<[usize]> = job.index.clone().into();
    let sum = engine::run(
        &crate::apps(cfg),
        &r,
        &job.files,
        &dir,
        &extras,
        &job.cancel,
        &mut |e| send(&job.tx, &job.ctx, Msg::Event(index.clone(), e)),
    )?;
    Ok(format!(
        "Finished: {} file(s), {} photo(s) failed{}{} → {}",
        sum.done.len(),
        sum.failed,
        if sum.skipped_groups > 0 {
            format!(", {} optional group(s) skipped", sum.skipped_groups)
        } else {
            String::new()
        },
        if sum.cancelled { ", cancelled" } else { "" },
        dir.display()
    ))
}
