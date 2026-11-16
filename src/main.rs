//! `kimera`: the command line. `kimera-gui` is the window.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use kimera::analysis::{self, Report};
use kimera::apps::lightcraft::Control;
use kimera::engine::{self, Event, Extras};
use kimera::recipe::{self, Recipe};
use kimera::status::{self, Level};
use kimera::{config, writer};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
#[command(
    name = "kimera",
    version,
    about = "Kimera: a photographer's conveyor over the Craft apps (LightCraft, PhotoCraft, VectorCraft).\nNot affiliated with or endorsed by the ArtCraft team."
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
    /// Check the apps, their models, the work folder and the AI
    Doctor,
    /// Run a recipe on photos (folders and/or files); originals are never changed
    Run {
        /// Recipe file (see recipes/)
        #[arg(long)]
        recipe: PathBuf,
        /// Let the AI look at each photo first (red eyes, closed eyes)
        #[arg(long)]
        analyse: bool,
        /// Notes per photo: a JSON file {"photo path": [{"points": [[x, y], …], "text": "…"}]}
        #[arg(long)]
        notes: Option<PathBuf>,
        /// Photo files or folders (a folder gives its top-level images)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Describe the edit; the AI writes the recipe (shown, saved, and run with --run)
    Ask {
        /// What to do, in your words: "remove the pimples and give it a film look"
        request: String,
        /// Photo files or folders
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Change this recipe instead of starting over
        #[arg(long)]
        from: Option<PathBuf>,
        /// Where to save the recipe (default: <work>/recipes/ask-<time>.toml)
        #[arg(long)]
        save: Option<PathBuf>,
        /// Run the recipe right away
        #[arg(long)]
        run: bool,
        /// With --run: let the AI look at each photo first
        #[arg(long)]
        analyse: bool,
        /// Notes per photo: a JSON file {"photo path": [{"points": [[x, y], …], "text": "…"}]}
        #[arg(long)]
        notes: Option<PathBuf>,
    },
    /// Let the AI look at photos: faces, closed eyes, red eyes
    Analyse {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Export photos from a running LightCraft, then run the recipe on them
    Pipeline {
        /// Recipe file (see recipes/)
        #[arg(long)]
        recipe: PathBuf,
        /// LightCraft photo ids (default: the photos selected in the app)
        #[arg(long, value_delimiter = ',')]
        ids: Vec<u64>,
        /// Import the results back into LightCraft, stacked on their originals
        #[arg(long)]
        reimport: bool,
    },
    /// Install an optional LightCraft model through the running app (`lightcraft --control 7980`)
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// Client proof: A4 contact sheets as PDF (one per page), e.g. from a run's result folder
    Proof {
        /// Photo files or folders
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Folder for proof-01.pdf, proof-02.pdf… (default: <work>/proof-<time>)
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = 4)]
        columns: u32,
        #[arg(long, default_value_t = 5)]
        rows: u32,
    },
    /// Check recipes against the apps' real commands and the installed models, without running
    Check {
        #[arg(required = true)]
        recipes: Vec<PathBuf>,
    },
    /// Undo a `pipeline --reimport`: the imported results go to LightCraft's Recently Deleted
    Unimport {
        /// The run folder (it holds reimported.json)
        run: PathBuf,
    },
    /// List the bundled recipes
    Recipes,
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// sam3 (AI masks, ~3.4 GB), yunet (face detector, 232 KB) or lensdb (lens profiles)
    Install {
        name: String,
        /// You accept the model's licence (shown by `kimera doctor`)
        #[arg(long)]
        accept: bool,
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
    if let Cmd::Recipes = cli.cmd {
        return list_recipes();
    }
    let cfg_path = config::locate(cli.config.as_deref())?;
    let cfg = config::load(&cfg_path)?;
    let cancel = AtomicBool::new(false);
    match cli.cmd {
        Cmd::Doctor => doctor(&cfg, &cancel),
        Cmd::Run {
            recipe,
            analyse,
            notes,
            paths,
        } => {
            let mut r = recipe::load(&recipe)?;
            let files = photos(&paths)?;
            let extras = extras_for(&cfg, &files, analyse, notes.as_deref(), &mut r, &cancel)?;
            stages(
                &cfg,
                &r,
                &files,
                &kimera::run_dir(&cfg.work.dir)?,
                &extras,
                &cancel,
            )
            .and_then(|r| r.ok())
        }
        Cmd::Ask {
            request,
            paths,
            from,
            save,
            run,
            analyse,
            notes,
        } => ask(
            &cfg,
            &request,
            &paths,
            from,
            save,
            run,
            (analyse, notes),
            &cancel,
        ),
        Cmd::Analyse { paths } => look(&cfg, &photos(&paths)?, &cancel).map(|_| ()),
        Cmd::Pipeline {
            recipe,
            ids,
            reimport,
        } => pipeline(&cfg, &recipe::load(&recipe)?, ids, reimport, &cancel),
        Cmd::Models {
            cmd: ModelsCmd::Install { name, accept },
        } => {
            status::install(&cfg, &name, accept, &mut |m| println!("{m}"))?;
            println!("{name} installed");
            Ok(())
        }
        Cmd::Proof {
            paths,
            out,
            columns,
            rows,
        } => {
            let files = photos(&paths)?;
            let dir = match out {
                Some(d) => d,
                None => kimera::run_dir(&cfg.work.dir)?.with_file_name(format!(
                    "proof-{}",
                    kimera::run_dir(&cfg.work.dir)?
                        .file_name()
                        .map(|n| n.to_string_lossy().trim_start_matches("run-").to_owned())
                        .unwrap_or_default()
                )),
            };
            let pages = kimera::apps::photocraft::proof(
                &cfg.photocraft.cli,
                &files,
                &dir,
                columns,
                rows,
                &cancel,
            )?;
            for p in pages {
                println!("{}", p.display());
            }
            Ok(())
        }
        Cmd::Check { recipes } => {
            let cats = kimera::catalogs(&cfg, &cancel)?;
            let models = status::Models::read(&cfg, &cancel);
            let mut bad = 0usize;
            for p in &recipes {
                let r = recipe::load(p).and_then(|r| writer::check(&r, &cats).map(|()| r));
                match r {
                    Ok(r) => {
                        let flow: Vec<String> =
                            engine::plan(&r).into_iter().map(|s| s.dir).collect();
                        let missing = models.map(|m| m.missing_for(&r)).unwrap_or_default();
                        if missing.is_empty() {
                            println!("ok    {}  ({})", p.display(), flow.join(" -> "));
                        } else {
                            println!("needs {}: {}", p.display(), missing.join("; "));
                        }
                    }
                    Err(e) => {
                        bad = bad.saturating_add(1);
                        println!("FAIL  {}: {e:#}", p.display());
                    }
                }
            }
            if bad > 0 {
                bail!("{bad} recipe(s) failed the check");
            }
            Ok(())
        }
        Cmd::Unimport { run } => unimport(&cfg, &run),
        Cmd::Recipes => Ok(()),
    }
}

fn photos(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let files = engine::collect(paths)?;
    if files.is_empty() {
        bail!("no images found in the given paths");
    }
    Ok(files)
}

fn doctor(cfg: &config::Config, cancel: &AtomicBool) -> Result<()> {
    let checks = status::all(cfg, cancel);
    for c in &checks {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Info => "--  ",
            Level::Fail => "FAIL",
        };
        println!("  {tag} {:<14} {}", c.name, c.detail);
    }
    if checks.iter().any(|c| c.level == Level::Fail) {
        bail!("some checks failed (see above)");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ask(
    cfg: &config::Config,
    request: &str,
    paths: &[PathBuf],
    from: Option<PathBuf>,
    save: Option<PathBuf>,
    run: bool,
    (analyse, notes): (bool, Option<PathBuf>),
    cancel: &AtomicBool,
) -> Result<()> {
    let files = photos(paths)?;
    let previous = match &from {
        Some(p) => {
            Some(std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
        }
        None => None,
    };
    println!("Reading the apps' command lists…");
    let cats = kimera::catalogs(cfg, cancel)?;
    println!("Asking {} for a recipe…", cfg.ai.provider.label());
    let context = status::Models::read(cfg, cancel)
        .map(|m| m.describe())
        .unwrap_or_default();
    let mut draft = writer::write(
        &kimera::ai_client(cfg),
        &cats,
        request,
        &files,
        previous.as_deref(),
        &context,
        cancel,
    )?;
    println!("\n{}\n", draft.notes);
    println!("{}\n", draft.toml);
    let path = match save {
        Some(p) => p,
        None => {
            let dir = cfg.work.dir.join("recipes");
            std::fs::create_dir_all(&dir)?;
            kimera::run_dir(&dir)?.with_extension("toml")
        }
    };
    let header: String = draft.notes.lines().map(|l| format!("# {l}\n")).collect();
    std::fs::write(
        &path,
        format!("# Request: {request}\n{header}\n{}\n", draft.toml),
    )
    .with_context(|| format!("writing {}", path.display()))?;
    println!("Saved: {}", path.display());
    if !run {
        println!(
            "Run it with: kimera run --recipe \"{}\" {}",
            path.display(),
            paths
                .iter()
                .map(|p| format!("\"{}\"", p.display()))
                .collect::<Vec<_>>()
                .join(" ")
        );
        return Ok(());
    }
    let extras = extras_for(
        cfg,
        &files,
        analyse,
        notes.as_deref(),
        &mut draft.recipe,
        cancel,
    )?;
    stages(
        cfg,
        &draft.recipe,
        &files,
        &kimera::run_dir(&cfg.work.dir)?,
        &extras,
        cancel,
    )
    .and_then(|r| r.ok())
}

/// AI look at every photo: prints findings, returns per-photo extra steps.
fn look(cfg: &config::Config, files: &[PathBuf], cancel: &AtomicBool) -> Result<Extras> {
    let client = kimera::ai_client(cfg);
    let scratch = cfg.work.dir.join(".kimera").join("look");
    let mut reports: Vec<Option<Report>> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let name = file_name(f);
        match analysis::analyse(&client, &cfg.lightcraft.cli, f, &scratch, cancel) {
            Ok(r) => {
                println!(
                    "[{}/{}] {name}: {} face(s){}{}{}",
                    i.saturating_add(1),
                    files.len(),
                    r.faces,
                    if r.closed_eyes { ", closed eyes" } else { "" },
                    if r.red_eye.is_empty() {
                        String::new()
                    } else {
                        format!(", {} red eye(s)", r.red_eye.len())
                    },
                    if r.notes.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", r.notes)
                    }
                );
                reports.push(Some(r));
            }
            Err(e) => {
                println!(
                    "[{}/{}] {name}: not checked ({e:#})",
                    i.saturating_add(1),
                    files.len()
                );
                reports.push(None);
            }
        }
    }
    for (i, donors) in analysis::review(files, &reports) {
        let who = files.get(i).map(|f| file_name(f)).unwrap_or_default();
        if donors.is_empty() {
            println!("review: {who} has closed eyes and no nearby frame with open eyes");
        } else {
            let names: Vec<String> = donors
                .iter()
                .filter_map(|j| files.get(*j))
                .map(|f| file_name(f))
                .collect();
            println!(
                "review: {who} has closed eyes; try swapping from {}",
                names.join(", ")
            );
        }
    }
    Ok(reports
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| {
            r.map(|r| {
                (
                    i,
                    engine::PhotoExtras {
                        lightcraft: r.extra_steps(),
                        photocraft: Vec::new(),
                    },
                )
            })
        })
        .filter(|(_, x)| !x.is_empty())
        .collect())
}

/// Per-photo steps from the AI's look (`analyse`) and from the user's notes file, and the
/// stages they need added to the recipe.
fn extras_for(
    cfg: &config::Config,
    files: &[PathBuf],
    analyse: bool,
    notes: Option<&Path>,
    r: &mut Recipe,
    cancel: &AtomicBool,
) -> Result<Extras> {
    let mut extras = if analyse {
        look(cfg, files, cancel)?
    } else {
        Extras::new()
    };
    if let Some(path) = notes {
        let file = kimera::notes::load(path)?;
        let by_index = notes_by_index(files, &file);
        if by_index.is_empty() {
            println!("No photo of this run has notes in {}", path.display());
        } else {
            let cats = kimera::catalogs(cfg, cancel)?;
            kimera::notes::plan_all(
                &kimera::ai_client(cfg),
                &cats,
                &cfg.lightcraft.cli,
                files,
                &by_index,
                &cfg.work.dir.join(".kimera").join("notes-look"),
                &mut extras,
                cancel,
                &mut |l| println!("{l}"),
            )?;
        }
    }
    for app in engine::ensure_apps(r, &extras, files) {
        println!("Turned {app} on for the per-photo steps (the recipe did not use it)");
    }
    Ok(extras)
}

/// Notes keyed by photo index: the same path, or the same file name when that is unique.
fn notes_by_index(
    files: &[PathBuf],
    file: &kimera::notes::NotesFile,
) -> std::collections::HashMap<usize, Vec<kimera::notes::Note>> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
    let mut out = std::collections::HashMap::new();
    for (path, notes) in file {
        let hit = files.iter().position(|f| f == path).or_else(|| {
            let same: Vec<usize> = (0..files.len())
                .filter(|&i| files.get(i).and_then(|f| name(f)) == name(path))
                .collect();
            (same.len() == 1).then(|| same.first().copied()).flatten()
        });
        if let Some(i) = hit {
            out.insert(i, notes.clone());
        }
    }
    out
}

fn pipeline(
    cfg: &config::Config,
    r: &Recipe,
    ids: Vec<u64>,
    reimport: bool,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut lc = Control::new(&cfg.lightcraft.control)?;
    let ids = if ids.is_empty() { lc.selection()? } else { ids };
    if ids.is_empty() {
        bail!("no photos: select some in LightCraft or pass --ids");
    }
    let dir = kimera::run_dir(&cfg.work.dir)?;
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
    let ran = stages(cfg, r, &files, &dir, &Extras::new(), cancel)?;
    if reimport {
        // The photos that made it are brought back even when others failed.
        bring_back(&mut lc, &ids, &ran.finished, &dir)?;
    }
    ran.ok()
}

/// What a run produced: each finished photo's results, and how many photos failed.
struct Ran {
    finished: Vec<(usize, Vec<PathBuf>)>,
    failed: usize,
    total: usize,
}

impl Ran {
    /// The run as an exit status: an error when any photo failed.
    fn ok(&self) -> Result<()> {
        if self.failed > 0 {
            bail!("{} of {} photo(s) failed", self.failed, self.total);
        }
        Ok(())
    }
}

/// `reimported.json` in a run folder: the LightCraft ids `--reimport` added.
const REIMPORTED: &str = "reimported.json";

/// Imports finished files into LightCraft and stacks each on its original (`finished[i]` is
/// photo `i`'s results; LightCraft exported in the order of `ids`).
fn bring_back(
    lc: &mut Control,
    ids: &[u64],
    finished: &[(usize, Vec<PathBuf>)],
    dir: &Path,
) -> Result<()> {
    let mut stacked = 0usize;
    let mut added: Vec<u64> = Vec::new();
    for (photo, outs) in finished {
        let Some(orig) = ids.get(*photo) else {
            continue;
        };
        let r = lc.execute("library.import", json!({"paths": outs, "mode": "add"}))?;
        let new_ids = ids_in(&r);
        added.extend(&new_ids);
        if new_ids.is_empty() {
            println!(
                "imported {} file(s) for photo {orig}; LightCraft did not say their ids, so they are not stacked",
                outs.len()
            );
            continue;
        }
        let mut group = vec![*orig];
        group.extend(new_ids);
        lc.execute("stack.group", json!({"ids": group, "top": orig}))?;
        stacked = stacked.saturating_add(1);
    }
    println!("LightCraft: {stacked} result set(s) stacked on their originals");
    if !added.is_empty() {
        std::fs::write(dir.join(REIMPORTED), serde_json::to_string(&added)?)?;
        println!("Undo with: kimera unimport \"{}\"", dir.display());
    }
    Ok(())
}

/// Moves the photos a `--reimport` added to LightCraft's Recently Deleted (files stay on disk).
fn unimport(cfg: &config::Config, run: &Path) -> Result<()> {
    let list = run.join(REIMPORTED);
    let text = std::fs::read_to_string(&list).with_context(|| {
        format!(
            "{} has no {REIMPORTED}: nothing was reimported from it",
            run.display()
        )
    })?;
    let ids: Vec<u64> = serde_json::from_str(&text).context("reading the reimported ids")?;
    let mut lc = Control::new(&cfg.lightcraft.control)?;
    lc.execute("photo.delete", json!({"ids": ids}))?;
    std::fs::remove_file(&list)?;
    println!(
        "{} photo(s) moved to LightCraft's Recently Deleted; their files were kept",
        ids.len()
    );
    Ok(())
}

/// Photo ids in an import result: `ids: [..]`, or objects with `id` under any array.
fn ids_in(v: &Value) -> Vec<u64> {
    if let Some(a) = v.get("ids").and_then(Value::as_array) {
        return a.iter().filter_map(Value::as_u64).collect();
    }
    let mut out = Vec::new();
    if let Some(o) = v.as_object() {
        for x in o.values().filter_map(Value::as_array).flatten() {
            if let Some(id) = x.get("id").and_then(Value::as_u64) {
                out.push(id);
            }
        }
    }
    out
}

fn file_name(f: &Path) -> String {
    f.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Runs the recipe on `files` into `dir`, printing progress; returns each photo's results.
fn stages(
    cfg: &config::Config,
    r: &Recipe,
    files: &[PathBuf],
    dir: &Path,
    extras: &Extras,
    cancel: &AtomicBool,
) -> Result<Ran> {
    if let Some(m) = status::Models::read(cfg, cancel) {
        let missing = m.missing_for(r);
        if !missing.is_empty() {
            bail!(
                "this recipe needs LightCraft models that are not installed:\n  {}\n(or mark those steps optional = \"<group>\" to skip them)",
                missing.join("\n  ")
            );
        }
    }
    let plan = engine::plan(r);
    let total = files.len();
    let flow: Vec<String> = plan
        .iter()
        .map(|s| match &s.variant {
            Some(v) => format!("{}[{v}]", s.app),
            None => s.app.to_owned(),
        })
        .collect();
    println!(
        "{total} photo(s): {} -> {}",
        flow.join(" -> "),
        dir.display()
    );
    let name = |i: usize| files.get(i).map(|f| file_name(f)).unwrap_or_default();
    let app = |i: usize| flow.get(i).cloned().unwrap_or_default();
    let mut finished = Vec::new();
    let sum = engine::run(
        &kimera::apps(cfg),
        r,
        files,
        dir,
        extras,
        cancel,
        &mut |e| match e {
            Event::StageStarted { photo, slot } => {
                println!(
                    "[{}/{total}] {} : {}",
                    photo.saturating_add(1),
                    name(photo),
                    app(slot)
                );
            }
            Event::StageDone { slot, out, ms, .. } => {
                println!(
                    "        ok   {} ({:.1}s) -> {}",
                    app(slot),
                    ms as f64 / 1000.0,
                    out.display()
                );
            }
            Event::StepsSkipped { group, reason, .. } => {
                println!("        skip {group}: {reason}");
            }
            Event::PhotoFailed { photo, slot, error } => {
                eprintln!("        FAIL {} {}: {error}", app(slot), name(photo));
            }
            Event::PhotoDone { photo, outs } => {
                for o in &outs {
                    println!("        done {} => {}", name(photo), o.display());
                }
                finished.push((photo, outs));
            }
        },
    )?;
    println!(
        "done: {} file(s), {} photo(s) failed{}{} -> {}",
        sum.done.len(),
        sum.failed,
        if sum.skipped_groups > 0 {
            format!(", {} optional group(s) skipped", sum.skipped_groups)
        } else {
            String::new()
        },
        if sum.cancelled { ", cancelled" } else { "" },
        dir.display()
    );
    Ok(Ran {
        finished,
        failed: sum.failed,
        total,
    })
}

fn list_recipes() -> Result<()> {
    let dirs = [
        PathBuf::from("recipes"),
        config::beside_exe().with_file_name("recipes"),
    ];
    let Some(dir) = dirs.iter().find(|d| d.is_dir()) else {
        bail!("no recipes folder here or next to kimera.exe");
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let about = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or("")
            .to_owned();
        let ok = if recipe::parse(&text).is_ok() {
            ""
        } else {
            "  (does not parse)"
        };
        println!("{:<34} {about}{ok}", f.display());
    }
    Ok(())
}
