//! Work the window hands to background threads; results come back as [`Msg`]s.

use crate::analysis::{self, Report};
use crate::config::Config;
use crate::engine::{self, Event, Extras, PhotoExtras};
use crate::look::{self, Passes};
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
    /// The stages the run really has (an app may have been turned on for notes or the finish).
    Plan(Vec<engine::Slot>),
    /// What the run is doing now: for one photo (its index in the window's list) or for all.
    Phase(Option<usize>, String),
    /// Photo index in the window's list.
    Analysed(usize, Result<Report, String>),
    /// The AI's sentence about how it finished a photo.
    Finished1(usize, String),
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
    /// Large pictures for the window, decoded off the UI thread.
    View(View),
    Log(String),
}

/// What a [`view`] request was for, and what came of it.
pub enum View {
    /// Before / after of the photo at this index.
    Compare {
        photo: usize,
        before: Option<ColorImage>,
        after: Option<ColorImage>,
        after_path: Option<PathBuf>,
    },
    /// The photo the note editor shows.
    Note(PathBuf, Option<ColorImage>),
}

/// What [`view`] should load.
pub enum ViewRequest {
    Compare {
        photo: usize,
        before: PathBuf,
        after: Option<PathBuf>,
        size: u32,
    },
    Note(PathBuf, u32),
}

/// Decodes large pictures in the background: a 24-megapixel JPEG (or a raw, rendered by
/// LightCraft) takes seconds, and the window must keep answering meanwhile.
pub fn view(req: ViewRequest, lightcraft: PathBuf, tx: Sender<Msg>, ctx: Context) {
    std::thread::spawn(move || {
        let load = |p: &PathBuf, size| super::thumbs::load_rgba(p, &lightcraft, size);
        let v = match req {
            ViewRequest::Compare {
                photo,
                before,
                after,
                size,
            } => View::Compare {
                photo,
                before: load(&before, size),
                after: after.as_ref().and_then(|a| load(a, size)),
                after_path: after,
            },
            ViewRequest::Note(path, size) => {
                let img = load(&path, size);
                View::Note(path, img)
            }
        };
        send(&tx, &ctx, Msg::View(v));
    });
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

/// A recipe from the AI.
pub struct WriteJob {
    pub cfg: Config,
    pub request: String,
    pub photos: Vec<PathBuf>,
    /// The recipe to change, or `None` to start over.
    pub previous: Option<String>,
    pub references: Vec<PathBuf>,
    pub finishing: bool,
    pub cancel: Arc<AtomicBool>,
    pub tx: Sender<Msg>,
    pub ctx: Context,
}

pub fn write(job: WriteJob) {
    std::thread::spawn(move || {
        let cfg = &job.cfg;
        let r = (|| {
            let cats = crate::catalogs(cfg, &job.cancel)?;
            let context = Models::read(cfg, &job.cancel)
                .map(|m| m.describe())
                .unwrap_or_default();
            let refs = if job.references.is_empty() {
                Vec::new()
            } else {
                send(
                    &job.tx,
                    &job.ctx,
                    Msg::Phase(None, "Reading your reference photos".into()),
                );
                look::references(
                    &cfg.lightcraft.cli,
                    &job.references,
                    &finish_scratch(cfg),
                    &job.cancel,
                    &mut |l| send(&job.tx, &job.ctx, Msg::Log(l)),
                )?
            };
            let previews: Vec<PathBuf> = refs
                .iter()
                .take(look::MAX_REFERENCE_IMAGES)
                .map(|r| r.preview.clone())
                .collect();
            send(
                &job.tx,
                &job.ctx,
                Msg::Phase(
                    None,
                    format!("{} is writing the recipe", cfg.ai.provider.label()),
                ),
            );
            let d = writer::write(
                &crate::ai_client(cfg),
                &cats,
                &writer::Request {
                    text: &job.request,
                    photos: &job.photos,
                    previous: job.previous.as_deref(),
                    context: &context,
                    references: &previews,
                    finishing: job.finishing,
                },
                &job.cancel,
            )?;
            Ok::<_, anyhow::Error>((d.notes, d.toml))
        })();
        send(
            &job.tx,
            &job.ctx,
            Msg::Draft(r.map_err(|e| format!("{e:#}"))),
        );
    });
}

fn finish_scratch(cfg: &Config) -> PathBuf {
    cfg.work.dir.join(".kimera").join("finish")
}

pub struct RunJob {
    pub cfg: Config,
    pub recipe_text: String,
    pub files: Vec<PathBuf>,
    /// Window index of each file.
    pub index: Vec<usize>,
    pub analyse: bool,
    pub finish: Passes,
    /// The user's finished photos whose look the finish matches.
    pub references: Vec<PathBuf>,
    /// The user's request, for the finish.
    pub brief: String,
    /// The user's notes, keyed by position in `files`.
    pub notes: HashMap<usize, Vec<Note>>,
    pub test: bool,
    pub cancel: Arc<AtomicBool>,
    pub tx: Sender<Msg>,
    pub ctx: Context,
}

impl RunJob {
    fn send(&self, m: Msg) {
        send(&self.tx, &self.ctx, m);
    }

    fn at(&self, i: usize) -> usize {
        self.index.get(i).copied().unwrap_or(i)
    }

    fn name(&self, i: usize) -> String {
        self.files
            .get(i)
            .and_then(|f| f.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

pub fn run(job: RunJob) {
    std::thread::spawn(move || {
        let test = job.test;
        let r = run_inner(&job);
        job.send(Msg::Finished {
            test,
            result: r.map_err(|e| format!("{e:#}")),
        });
    });
}

fn run_inner(job: &RunJob) -> anyhow::Result<String> {
    let mut r = recipe::parse(&job.recipe_text)?;
    let cfg = &job.cfg;
    let models = Models::read(cfg, &job.cancel);
    if let Some(m) = &models {
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
    job.send(Msg::RunDir(dir.clone()));
    let kept = dir.join("recipe.toml");
    if let Err(e) = std::fs::write(&kept, &job.recipe_text) {
        job.send(Msg::Log(format!(
            "Could not keep the recipe in {}: {e}",
            kept.display()
        )));
    }
    let client = crate::ai_client(cfg);
    let finishing = job.finish != Passes::Off;
    let refs = if finishing && !job.references.is_empty() {
        job.send(Msg::Phase(None, "Reading your reference photos".into()));
        look::references(
            &cfg.lightcraft.cli,
            &job.references,
            &finish_scratch(cfg),
            &job.cancel,
            &mut |l| job.send(Msg::Log(l)),
        )?
    } else {
        Vec::new()
    };
    let mut extras = Extras::new();
    if !job.notes.is_empty() {
        job.send(Msg::Phase(None, "Turning your notes into steps".into()));
        let cats = crate::catalogs(cfg, &job.cancel)?;
        let scratch = cfg.work.dir.join(".kimera").join("notes-look");
        crate::notes::NotesJob {
            client: &client,
            catalogs: &cats,
            lightcraft: &cfg.lightcraft.cli,
            scratch: &scratch,
            cancel: &job.cancel,
        }
        .plan_all(&job.files, &job.notes, &mut extras, &mut |l| {
            job.send(Msg::Log(l));
        })?;
    }
    // The retouch runs in a PhotoCraft stage put first, on the photo at its own size; a recipe
    // with PhotoCraft stages of its own may have cropped or resized before them.
    let retouch = finishing && look::retouch_fits(&r) && cfg.photocraft.cli.is_file();
    let mut added = engine::ensure_apps(&mut r, &extras, &job.files);
    added.extend(engine::ensure_stages(
        &mut r,
        job.analyse || finishing,
        retouch,
        &job.files,
    ));
    if !added.is_empty() {
        job.send(Msg::Log(format!(
            "Turned {} on for the per-photo steps",
            added
                .iter()
                .map(|a| a.label())
                .collect::<Vec<_>>()
                .join(" and ")
        )));
        job.send(Msg::Plan(engine::plan(&r)));
    }
    let base = look::look_steps(&r);
    let finisher = look::FinishJob {
        client: &client,
        lightcraft: &cfg.lightcraft.cli,
        scratch: &finish_scratch(cfg),
        brief: &job.brief,
        base: &base,
        references: &refs,
        passes: job.finish,
        scans: &crate::portrait::scratch(&cfg.work.dir),
        sam: models.as_ref().is_some_and(|m| m.sam3),
        retouch,
        cancel: &job.cancel,
    };
    let look_scratch = cfg.work.dir.join(".kimera").join("look");
    let mut anchor: Option<look::Reference> = None;
    let mut reports: Vec<Option<Report>> = vec![None; job.files.len()];
    let index: Arc<[usize]> = job.index.clone().into();
    let sum = engine::run_prepared(
        &crate::apps(cfg),
        &r,
        &job.files,
        &dir,
        &mut |i| {
            let mut x: PhotoExtras = extras.remove(&i).unwrap_or_default();
            let Some(photo) = job.files.get(i) else {
                return x;
            };
            let at = job.at(i);
            if job.analyse && !job.cancel.load(Ordering::Relaxed) {
                job.send(Msg::Phase(Some(at), "AI checks faces and eyes".into()));
                let rep = analysis::analyse(
                    &client,
                    &cfg.lightcraft.cli,
                    photo,
                    &look_scratch,
                    &job.cancel,
                );
                if let Ok(rep) = &rep {
                    x.lightcraft.extend(rep.extra_steps());
                    if let Some(slot) = reports.get_mut(i) {
                        *slot = Some(rep.clone());
                    }
                }
                job.send(Msg::Analysed(at, rep.map_err(|e| format!("{e:#}"))));
            }
            if finishing && !job.cancel.load(Ordering::Relaxed) {
                let r = finisher.finish(photo, anchor.as_ref(), &mut |l| {
                    job.send(Msg::Phase(Some(at), l));
                });
                match r {
                    Ok(f) => {
                        if let Err(e) = look::record(&dir, &job.name(i), &f.reply) {
                            job.send(Msg::Log(format!("{e:#}")));
                        }
                        x.lightcraft_finish = f.steps;
                        x.photocraft.extend(f.photocraft);
                        job.send(Msg::Finished1(at, f.summary));
                        if anchor.is_none() {
                            anchor = f.anchor;
                        }
                    }
                    Err(e) => job.send(Msg::Log(format!(
                        "{}: not finished by the AI ({e:#})",
                        job.name(i)
                    ))),
                }
            }
            x
        },
        &job.cancel,
        &mut |e| job.send(Msg::Event(index.clone(), e)),
    )?;
    for (i, donors) in analysis::review(&job.files, &reports) {
        let line = if donors.is_empty() {
            format!(
                "{}: closed eyes, no nearby frame with open eyes",
                job.name(i)
            )
        } else {
            let d: Vec<String> = donors.iter().map(|&k| job.name(k)).collect();
            format!(
                "{}: closed eyes; a swap could come from {}",
                job.name(i),
                d.join(", ")
            )
        };
        job.send(Msg::Log(line));
    }
    Ok(format!(
        "{} file(s) written, {} photo(s) failed{}{}",
        sum.done.len(),
        sum.failed,
        if sum.skipped_groups > 0 {
            format!(", {} optional group(s) skipped", sum.skipped_groups)
        } else {
            String::new()
        },
        if sum.cancelled { ", cancelled" } else { "" },
    ))
}
