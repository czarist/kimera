//! The Kimera window: set the apps and the AI up, pick a folder, describe the edit (optionally
//! with finished photos whose look to match), test it on one photo, then watch the conveyor run
//! on the photos you chose.

mod jobs;
mod theme;
mod thumbs;

use crate::analysis::Report;
use crate::config::{self, Config, Provider};
use crate::engine::{self, Slot};
use crate::look::Passes;
use crate::notes::{self, Note};
use crate::recipe::{self, Stage, Step};
use crate::status::{self, Check, Level};
use eframe::egui::{self, Color32, RichText, TextureHandle, Vec2};
use jobs::Msg;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};
use theme::{ACCENT, BAD, BG, CARD, FAINT, GOOD, LINE, MUTED, WARN};

/// Long edge of the before / after pictures.
const COMPARE_SIZE: u32 = 1600;

/// Long edge of the photo in the note editor: room to circle small things.
const NOTE_EDITOR_SIZE: u32 = 1600;

/// Size of a photo's tile in the grid.
const TILE: Vec2 = Vec2::new(196.0, 147.0);

/// Lines kept in the details log.
const MAX_LOG: usize = 2000;

/// How often the window redraws while a job runs, so the time left keeps moving.
const TICK: Duration = Duration::from_millis(500);

/// First guesses, in seconds, until the run has timed its own first photo: one LightCraft or
/// PhotoCraft pass, one AI look, one AI finishing pass (a small render and a question).
const GUESS_STAGE: f64 = 8.0;
const GUESS_LOOK: f64 = 25.0;
const GUESS_FINISH_PASS: f64 = 18.0;

pub fn run() -> anyhow::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([980.0, 640.0])
        .with_title("Kimera");
    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Kimera",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone())))),
    )
    .map_err(|e| anyhow::anyhow!("could not open the window: {e}"))
}

/// The window and taskbar icon (`assets/kimera.svg`, rendered at 256 px).
fn icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../../assets/kimera-256.png"))
        .ok()?
        .to_rgba8();
    Some(egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    })
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Studio,
    Setup,
}

/// Where a photo is in a run.
#[derive(Clone, Debug, PartialEq)]
enum Place {
    Idle,
    Queued,
    /// The AI is looking at it or finishing it (what it does is in `Photo::phase`).
    Preparing,
    /// Working in this slot.
    At(usize),
    Done,
    Failed(String),
}

struct Photo {
    path: PathBuf,
    selected: bool,
    thumb: Option<TextureHandle>,
    thumb_failed: bool,
    /// The first finished file, small.
    result_thumb: Option<TextureHandle>,
    place: Place,
    phase: String,
    report: Option<Report>,
    /// The AI's sentence about its finish.
    finish_note: String,
    outs: Vec<PathBuf>,
    skipped: Vec<String>,
    /// Circled areas with what to do there.
    notes: Vec<Note>,
}

impl Photo {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            selected: true,
            thumb: None,
            thumb_failed: false,
            result_thumb: None,
            place: Place::Idle,
            phase: String::new(),
            report: None,
            finish_note: String::new(),
            outs: Vec::new(),
            skipped: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn reset_run(&mut self) {
        self.place = Place::Idle;
        self.phase.clear();
        self.outs.clear();
        self.skipped.clear();
        self.result_thumb = None;
        self.finish_note.clear();
    }
}

/// A finished photo of the user's whose look the finish matches.
struct RefPhoto {
    path: PathBuf,
    thumb: Option<TextureHandle>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Busy {
    Checking,
    Writing,
    Running,
    Testing,
    Installing,
    Proofing,
}

impl Busy {
    fn label(self) -> &'static str {
        match self {
            Busy::Checking => "Checking the apps",
            Busy::Writing => "Writing the recipe",
            Busy::Running => "Running",
            Busy::Testing => "Testing",
            Busy::Installing => "Installing",
            Busy::Proofing => "Making the proof",
        }
    }
}

/// A run's progress and its time left, from the photos it has timed so far.
struct Progress {
    test: bool,
    total: usize,
    done: usize,
    started: Instant,
    photo_started: Option<Instant>,
    current: Option<usize>,
    /// Seconds each finished photo took.
    took: Vec<f64>,
    /// The guess per photo until one is timed.
    guess: f64,
    /// What happens now, in words.
    phase: String,
}

impl Progress {
    fn per_photo(&self) -> f64 {
        if self.took.is_empty() {
            self.guess
        } else {
            self.took.iter().sum::<f64>() / self.took.len() as f64
        }
    }

    fn current_secs(&self) -> f64 {
        self.photo_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    fn fraction(&self) -> f32 {
        let within = (self.current_secs() / self.per_photo().max(1.0)).min(0.95);
        let f = (self.done as f64 + if self.current.is_some() { within } else { 0.0 })
            / self.total.max(1) as f64;
        f.clamp(0.0, 1.0) as f32
    }

    fn remaining(&self) -> f64 {
        let left = self.total.saturating_sub(self.done) as f64;
        (self.per_photo() * left - self.current_secs()).max(self.per_photo() * 0.05)
    }

    fn photo_finished(&mut self) {
        if let Some(t) = self.photo_started.take() {
            self.took.push(t.elapsed().as_secs_f64());
            self.done = (self.done + 1).min(self.total);
        }
        self.current = None;
    }
}

/// The before / after of one photo.
struct Compare {
    photo: usize,
    before: TextureHandle,
    after: Option<TextureHandle>,
    label: String,
}

pub struct App {
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    tab: Tab,

    cfg: Config,
    cfg_path: PathBuf,
    cfg_note: String,
    checks: Vec<Check>,

    folder: Option<PathBuf>,
    photos: Vec<Photo>,
    thumb_queue: Arc<thumbs::Queue>,

    prompt: String,
    notes: String,
    recipe_text: String,
    recipe_path: Option<PathBuf>,
    analyse_first: bool,
    finish: Passes,
    references: Vec<RefPhoto>,

    busy: Option<Busy>,
    /// What a non-run job (the recipe writer) is doing now.
    activity: String,
    cancel: Arc<AtomicBool>,
    plan: Vec<Slot>,
    run_dir: Option<PathBuf>,
    progress: Option<Progress>,
    /// The last run's outcome, shown until the next one.
    outcome: Option<(bool, String)>,
    banner: Option<(Color32, String)>,
    log: Vec<String>,
    show_details: bool,
    compare: Option<Compare>,
    /// Where the before / after divider is, 0..1 from the left.
    split: f32,

    /// Note editor: the photo open in it, its large picture, the loop being drawn, a closed
    /// loop waiting for its text.
    annotating: Option<usize>,
    big: Option<(PathBuf, TextureHandle)>,
    /// The photo the note editor is waiting for.
    big_loading: Option<PathBuf>,
    /// The photo whose before / after view is being decoded.
    opening: Option<usize>,
    stroke: Vec<[f32; 2]>,
    pending: Option<Vec<[f32; 2]>>,
    note_text: String,
    /// Put the cursor in the note box once, when a loop closes.
    focus_note: bool,
}

impl App {
    fn new(ctx: egui::Context) -> Self {
        theme::apply(&ctx);
        let (tx, rx) = channel();
        let cfg_path = config::locate(None).unwrap_or_else(|_| config::beside_exe());
        let (cfg, cfg_note) = match config::load(&cfg_path) {
            Ok(c) => (c, String::new()),
            Err(_) if !cfg_path.exists() => (
                Config::default(),
                "First start: check the paths below, then Save.".to_owned(),
            ),
            Err(e) => (Config::default(), format!("{e:#}")),
        };
        let first_run = !cfg_path.exists();
        let thumb_queue = thumbs::Queue::start(tx.clone(), ctx.clone(), cfg.lightcraft.cli.clone());
        let recipe_text = include_str!("../../recipes/natural.toml").to_owned();
        let mut app = Self {
            ctx,
            tx,
            rx,
            tab: if first_run { Tab::Setup } else { Tab::Studio },
            cfg,
            cfg_path,
            cfg_note,
            checks: Vec::new(),
            folder: None,
            photos: Vec::new(),
            thumb_queue,
            prompt: String::new(),
            notes: String::new(),
            recipe_text,
            recipe_path: None,
            analyse_first: false,
            finish: Passes::default(),
            references: Vec::new(),
            busy: None,
            activity: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            plan: Vec::new(),
            run_dir: None,
            progress: None,
            outcome: None,
            banner: None,
            log: Vec::new(),
            show_details: false,
            compare: None,
            split: 0.5,
            annotating: None,
            big: None,
            big_loading: None,
            opening: None,
            stroke: Vec::new(),
            pending: None,
            note_text: String::new(),
            focus_note: false,
        };
        if app.cfg.startup.open_apps && !first_run {
            app.open_apps();
        } else {
            app.start_checks();
        }
        // `kimera-gui <folder>`: "Open with" or a folder dropped on the program.
        if let Some(dir) = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
        {
            app.open_folder(dir);
            app.tab = Tab::Studio;
        }
        app
    }

    fn say(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > MAX_LOG {
            self.log.drain(..MAX_LOG / 4);
        }
    }

    /// A message the user must see (an error, a refusal): a strip over the photos, and the log.
    fn alert(&mut self, color: Color32, line: impl Into<String>) {
        let line = line.into();
        self.say(line.clone());
        self.banner = Some((color, line));
    }

    fn start_checks(&mut self) {
        self.busy = Some(Busy::Checking);
        jobs::checks(self.cfg.clone(), self.tx.clone(), self.ctx.clone());
    }

    /// Opens the Craft apps, then checks again (LightCraft's control channel is a check).
    fn open_apps(&mut self) {
        self.busy = Some(Busy::Checking);
        self.say("Opening the Craft apps…");
        jobs::open_apps(self.cfg.clone(), self.tx.clone(), self.ctx.clone());
    }

    fn selected(&self) -> Vec<PathBuf> {
        self.photos
            .iter()
            .filter(|p| p.selected)
            .map(|p| p.path.clone())
            .collect()
    }

    fn open_folder(&mut self, dir: PathBuf) {
        match engine::collect(std::slice::from_ref(&dir)) {
            Ok(files) => {
                self.photos = files.into_iter().map(Photo::new).collect();
                self.thumb_queue
                    .reset(self.photos.iter().map(|p| p.path.clone()).collect());
                let n = self.photos.len();
                self.say(format!("{n} photo(s) in {}", dir.display()));
                // Notes made earlier on this folder (kept in the work folder).
                let store = notes::store_for(&self.cfg.work.dir, &dir);
                if let Ok(saved) = notes::load(&store) {
                    let mut count = 0usize;
                    for p in &mut self.photos {
                        if let Some(n) = saved.get(&p.path) {
                            p.notes = n.clone();
                            count += n.len();
                        }
                    }
                    if count > 0 {
                        self.say(format!("{count} note(s) from before on this folder"));
                    }
                }
                self.annotating = None;
                self.big = None;
                self.compare = None;
                self.progress = None;
                self.outcome = None;
                self.folder = Some(dir);
            }
            Err(e) => self.alert(BAD, format!("Cannot open the folder: {e:#}")),
        }
    }

    fn add_references(&mut self, files: Vec<PathBuf>) {
        let files: Vec<PathBuf> = files
            .into_iter()
            .flat_map(|f| engine::collect(std::slice::from_ref(&f)).unwrap_or_default())
            .filter(|f| !self.references.iter().any(|r| &r.path == f))
            .collect();
        if files.is_empty() {
            return;
        }
        self.thumb_queue.push(files.clone());
        self.references
            .extend(files.into_iter().map(|path| RefPhoto { path, thumb: None }));
        if self.finish == Passes::Off {
            self.finish = Passes::Two;
        }
        if self.references.len() > crate::look::MAX_REFERENCES {
            self.say(format!(
                "Only the first {} reference photos are measured; {} are sent to the AI with each photo.",
                crate::look::MAX_REFERENCES,
                crate::look::MAX_REFERENCE_IMAGES
            ));
        }
    }

    fn parsed(&self) -> Result<recipe::Recipe, String> {
        recipe::parse(&self.recipe_text).map_err(|e| format!("{e:#}"))
    }

    /// The guessed seconds per photo for this recipe and these options.
    fn guess_per_photo(&self) -> f64 {
        let stages = self
            .parsed()
            .map(|r| engine::plan(&r).len())
            .unwrap_or(1)
            .max(1) as f64;
        let finish = self.finish.count() as f64;
        stages * GUESS_STAGE
            + if self.analyse_first { GUESS_LOOK } else { 0.0 }
            + finish * GUESS_FINISH_PASS
            + if finish > 0.0 { GUESS_STAGE } else { 0.0 }
    }

    fn start_write(&mut self, change: bool) {
        let request = if self.prompt.trim().is_empty() {
            "Match the look of my reference photos.".to_owned()
        } else {
            self.prompt.clone()
        };
        self.busy = Some(Busy::Writing);
        self.activity = format!("Asking {}…", self.cfg.ai.provider.label());
        self.cancel = Arc::new(AtomicBool::new(false));
        self.banner = None;
        jobs::write(jobs::WriteJob {
            cfg: self.cfg.clone(),
            request,
            photos: self.selected(),
            previous: change.then(|| self.recipe_text.clone()),
            references: self.references.iter().map(|r| r.path.clone()).collect(),
            finishing: self.finish != Passes::Off,
            cancel: self.cancel.clone(),
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        });
    }

    fn start_run(&mut self, test: bool) {
        let r = match self.parsed() {
            Ok(r) => r,
            Err(e) => {
                self.alert(BAD, format!("The recipe has a problem: {e}"));
                return;
            }
        };
        let picked: Vec<usize> = self
            .photos
            .iter()
            .enumerate()
            .filter(|(_, p)| p.selected)
            .map(|(i, _)| i)
            .collect();
        // A test runs on the photo being annotated, else the first selected one with notes,
        // else the first selected one.
        let picked: Vec<usize> = if test {
            let with_notes = picked
                .iter()
                .copied()
                .find(|&i| self.photos.get(i).is_some_and(|p| !p.notes.is_empty()));
            self.annotating
                .or(with_notes)
                .or(picked.first().copied())
                .into_iter()
                .collect()
        } else {
            picked
        };
        if picked.is_empty() {
            self.alert(WARN, "Select at least one photo.");
            return;
        }
        self.plan = engine::plan(&r);
        for p in &mut self.photos {
            p.reset_run();
        }
        for &i in &picked {
            if let Some(p) = self.photos.get_mut(i) {
                p.place = Place::Queued;
            }
        }
        self.compare = None;
        self.annotating = None;
        self.banner = None;
        self.outcome = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = Some(if test { Busy::Testing } else { Busy::Running });
        self.progress = Some(Progress {
            test,
            total: picked.len(),
            done: 0,
            started: Instant::now(),
            photo_started: None,
            current: None,
            took: Vec::new(),
            guess: self.guess_per_photo(),
            phase: "Getting ready".into(),
        });
        let files: Vec<PathBuf> = picked
            .iter()
            .filter_map(|&i| self.photos.get(i))
            .map(|p| p.path.clone())
            .collect();
        self.say(format!(
            "{} {} photo(s)…",
            if test { "Testing on" } else { "Running on" },
            files.len()
        ));
        jobs::run(jobs::RunJob {
            cfg: self.cfg.clone(),
            recipe_text: self.recipe_text.clone(),
            files,
            index: picked.clone(),
            analyse: self.analyse_first,
            finish: self.finish,
            references: self.references.iter().map(|r| r.path.clone()).collect(),
            brief: self.prompt.clone(),
            notes: picked
                .iter()
                .enumerate()
                .filter_map(|(k, &i)| {
                    self.photos
                        .get(i)
                        .filter(|p| !p.notes.is_empty())
                        .map(|p| (k, p.notes.clone()))
                })
                .collect(),
            test,
            cancel: self.cancel.clone(),
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        });
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Checks(c) => {
                self.checks = c;
                if self.busy == Some(Busy::Checking) {
                    self.busy = None;
                }
            }
            Msg::Thumb(path, img) => self.on_thumb(&path, img),
            Msg::Draft(r) => {
                self.busy = None;
                self.activity.clear();
                match r {
                    Ok((notes, toml)) => {
                        // Plain text: the notes may carry markdown emphasis and code marks.
                        self.notes = notes.replace("**", "").replace('`', "");
                        self.recipe_text = toml;
                        self.recipe_path = None;
                        self.say("The AI wrote a recipe.");
                    }
                    Err(e) => self.alert(BAD, format!("The AI could not write a recipe: {e}")),
                }
            }
            Msg::RunDir(d) => self.run_dir = Some(d),
            // The run turned an app on for per-photo steps: the board follows the real stages.
            Msg::Plan(p) => self.plan = p,
            Msg::Phase(at, line) => self.on_phase(at, line),
            Msg::Analysed(i, r) => {
                let name = self
                    .photos
                    .get(i)
                    .map(|p| file_name(&p.path))
                    .unwrap_or_default();
                match r {
                    Ok(rep) => {
                        let mut line = format!("{name}: {} face(s)", rep.faces);
                        if rep.closed_eyes {
                            line.push_str(", closed eyes: review");
                        }
                        if !rep.red_eye.is_empty() {
                            line.push_str(&format!(", {} red eye(s) to fix", rep.red_eye.len()));
                        }
                        self.say(line);
                        if let Some(p) = self.photos.get_mut(i) {
                            p.report = Some(rep);
                        }
                    }
                    Err(e) => self.say(format!("{name}: not checked ({e})")),
                }
            }
            Msg::Finished1(i, summary) => {
                if let Some(p) = self.photos.get_mut(i) {
                    let line = format!("{}: {summary}", file_name(&p.path));
                    p.finish_note = summary;
                    self.say(line);
                }
            }
            Msg::Event(index, ev) => self.on_event(&index, ev),
            Msg::Finished { test, result } => self.on_finished(test, result),
            Msg::Opened(lines) => {
                for l in lines {
                    self.say(l);
                }
                self.start_checks();
            }
            Msg::Installed(r) => {
                self.busy = None;
                match r {
                    Ok(name) => self.say(format!("{name} installed.")),
                    Err(e) => self.alert(BAD, format!("Install failed: {e}")),
                }
                self.start_checks();
            }
            Msg::Proof(r) => {
                self.busy = None;
                match r {
                    Ok(pages) => {
                        for p in &pages {
                            self.say(format!("Proof page: {}", p.display()));
                        }
                        self.alert(
                            GOOD,
                            format!(
                                "Client proof ready: {} page(s) in the run folder.",
                                pages.len()
                            ),
                        );
                    }
                    Err(e) => self.alert(BAD, format!("Proof failed: {e}")),
                }
            }
            Msg::View(v) => self.on_view(v),
            Msg::Log(l) => self.say(l),
        }
    }

    fn on_view(&mut self, v: jobs::View) {
        match v {
            jobs::View::Compare {
                photo,
                before,
                after,
                after_path,
            } => {
                // The user may have moved on meanwhile.
                if self.opening != Some(photo) {
                    return;
                }
                self.opening = None;
                let Some(before) = before else {
                    self.alert(WARN, "Cannot show the original for comparison.");
                    return;
                };
                let b = self
                    .ctx
                    .load_texture("before", before, egui::TextureOptions::LINEAR);
                let a = after.map(|img| {
                    self.ctx
                        .load_texture("after", img, egui::TextureOptions::LINEAR)
                });
                let label = after_path
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "no viewable result".into());
                self.split = 0.5;
                self.annotating = None;
                self.compare = Some(Compare {
                    photo,
                    before: b,
                    after: a,
                    label,
                });
            }
            jobs::View::Note(path, img) => {
                if self.big_loading.as_ref() != Some(&path) {
                    return;
                }
                self.big_loading = None;
                match img {
                    Some(img) => {
                        let t =
                            self.ctx
                                .load_texture("note-photo", img, egui::TextureOptions::LINEAR);
                        self.big = Some((path, t));
                    }
                    None => {
                        self.alert(WARN, format!("Cannot show {} for notes.", path.display()));
                        self.annotating = None;
                    }
                }
            }
        }
    }

    fn on_thumb(&mut self, path: &Path, img: Option<egui::ColorImage>) {
        let tex = img.map(|img| {
            self.ctx.load_texture(
                format!("thumb-{}", path.display()),
                img,
                egui::TextureOptions::LINEAR,
            )
        });
        if let Some(p) = self.photos.iter_mut().find(|p| p.path == path) {
            p.thumb_failed = tex.is_none();
            p.thumb = tex;
        } else if let Some(p) = self
            .photos
            .iter_mut()
            .find(|p| p.outs.iter().any(|o| o == path))
        {
            p.result_thumb = tex;
        } else if let Some(r) = self.references.iter_mut().find(|r| r.path == path) {
            r.thumb = tex;
        }
    }

    fn on_phase(&mut self, at: Option<usize>, line: String) {
        self.say(line.clone());
        let short = match at.and_then(|i| self.photos.get(i)) {
            Some(p) => line
                .strip_prefix(&format!("{}: ", file_name(&p.path)))
                .unwrap_or(&line)
                .to_owned(),
            None => line,
        };
        // The finish's slider values go to the details only.
        if short.starts_with("pass ") && short.contains('{') {
            return;
        }
        let short = capitalize(&short);
        match at.and_then(|i| self.photos.get_mut(i)) {
            Some(p) => {
                p.phase = short.clone();
                if let Some(pr) = &mut self.progress {
                    pr.phase = format!("{} — {short}", file_name(&p.path));
                }
            }
            None => match &mut self.progress {
                Some(pr) => pr.phase = short,
                None => self.activity = short,
            },
        }
    }

    fn on_event(&mut self, index: &[usize], ev: engine::Event) {
        use engine::Event;
        let at = |photo: usize| index.get(photo).copied();
        let slot_label = |plan: &[Slot], s: usize| {
            plan.get(s)
                .map(|s| match &s.variant {
                    Some(v) => format!("{} ({v})", s.app.label()),
                    None => s.app.label().to_owned(),
                })
                .unwrap_or_default()
        };
        match ev {
            Event::PhotoStarted { photo } => {
                if let Some(pr) = &mut self.progress {
                    pr.photo_finished();
                    pr.current = at(photo);
                    pr.photo_started = Some(Instant::now());
                }
                if let Some(p) = at(photo).and_then(|i| self.photos.get_mut(i)) {
                    p.place = Place::Preparing;
                    p.phase = "Starting".into();
                    if let Some(pr) = &mut self.progress {
                        pr.phase = format!("{} — starting", file_name(&p.path));
                    }
                }
            }
            Event::StageStarted { photo, slot } => {
                let label = slot_label(&self.plan, slot);
                if let Some(p) = at(photo).and_then(|i| self.photos.get_mut(i)) {
                    p.place = Place::At(slot);
                    p.phase = format!("{label}: applying the recipe");
                    if let Some(pr) = &mut self.progress {
                        pr.phase = format!("{} — {label} applies the recipe", file_name(&p.path));
                    }
                }
            }
            Event::StageDone { .. } => {}
            Event::StepsSkipped {
                photo,
                slot,
                group,
                reason,
            } => {
                let name = at(photo)
                    .and_then(|i| self.photos.get(i))
                    .map(|p| file_name(&p.path))
                    .unwrap_or_default();
                let line = format!(
                    "{name}: skipped \"{group}\" in {} ({reason})",
                    slot_label(&self.plan, slot)
                );
                if let Some(p) = at(photo).and_then(|i| self.photos.get_mut(i)) {
                    p.skipped.push(group);
                }
                self.say(line);
            }
            Event::PhotoFailed { photo, slot, error } => {
                let name = at(photo)
                    .and_then(|i| self.photos.get(i))
                    .map(|p| file_name(&p.path))
                    .unwrap_or_default();
                self.say(format!(
                    "{name}: FAILED in {}: {error}",
                    slot_label(&self.plan, slot)
                ));
                if let Some(p) = at(photo).and_then(|i| self.photos.get_mut(i)) {
                    p.place = Place::Failed(error);
                }
            }
            Event::PhotoDone { photo, outs } => {
                if let Some(p) = at(photo).and_then(|i| self.photos.get_mut(i)) {
                    if !matches!(p.place, Place::Failed(_)) {
                        p.place = Place::Done;
                    }
                    p.phase.clear();
                    let first: Vec<PathBuf> = outs
                        .iter()
                        .find(|o| thumbs::is_viewable(o))
                        .cloned()
                        .into_iter()
                        .collect();
                    p.outs = outs;
                    self.thumb_queue.push(first);
                }
            }
        }
    }

    fn on_finished(&mut self, test: bool, result: Result<String, String>) {
        self.busy = None;
        let took = self
            .progress
            .as_ref()
            .map(|p| p.started.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.progress = None;
        for p in &mut self.photos {
            match p.place {
                Place::Queued => p.place = Place::Idle,
                // Stopped in the middle (cancel or a stopped run).
                Place::At(_) | Place::Preparing => p.place = Place::Failed("stopped".into()),
                _ => {}
            }
            p.phase.clear();
        }
        match result {
            Ok(line) => {
                let failed = self
                    .photos
                    .iter()
                    .any(|p| matches!(p.place, Place::Failed(_)));
                self.say(format!("Finished in {}: {line}", theme::duration(took)));
                self.outcome = Some((!failed, format!("{line} · {}", theme::duration(took))));
            }
            Err(e) => self.alert(BAD, format!("The run stopped: {e}")),
        }
        if test && let Some(i) = self.photos.iter().position(|p| p.place == Place::Done) {
            self.open_compare(i);
        }
    }

    /// Before / after of one photo.
    fn open_compare(&mut self, i: usize) {
        let Some(p) = self.photos.get(i) else {
            return;
        };
        let req = jobs::ViewRequest::Compare {
            photo: i,
            before: p.path.clone(),
            after: p.outs.iter().find(|o| thumbs::is_viewable(o)).cloned(),
            size: COMPARE_SIZE,
        };
        self.opening = Some(i);
        self.say("Opening the before / after view");
        jobs::view(
            req,
            self.cfg.lightcraft.cli.clone(),
            self.tx.clone(),
            self.ctx.clone(),
        );
    }

    // ---------- drawing ----------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(RichText::new("◆").size(20.0).color(ACCENT));
            ui.label(RichText::new("Kimera").size(20.0).strong());
            ui.add_space(18.0);
            for (tab, label) in [(Tab::Studio, "Studio"), (Tab::Setup, "Setup")] {
                let on = self.tab == tab;
                let text = RichText::new(label).color(if on { theme::TEXT } else { MUTED });
                let b = egui::Button::new(text)
                    .fill(if on {
                        theme::CARD_HI
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(egui::CornerRadius::same(255));
                if ui.add(b).clicked() {
                    self.tab = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                let fails = self
                    .checks
                    .iter()
                    .filter(|c| c.level == Level::Fail)
                    .count();
                if self.busy == Some(Busy::Checking) {
                    theme::chip(ui, "checking the apps…", MUTED);
                } else if fails > 0 {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("● {fails} setup problem(s)")).color(BAD),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Open Setup")
                        .clicked()
                    {
                        self.tab = Tab::Setup;
                    }
                } else if !self.checks.is_empty() {
                    theme::chip(ui, "● apps ready", GOOD);
                }
                ui.label(
                    RichText::new(format!("AI: {}", self.cfg.ai.provider.label()))
                        .small()
                        .color(FAINT),
                );
            });
        });
    }

    fn setup(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_max_width(900.0);
            if !self.cfg_note.is_empty() {
                let note = self.cfg_note.clone();
                if theme::banner(ui, WARN, &note) {
                    self.cfg_note.clear();
                }
                ui.add_space(8.0);
            }
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::eyebrow(ui, "", "Apps and folders");
                theme::hint(ui, &format!("Settings file: {}", self.cfg_path.display()));
                ui.add_space(6.0);
                egui::Grid::new("paths").num_columns(3).spacing([10.0, 10.0]).show(ui, |ui| {
                    path_row(ui, "LightCraft CLI", &mut self.cfg.lightcraft.cli, false);
                    path_row(ui, "PhotoCraft CLI", &mut self.cfg.photocraft.cli, false);
                    let mut has_vc = self.cfg.vectorcraft.is_some();
                    ui.label("VectorCraft");
                    if ui.checkbox(&mut has_vc, "use VectorCraft (layouts, posters)").changed() {
                        self.cfg.vectorcraft = has_vc.then(|| config::VectorCraft {
                            cli: PathBuf::from(r"C:\Program Files\VectorCraft\vectorcraft-cli.exe"),
                        });
                    }
                    ui.end_row();
                    if let Some(vc) = &mut self.cfg.vectorcraft {
                        path_row(ui, "VectorCraft CLI", &mut vc.cli, false);
                    }
                    path_row(ui, "Work folder", &mut self.cfg.work.dir, true);
                    ui.label("LightCraft control");
                    ui.text_edit_singleline(&mut self.cfg.lightcraft.control);
                    theme::hint(ui, "only for catalog export and model installs");
                    ui.end_row();
                    ui.label("At start");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.cfg.startup.open_apps, "open the Craft apps");
                        ui.add_enabled_ui(self.cfg.startup.open_apps, |ui| {
                            egui::ComboBox::from_id_salt("window_style")
                                .selected_text(self.cfg.startup.window.label())
                                .show_ui(ui, |ui| {
                                    for w in config::WindowStyle::ALL {
                                        ui.selectable_value(&mut self.cfg.startup.window, w, w.label());
                                    }
                                });
                        });
                    });
                    ui.end_row();
                });
            });
            ui.add_space(10.0);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::eyebrow(ui, "", "AI");
                ui.add_space(4.0);
                egui::Grid::new("ai").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
                    ui.label("Provider");
                    egui::ComboBox::from_id_salt("provider")
                        .selected_text(self.cfg.ai.provider.label())
                        .show_ui(ui, |ui| {
                            for p in Provider::ALL {
                                ui.selectable_value(&mut self.cfg.ai.provider, p, p.label());
                            }
                        });
                    ui.end_row();
                    ui.label("Model");
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.ai.model).hint_text(match self.cfg.ai.provider.default_model() {
                        "" => "the CLI's default",
                        m => m,
                    }));
                    ui.end_row();
                    if self.cfg.ai.provider.uses_api_key() {
                        ui.label("Key variable");
                        ui.add(egui::TextEdit::singleline(&mut self.cfg.ai.api_key_env).hint_text(self.cfg.ai.provider.default_key_env()));
                        ui.end_row();
                        ui.label("Or the key itself");
                        ui.add(egui::TextEdit::singleline(&mut self.cfg.ai.api_key).password(true).hint_text("stored in plain text in kimera.toml"));
                        ui.end_row();
                    } else {
                        ui.label("CLI path");
                        let mut s = self.cfg.ai.cli.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                        if ui.add(egui::TextEdit::singleline(&mut s).hint_text("found on PATH")).changed() {
                            self.cfg.ai.cli = (!s.trim().is_empty()).then(|| PathBuf::from(s.trim()));
                        }
                        ui.end_row();
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(theme::primary("Save settings")).clicked() {
                    match config::save(&self.cfg_path, &self.cfg) {
                        Ok(()) => {
                            self.cfg_note.clear();
                            self.thumb_queue.set_lightcraft(self.cfg.lightcraft.cli.clone());
                            self.alert(GOOD, format!("Settings saved to {}", self.cfg_path.display()));
                            self.start_checks();
                        }
                        Err(e) => self.alert(BAD, format!("Could not save: {e:#}")),
                    }
                }
                if ui.add_enabled(self.busy.is_none(), theme::secondary("Check again")).clicked() {
                    self.start_checks();
                }
                if ui.add_enabled(self.busy.is_none(), theme::secondary("Open apps")).clicked() {
                    self.open_apps();
                }
            });
            if let Some((color, text)) = self.banner.clone()
                && theme::banner(ui, color, &text)
            {
                self.banner = None;
            }
            ui.add_space(10.0);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::eyebrow(ui, "", "Checks");
                ui.add_space(4.0);
                // The click is acted on after the loop, so the checks are only borrowed.
                let idle = self.busy.is_none();
                let mut install = None;
                for c in &self.checks {
                    ui.horizontal(|ui| {
                        let col = match c.level {
                            Level::Ok => GOOD,
                            Level::Info => WARN,
                            Level::Fail => BAD,
                        };
                        ui.label(RichText::new("●").color(col));
                        ui.label(RichText::new(c.name).strong());
                        ui.label(RichText::new(&c.detail).color(MUTED));
                        let model = c.model.map(|m| {
                            let licence = match m {
                                status::Model::Sam3 => "SAM 3 (about 3.4 GB, Meta's SAM License)",
                                status::Model::Yunet => "YuNet (232 KB, MIT)",
                                status::Model::LensDb => "lensfun database (CC BY-SA 3.0)",
                            };
                            (m, licence)
                        });
                        if let (Level::Info, Some((name, licence))) = (c.level, model)
                            && ui
                                .add_enabled(idle, theme::secondary(format!("Install {name}")))
                                .on_hover_text(format!(
                                    "Downloads {licence} through the running LightCraft (lightcraft --control). Clicking means you accept its licence."
                                ))
                                .clicked()
                        {
                            install = Some(name);
                        }
                    });
                }
                if self.checks.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        theme::hint(ui, "checking…");
                    });
                }
                if let Some(name) = install {
                    self.busy = Some(Busy::Installing);
                    jobs::install(self.cfg.clone(), name, self.tx.clone(), self.ctx.clone());
                }
            });
        });
    }

    fn studio(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("left")
            .resizable(true)
            .default_size(400.0)
            .size_range(340.0..=560.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| self.left(ui));
        self.status_bar(ui);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| self.right(ui));
    }

    /// One line at the bottom: the latest message, and the full log on demand.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status")
            .exact_size(30.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let last = self.log.last().cloned().unwrap_or_default();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.show_details {
                            "Hide details"
                        } else {
                            "Details"
                        };
                        if ui.small_button(label).clicked() {
                            self.show_details = !self.show_details;
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(last).small().color(FAINT))
                                    .truncate(),
                            );
                        });
                    });
                });
            });
        if self.show_details {
            egui::Panel::bottom("details")
                .resizable(true)
                .default_size(220.0)
                .frame(
                    egui::Frame::new()
                        .fill(BG)
                        .inner_margin(egui::Margin::same(10)),
                )
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::eyebrow(ui, "", "Details");
                        if let Some(d) = &self.run_dir {
                            theme::hint(ui, &format!("run folder: {}", d.display()));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text(self.log.join("\n"));
                            }
                        });
                    });
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for l in &self.log {
                                ui.label(RichText::new(l).monospace().color(MUTED));
                            }
                        });
                });
        }
    }

    fn left(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            self.photos_card(ui);
            self.request_card(ui);
            self.reference_card(ui);
            self.recipe_card(ui);
        });
    }

    fn photos_card(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::eyebrow(ui, "1", "Photos");
            let label = if self.folder.is_some() {
                "Change folder…"
            } else {
                "Choose a folder…"
            };
            let b = if self.folder.is_some() {
                theme::secondary(label)
            } else {
                theme::primary(label)
            };
            if ui.add_enabled(self.busy.is_none(), b).clicked()
                && let Some(d) = rfd::FileDialog::new().pick_folder()
            {
                self.open_folder(d);
            }
            if let Some(f) = &self.folder {
                ui.add(
                    egui::Label::new(RichText::new(f.display().to_string()).small().color(MUTED))
                        .truncate(),
                )
                .on_hover_text(f.display().to_string());
            }
            if !self.photos.is_empty() {
                let n = self.photos.iter().filter(|p| p.selected).count();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{n} of {} selected", self.photos.len())).strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Invert").clicked() {
                            self.photos
                                .iter_mut()
                                .for_each(|p| p.selected = !p.selected);
                        }
                        if ui.small_button("None").clicked() {
                            self.photos.iter_mut().for_each(|p| p.selected = false);
                        }
                        if ui.small_button("All").clicked() {
                            self.photos.iter_mut().for_each(|p| p.selected = true);
                        }
                    });
                });
            }
        });
    }

    fn request_card(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::eyebrow(ui, "2", "What should happen?");
            ui.add(
                egui::TextEdit::multiline(&mut self.prompt)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::same(8))
                    .hint_text("e.g. clean skin, whiter teeth and a warm, cinematic look; feed and stories versions"),
            );
            let writing = self.busy == Some(Busy::Writing);
            ui.horizontal(|ui| {
                let can = self.busy.is_none()
                    && (!self.prompt.trim().is_empty() || !self.references.is_empty());
                if ui
                    .add_enabled(can, theme::primary("✨ Write the recipe"))
                    .on_hover_text("The AI writes a recipe from your words (and your reference photos)")
                    .clicked()
                {
                    self.start_write(false);
                }
                if ui
                    .add_enabled(
                        can && !self.recipe_text.trim().is_empty(),
                        theme::secondary("Adjust it"),
                    )
                    .on_hover_text("Ask the AI to change the current recipe instead of starting over")
                    .clicked()
                {
                    self.start_write(true);
                }
            });
            if writing {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(&self.activity).color(MUTED));
                    if ui.small_button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }
            if !self.notes.is_empty() {
                egui::Frame::new()
                    .fill(ACCENT.gamma_multiply(0.07))
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("The AI's plan").small().strong().color(ACCENT));
                        ui.label(&self.notes);
                    });
            }
        });
    }

    fn reference_card(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::eyebrow(ui, "3", "Finish and reference look");
            ui.horizontal(|ui| {
                ui.label("AI finish");
                egui::ComboBox::from_id_salt("finish")
                    .selected_text(self.finish.label())
                    .show_ui(ui, |ui| {
                        for p in Passes::ALL {
                            ui.selectable_value(&mut self.finish, p, p.label());
                        }
                    });
            });
            theme::hint(
                ui,
                match self.finish {
                    Passes::Off => "Only the recipe runs: the same settings on every photo.",
                    Passes::One => "The AI looks at each photo and sets its tone and colour.",
                    Passes::Two => {
                        "The AI sets each photo's tone and colour, looks at the result and corrects it."
                    }
                },
            );
            ui.add_space(4.0);
            ui.label(RichText::new("Reference photos (optional)").strong());
            theme::hint(
                ui,
                "Finished photos of yours whose look the AI should match: their tones, colours and mood.",
            );
            let mut remove = None;
            if !self.references.is_empty() {
                let side = 64.0;
                ui.horizontal_wrapped(|ui| {
                    for (i, r) in self.references.iter().enumerate() {
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::splat(side), egui::Sense::click());
                        ui.painter().rect_filled(rect, 6.0, BG);
                        if let Some(t) = &r.thumb {
                            let img = egui::Rect::from_center_size(
                                rect.center(),
                                fit(t.size_vec2(), rect.size()),
                            );
                            ui.painter().image(t.id(), img, uv_full(), Color32::WHITE);
                        } else {
                            ui.put(rect, egui::Spinner::new().size(14.0));
                        }
                        if resp.hovered() {
                            ui.painter()
                                .rect_filled(rect, 6.0, Color32::from_black_alpha(140));
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "✕",
                                egui::FontId::proportional(18.0),
                                theme::TEXT,
                            );
                        }
                        if resp
                            .on_hover_text(format!("{}\nClick to remove", file_name(&r.path)))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    }
                });
            }
            if let Some(i) = remove
                && i < self.references.len()
            {
                self.references.remove(i);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.busy.is_none(), theme::secondary("Add photos…"))
                    .clicked()
                    && let Some(files) = rfd::FileDialog::new()
                        .add_filter("photos", engine::IMAGE_EXTS)
                        .pick_files()
                {
                    self.add_references(files);
                }
                if ui
                    .add_enabled(self.busy.is_none(), theme::secondary("Add a folder…"))
                    .clicked()
                    && let Some(d) = rfd::FileDialog::new().pick_folder()
                {
                    self.add_references(vec![d]);
                }
                if !self.references.is_empty() && ui.small_button("Clear").clicked() {
                    self.references.clear();
                }
            });
            ui.add_space(4.0);
            ui.checkbox(&mut self.analyse_first, "AI checks faces first (red eyes, blinks)")
                .on_hover_text("Fixes red eyes in LightCraft and flags closed eyes for review, with nearby frames to swap from");
        });
    }

    fn recipe_card(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::eyebrow(ui, "4", "Recipe");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Save as…").clicked()
                        && let Some(f) = rfd::FileDialog::new()
                            .add_filter("recipe", &["toml"])
                            .set_file_name("recipe.toml")
                            .save_file()
                    {
                        match std::fs::write(&f, &self.recipe_text) {
                            Ok(()) => {
                                self.say(format!("Recipe saved to {}", f.display()));
                                self.recipe_path = Some(f);
                            }
                            Err(e) => self.alert(BAD, format!("Cannot save the recipe: {e}")),
                        }
                    }
                    if ui.small_button("Open…").clicked()
                        && let Some(f) = rfd::FileDialog::new()
                            .add_filter("recipe", &["toml"])
                            .set_directory("recipes")
                            .pick_file()
                    {
                        match std::fs::read_to_string(&f) {
                            Ok(t) => {
                                self.recipe_text = t;
                                self.notes.clear();
                                self.recipe_path = Some(f);
                            }
                            Err(e) => self.alert(BAD, format!("Cannot read the recipe: {e}")),
                        }
                    }
                });
            });
            if let Some(p) = &self.recipe_path {
                theme::hint(ui, &file_name(p));
            }
            match self.parsed() {
                Ok(r) => recipe_summary(ui, &r),
                Err(e) => {
                    ui.colored_label(BAD, format!("✖ {e}"));
                }
            }
            ui.add_space(2.0);
            egui::CollapsingHeader::new(RichText::new("Edit the recipe file").small().color(MUTED))
                .id_salt("toml")
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.recipe_text)
                            .code_editor()
                            .desired_rows(14)
                            .desired_width(f32::INFINITY),
                    );
                });
        });
    }

    /// The bar over the photos: the buttons and the time a run will take, or the run's
    /// progress while it goes.
    fn action_bar(&mut self, ui: &mut egui::Ui) {
        if let Some((color, text)) = self.banner.clone() {
            if theme::banner(ui, color, &text) {
                self.banner = None;
            }
            ui.add_space(8.0);
        }
        if let Some(pr) = &self.progress {
            let title = if pr.test {
                "Testing on 1 photo"
            } else {
                "Running"
            };
            let (phase, frac, left, elapsed) = (
                pr.phase.clone(),
                pr.fraction(),
                pr.remaining(),
                pr.started.elapsed().as_secs_f64(),
            );
            let (done, total, timed) = (pr.done, pr.total, !pr.took.is_empty());
            let mut cancel = false;
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(title).strong().size(16.0));
                    ui.label(
                        RichText::new(format!("· {done} of {total} photo(s) done")).color(MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::secondary("Cancel")).clicked() {
                            cancel = true;
                        }
                        ui.label(
                            RichText::new(format!(
                                "{}about {} left · {} so far",
                                if timed { "" } else { "first guess: " },
                                theme::duration(left),
                                theme::duration(elapsed)
                            ))
                            .color(MUTED),
                        );
                    });
                });
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(8.0)
                        .fill(ACCENT)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
                ui.label(RichText::new(phase).color(theme::TEXT));
            });
            if cancel {
                self.cancel.store(true, Ordering::Relaxed);
                self.say("Cancelling…");
                if let Some(pr) = &mut self.progress {
                    pr.phase = "Cancelling…".into();
                }
            }
            ui.add_space(10.0);
            return;
        }
        let n = self.photos.iter().filter(|p| p.selected).count();
        let can = self.busy.is_none() && n > 0;
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can, theme::secondary("Test on 1 photo"))
                    .on_hover_text("Runs everything on one photo and shows before / after")
                    .clicked()
                {
                    self.start_run(true);
                }
                if ui
                    .add_enabled(can, theme::primary(format!("Run on {n} photo(s)")))
                    .clicked()
                {
                    self.start_run(false);
                }
                if n > 0 {
                    let per = self.guess_per_photo();
                    ui.label(
                        RichText::new(format!(
                            "≈ {} per photo · {} in all",
                            theme::duration(per),
                            theme::duration(per * n as f64)
                        ))
                        .small()
                        .color(MUTED),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(d) = self.run_dir.clone() {
                        let finished: Vec<PathBuf> = self
                            .photos
                            .iter()
                            .flat_map(|p| p.outs.iter())
                            .filter(|o| thumbs::is_viewable(o))
                            .cloned()
                            .collect();
                        if !finished.is_empty()
                            && ui
                                .add_enabled(
                                    self.busy.is_none(),
                                    theme::secondary("Client proof (PDF)"),
                                )
                                .on_hover_text(
                                    "A4 contact sheets of the results, 20 per page, in the run folder",
                                )
                                .clicked()
                        {
                            self.busy = Some(Busy::Proofing);
                            self.say(format!(
                                "Making the client proof of {} file(s)…",
                                finished.len()
                            ));
                            jobs::proof(
                                self.cfg.clone(),
                                finished,
                                d.join("proof"),
                                self.tx.clone(),
                                self.ctx.clone(),
                            );
                        }
                        if ui.add(theme::secondary("Open run folder")).clicked() {
                            let _ = std::process::Command::new("explorer").arg(d).spawn();
                        }
                    }
                    if let Some(b) = self.busy
                        && matches!(b, Busy::Proofing | Busy::Installing)
                    {
                        ui.spinner();
                        ui.label(RichText::new(b.label()).color(MUTED));
                    }
                });
            });
            if let Some((ok, text)) = &self.outcome {
                ui.horizontal(|ui| {
                    let (mark, col) = if *ok { ("✔", GOOD) } else { ("!", WARN) };
                    ui.label(RichText::new(mark).color(col).strong());
                    ui.label(RichText::new(text).color(MUTED));
                });
            }
        });
        ui.add_space(10.0);
    }

    /// Saves every photo's notes for this folder (in the work folder).
    fn save_notes(&mut self) {
        let Some(folder) = &self.folder else { return };
        let all: notes::NotesFile = self
            .photos
            .iter()
            .filter(|p| !p.notes.is_empty())
            .map(|p| (p.path.clone(), p.notes.clone()))
            .collect();
        if let Err(e) = notes::save(&notes::store_for(&self.cfg.work.dir, folder), &all) {
            self.alert(BAD, format!("Could not save the notes: {e:#}"));
        }
    }

    /// One photo, large: drag to circle an area, then say what should happen there.
    fn annotate(&mut self, ui: &mut egui::Ui, idx: usize) {
        let Some(path) = self.photos.get(idx).map(|p| p.path.clone()) else {
            self.annotating = None;
            return;
        };
        if self.big.as_ref().is_none_or(|(p, _)| *p != path) {
            if self.big_loading.as_ref() != Some(&path) {
                self.big_loading = Some(path.clone());
                jobs::view(
                    jobs::ViewRequest::Note(path.clone(), NOTE_EDITOR_SIZE),
                    self.cfg.lightcraft.cli.clone(),
                    self.tx.clone(),
                    self.ctx.clone(),
                );
            }
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                theme::hint(ui, "Opening the photo");
            });
            return;
        }
        let Some((_, tex)) = self.big.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            if ui.add(theme::secondary("← All photos")).clicked() {
                self.annotating = None;
                self.pending = None;
                self.stroke.clear();
            }
            ui.label(
                RichText::new(format!("Notes on {}", file_name(&path)))
                    .size(17.0)
                    .strong(),
            );
            theme::hint(
                ui,
                "Drag on the photo to circle an area, then say what should happen there.",
            );
        });
        ui.add_space(6.0);
        // Picture, fitted above the notes list.
        let room = Vec2::new(
            ui.available_width(),
            (ui.available_height() - 190.0).max(200.0),
        );
        let size = fit(tex.size_vec2(), room);
        let resp = ui
            .vertical_centered(|ui| {
                ui.add(
                    egui::Image::new(&tex)
                        .fit_to_exact_size(size)
                        .sense(egui::Sense::drag()),
                )
            })
            .inner;
        let rect = resp.rect;
        let norm = |p: egui::Pos2| {
            [
                ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
                ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0),
            ]
        };
        if resp.drag_started() {
            self.stroke.clear();
            self.pending = None;
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let n = norm(p);
            if self
                .stroke
                .last()
                .is_none_or(|l| (l[0] - n[0]).abs() + (l[1] - n[1]).abs() > 0.002)
            {
                self.stroke.push(n);
            }
        }
        if resp.drag_stopped() {
            if self.stroke.len() >= 3 {
                self.pending = Some(std::mem::take(&mut self.stroke));
                self.focus_note = true;
                self.note_text.clear();
            } else {
                self.stroke.clear();
            }
        }
        let to_screen = |p: &[f32; 2]| {
            egui::pos2(
                rect.min.x + p[0] * rect.width(),
                rect.min.y + p[1] * rect.height(),
            )
        };
        let painter = ui.painter_at(rect);
        let photo_notes = self
            .photos
            .get(idx)
            .map(|p| p.notes.clone())
            .unwrap_or_default();
        let color = |i: usize| {
            let [r, g, b] = notes::COLORS[i % notes::COLORS.len()];
            Color32::from_rgb(r, g, b)
        };
        for (i, n) in photo_notes.iter().enumerate() {
            let pts: Vec<egui::Pos2> = n.points.iter().map(to_screen).collect();
            painter.add(egui::Shape::closed_line(
                pts.clone(),
                egui::Stroke::new(3.0, color(i)),
            ));
            if let Some(p) = pts.first() {
                painter.circle_filled(*p, 12.0, color(i));
                painter.text(
                    *p,
                    egui::Align2::CENTER_CENTER,
                    format!("{}", i + 1),
                    egui::FontId::proportional(14.0),
                    Color32::BLACK,
                );
            }
        }
        let next = color(photo_notes.len());
        if !self.stroke.is_empty() {
            let pts: Vec<egui::Pos2> = self.stroke.iter().map(to_screen).collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(3.0, next)));
        }
        if let Some(pend) = &self.pending {
            let pts: Vec<egui::Pos2> = pend.iter().map(to_screen).collect();
            painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(3.0, next)));
        }
        // The loop waiting for its text.
        if self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.colored_label(next, format!("Note {}:", photo_notes.len() + 1));
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.note_text)
                        .desired_width(ui.available_width() - 160.0)
                        .hint_text(
                            "e.g. remove this sign · brighten her face · blur the background here",
                        ),
                );
                if std::mem::take(&mut self.focus_note) {
                    edit.request_focus();
                }
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ok = !self.note_text.trim().is_empty();
                if (ui.add_enabled(ok, theme::primary("Add")).clicked() || (enter && ok))
                    && let Some(points) = self.pending.take()
                {
                    let text = std::mem::take(&mut self.note_text).trim().to_owned();
                    if let Some(p) = self.photos.get_mut(idx) {
                        p.notes.push(Note { points, text });
                        p.selected = true;
                    }
                    self.save_notes();
                }
                if ui.add(theme::secondary("Discard")).clicked() {
                    self.pending = None;
                }
            });
        }
        let mut remove = None;
        egui::ScrollArea::vertical().id_salt("note-list").show(ui, |ui| {
            for (i, n) in photo_notes.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.colored_label(color(i), format!("{} ·", i + 1));
                    ui.label(&n.text);
                    if ui.small_button("Delete").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if photo_notes.is_empty() && self.pending.is_none() {
                theme::hint(
                    ui,
                    "No notes yet. They are applied when this photo runs, on top of the recipe.",
                );
            }
        });
        if let Some(i) = remove
            && let Some(p) = self.photos.get_mut(idx)
        {
            if i < p.notes.len() {
                p.notes.remove(i);
            }
            self.save_notes();
        }
    }

    fn right(&mut self, ui: &mut egui::Ui) {
        if self.photos.is_empty() {
            if let Some((color, text)) = self.banner.clone()
                && theme::banner(ui, color, &text)
            {
                self.banner = None;
            }
            empty_state(ui);
            return;
        }
        self.action_bar(ui);
        if let Some(i) = self.annotating {
            self.annotate(ui, i);
            return;
        }
        if self.compare.is_some() {
            self.compare_view(ui);
            return;
        }
        self.grid(ui);
    }

    /// Before and after in one frame, with a divider to drag between them.
    fn compare_view(&mut self, ui: &mut egui::Ui) {
        let Some(c) = &self.compare else { return };
        let (before, after, label, photo) =
            (c.before.clone(), c.after.clone(), c.label.clone(), c.photo);
        let (name, note, out) = self
            .photos
            .get(photo)
            .map(|p| {
                (
                    file_name(&p.path),
                    p.finish_note.clone(),
                    p.outs.first().cloned(),
                )
            })
            .unwrap_or_default();
        let mut close = false;
        let mut step: Option<isize> = None;
        ui.horizontal(|ui| {
            if ui.add(theme::secondary("← All photos")).clicked() {
                close = true;
            }
            ui.label(RichText::new(name).size(17.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(theme::secondary("Next →")).clicked() {
                    step = Some(1);
                }
                if ui.add(theme::secondary("← Previous")).clicked() {
                    step = Some(-1);
                }
                if let Some(out) = &out
                    && ui.add(theme::secondary("Show file")).clicked()
                {
                    let _ = std::process::Command::new("explorer")
                        .arg(format!("/select,{}", out.display()))
                        .spawn();
                }
            });
        });
        if !note.is_empty() {
            ui.label(RichText::new(format!("AI finish: {note}")).color(MUTED));
        }
        ui.add_space(6.0);
        let room = Vec2::new(
            ui.available_width(),
            (ui.available_height() - 28.0).max(200.0),
        );
        let size = fit(before.size_vec2(), room);
        let (rect, resp) = ui
            .vertical_centered(|ui| ui.allocate_exact_size(size, egui::Sense::click_and_drag()))
            .inner;
        if (resp.dragged() || resp.clicked())
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.split = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        }
        let painter = ui.painter_at(rect);
        painter.image(before.id(), rect, uv_full(), Color32::WHITE);
        let x = rect.left() + self.split * rect.width();
        match &after {
            Some(a) => {
                let right = egui::Rect::from_min_max(egui::pos2(x, rect.top()), rect.max);
                let uv =
                    egui::Rect::from_min_max(egui::pos2(self.split, 0.0), egui::pos2(1.0, 1.0));
                painter.image(a.id(), right, uv, Color32::WHITE);
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    egui::Stroke::new(2.0, Color32::WHITE),
                );
                let knob = egui::pos2(x, rect.center().y);
                painter.circle_filled(knob, 14.0, Color32::WHITE);
                painter.text(
                    knob,
                    egui::Align2::CENTER_CENTER,
                    "↔",
                    egui::FontId::proportional(15.0),
                    BG,
                );
                tag(
                    &painter,
                    rect.left_top() + Vec2::new(10.0, 10.0),
                    egui::Align2::LEFT_TOP,
                    "Before",
                );
                tag(
                    &painter,
                    rect.right_top() + Vec2::new(-10.0, 10.0),
                    egui::Align2::RIGHT_TOP,
                    "After",
                );
            }
            None => tag(
                &painter,
                rect.left_top() + Vec2::new(10.0, 10.0),
                egui::Align2::LEFT_TOP,
                "The result is not a picture the window can show (PDF, PCRAFT…)",
            ),
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        theme::hint(ui, &format!("Drag across the photo to compare · {label}"));
        if close {
            self.compare = None;
        }
        if let Some(d) = step {
            let done: Vec<usize> = self
                .photos
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.outs.is_empty())
                .map(|(i, _)| i)
                .collect();
            if let Some(pos) = done.iter().position(|&i| i == photo) {
                let len = done.len() as isize;
                let next = (pos as isize + d).rem_euclid(len.max(1));
                if let Some(&i) = done.get(next as usize) {
                    self.open_compare(i);
                }
            }
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let gap = 12.0;
        let card_w = TILE.x + 16.0;
        let per_row = (((ui.available_width() + gap) / (card_w + gap)).floor() as usize).max(1);
        let idle = self.busy.is_none();
        let mut open_notes = None;
        let mut open_compare = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(gap);
                for (r, row) in self.photos.chunks_mut(per_row).enumerate() {
                    ui.horizontal(|ui| {
                        for (c, p) in row.iter_mut().enumerate() {
                            let idx = r * per_row + c;
                            match ui.vertical(|ui| photo_card(ui, p, idle)).inner {
                                CardAction::None => {}
                                CardAction::Notes => open_notes = Some(idx),
                                CardAction::Compare => open_compare = Some(idx),
                            }
                        }
                    });
                }
            });
        if let Some(i) = open_notes {
            self.annotating = Some(i);
            self.pending = None;
            self.stroke.clear();
        }
        if let Some(i) = open_compare {
            self.open_compare(i);
        }
    }
}

enum CardAction {
    None,
    Notes,
    Compare,
}

/// One photo in the grid: its picture (the result once there is one), state and actions.
fn photo_card(ui: &mut egui::Ui, p: &mut Photo, idle: bool) -> CardAction {
    let mut action = CardAction::None;
    let stroke = match &p.place {
        Place::Preparing | Place::At(_) => egui::Stroke::new(2.0, ACCENT),
        Place::Failed(_) => egui::Stroke::new(1.5, BAD),
        _ if p.selected => egui::Stroke::new(1.5, ACCENT.gamma_multiply(0.6)),
        _ => egui::Stroke::new(1.0, LINE),
    };
    egui::Frame::new()
        .fill(CARD)
        .stroke(stroke)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(TILE.x);
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            let (rect, resp) = ui.allocate_exact_size(TILE, egui::Sense::click());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 6.0, BG);
            match p.result_thumb.as_ref().or(p.thumb.as_ref()) {
                Some(t) => {
                    let img = egui::Rect::from_center_size(
                        rect.center(),
                        fit(t.size_vec2(), rect.size()),
                    );
                    painter.image(t.id(), img, uv_full(), Color32::WHITE);
                }
                None if p.thumb_failed => {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "no preview",
                        egui::FontId::proportional(12.0),
                        FAINT,
                    );
                }
                None => {
                    ui.put(
                        egui::Rect::from_center_size(rect.center(), Vec2::splat(18.0)),
                        egui::Spinner::new().size(18.0),
                    );
                }
            }
            // Photos left out of the run are dimmed.
            if !p.selected && p.place == Place::Idle {
                painter.rect_filled(rect, 6.0, Color32::from_black_alpha(130));
            }
            let mark = rect.left_top() + Vec2::new(14.0, 14.0);
            if p.selected {
                painter.circle_filled(mark, 9.0, ACCENT);
                painter.text(
                    mark,
                    egui::Align2::CENTER_CENTER,
                    "✔",
                    egui::FontId::proportional(11.0),
                    theme::ON_ACCENT,
                );
            } else {
                painter.circle_stroke(
                    mark,
                    8.0,
                    egui::Stroke::new(1.5, Color32::from_white_alpha(200)),
                );
            }
            let badge = rect.right_top() + Vec2::new(-14.0, 14.0);
            match &p.place {
                Place::Queued => tag(
                    &painter,
                    rect.right_top() + Vec2::new(-8.0, 8.0),
                    egui::Align2::RIGHT_TOP,
                    "waiting",
                ),
                Place::Preparing | Place::At(_) => {
                    painter.rect_filled(rect, 6.0, Color32::from_black_alpha(100));
                    ui.put(
                        egui::Rect::from_center_size(rect.center(), Vec2::splat(28.0)),
                        egui::Spinner::new().size(28.0).color(ACCENT),
                    );
                }
                Place::Done => {
                    painter.circle_filled(badge, 10.0, GOOD);
                    painter.text(
                        badge,
                        egui::Align2::CENTER_CENTER,
                        "✔",
                        egui::FontId::proportional(12.0),
                        BG,
                    );
                }
                Place::Failed(_) => {
                    painter.circle_filled(badge, 10.0, BAD);
                    painter.text(
                        badge,
                        egui::Align2::CENTER_CENTER,
                        "!",
                        egui::FontId::proportional(13.0),
                        Color32::WHITE,
                    );
                }
                Place::Idle => {}
            }
            if resp.hovered() {
                painter.rect_stroke(
                    rect,
                    6.0,
                    egui::Stroke::new(1.0, Color32::from_white_alpha(70)),
                    egui::StrokeKind::Inside,
                );
            }
            let mut hover = file_name(&p.path);
            if !p.outs.is_empty() {
                hover.push_str("\nClick to compare before / after");
            } else if idle {
                hover.push_str("\nClick to select · double-click for notes");
            }
            if !p.finish_note.is_empty() {
                hover.push_str(&format!("\n\nAI finish: {}", p.finish_note));
            }
            if let Place::Failed(e) = &p.place {
                hover.push_str(&format!("\n\nFailed: {e}"));
            }
            let resp = resp.on_hover_text(hover);
            if resp.double_clicked() && p.outs.is_empty() {
                action = CardAction::Notes;
            } else if resp.clicked() {
                if !p.outs.is_empty() {
                    action = CardAction::Compare;
                } else if idle {
                    p.selected = !p.selected;
                }
            }
            ui.add(egui::Label::new(RichText::new(file_name(&p.path)).small().strong()).truncate());
            let (line, col) = match &p.place {
                Place::Preparing | Place::At(_) => (p.phase.clone(), ACCENT),
                Place::Failed(e) => (e.clone(), BAD),
                Place::Done if !p.skipped.is_empty() => {
                    (format!("done · skipped {}", p.skipped.join(", ")), MUTED)
                }
                Place::Done => ("done · click to compare".to_owned(), MUTED),
                Place::Queued => ("waiting its turn".to_owned(), FAINT),
                Place::Idle => (String::new(), MUTED),
            };
            ui.horizontal(|ui| {
                let label = if p.notes.is_empty() {
                    "✏ Notes".to_owned()
                } else {
                    format!("✏ {} note(s)", p.notes.len())
                };
                if ui
                    .add_enabled(
                        idle,
                        egui::Button::new(RichText::new(label).small())
                            .min_size(Vec2::new(0.0, 22.0)),
                    )
                    .on_hover_text("Circle a part of this photo and say what to do there")
                    .clicked()
                {
                    action = CardAction::Notes;
                }
                if let Some(r) = &p.report {
                    if r.closed_eyes {
                        ui.label(RichText::new("blink?").small().color(WARN))
                            .on_hover_text("Closed eyes: review this one");
                    }
                    if !r.red_eye.is_empty() {
                        ui.label(
                            RichText::new(format!("{} red eye(s)", r.red_eye.len()))
                                .small()
                                .color(MUTED),
                        );
                    }
                }
            });
            if !line.is_empty() {
                ui.add(egui::Label::new(RichText::new(line).small().color(col)).truncate());
            }
        });
    action
}

/// The Studio before any folder is open.
fn empty_state(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(40.0));
        ui.label(RichText::new("◆").size(46.0).color(ACCENT));
        ui.add_space(6.0);
        ui.label(
            RichText::new("Choose a folder of photos to start")
                .size(22.0)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Describe the edit, add a few finished photos whose look you like, test on one photo, then run them all.\nYour originals are never changed: every run writes into a folder of its own.",
            )
            .color(MUTED),
        );
    });
}

/// A dark label over a picture.
fn tag(painter: &egui::Painter, pos: egui::Pos2, anchor: egui::Align2, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        Color32::WHITE,
    );
    let rect = anchor.anchor_size(pos, galley.size() + Vec2::new(12.0, 6.0));
    painter.rect_filled(rect, 5.0, Color32::from_black_alpha(170));
    painter.galley(rect.min + Vec2::new(6.0, 3.0), galley, Color32::WHITE);
}

/// The recipe in words: its stages and what each does.
fn recipe_summary(ui: &mut egui::Ui, r: &recipe::Recipe) {
    let section = |ui: &mut egui::Ui, title: &str, stages: &[Stage]| {
        for s in stages {
            ui.horizontal(|ui| {
                theme::chip(ui, s.app().label(), ACCENT);
                if !title.is_empty() {
                    ui.label(RichText::new(title).small().color(MUTED));
                }
                ui.label(
                    RichText::new(format!("→ {}", s.out_ext().to_uppercase()))
                        .small()
                        .color(FAINT),
                );
            });
            let steps = s.steps();
            if steps.is_empty() {
                theme::hint(ui, "   no steps of its own");
            }
            for st in steps {
                let mut line = format!("   •  {}", describe_step(st));
                if st.optional.is_some() {
                    line.push_str("  (when found)");
                }
                ui.label(RichText::new(line).small());
            }
        }
    };
    section(ui, "", &r.stages);
    for v in &r.variants {
        section(ui, &format!("version “{}”", v.name), &v.stages);
    }
}

/// One step in plain words.
fn describe_step(s: &Step) -> String {
    let p = s.params.as_ref();
    let get = |k: &str| p.and_then(|v| v.get(k));
    let text = |k: &str| {
        get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let values = |k: &str| {
        get(k)
            .and_then(|v| v.as_table())
            .map(|t| {
                let mut parts: Vec<String> = t
                    .iter()
                    .take(4)
                    .map(|(k, v)| {
                        let name = k.rsplit('.').next().unwrap_or(k);
                        let n = v
                            .as_float()
                            .or_else(|| v.as_integer().map(|i| i as f64))
                            .unwrap_or(0.0);
                        let n = if n > 0.0 {
                            format!("+{n}")
                        } else {
                            n.to_string()
                        };
                        format!("{} {n}", words(name))
                    })
                    .collect();
                if t.len() > 4 {
                    parts.push(format!("+{} more", t.len() - 4));
                }
                parts.join(", ")
            })
            .unwrap_or_default()
    };
    match s.cmd.as_str() {
        "develop.set" => format!("Tone & colour: {}", values("values")),
        "develop.auto" => "Auto tone".into(),
        "develop.treatment" => "Black & white".into(),
        "develop.autoBwMix" => "Black & white (auto mix)".into(),
        "preset.apply" => format!("Preset {}", text("id")),
        "mask.add" => match text("kind").as_str() {
            "prompt" => format!("Select “{}”", text("text")),
            "" => "New mask".into(),
            k => format!("Select the {k}"),
        },
        "mask.adjust" => format!("   adjust it: {}", values("values")),
        "spot.findBlemishes" => "Remove skin blemishes".into(),
        "redeye.add" => "Fix a red eye".into(),
        other => capitalize(&words(other.rsplit('.').next().unwrap_or(other))),
    }
}

/// `findBlemishes` → `find blemishes`.
fn words(id: &str) -> String {
    let mut out = String::new();
    for ch in id.chars() {
        if ch.is_uppercase() {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The largest size of `image`'s shape inside `room`.
fn fit(image: Vec2, room: Vec2) -> Vec2 {
    let scale = (room.x / image.x.max(1.0)).min(room.y / image.y.max(1.0));
    image * scale
}

fn uv_full() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        if self.busy.is_some() {
            ui.ctx().request_repaint_after(TICK);
        }
        egui::Panel::top("top")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(12, 10)),
            )
            .show(ui, |ui| self.top_bar(ui));
        match self.tab {
            Tab::Setup => {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(BG)
                            .inner_margin(egui::Margin::same(20)),
                    )
                    .show(ui, |ui| self.setup(ui));
            }
            Tab::Studio => self.studio(ui),
        }
    }
}

fn path_row(ui: &mut egui::Ui, label: &str, path: &mut PathBuf, folder: bool) {
    ui.label(label);
    let mut s = path.display().to_string();
    if ui
        .add(egui::TextEdit::singleline(&mut s).desired_width(460.0))
        .changed()
    {
        *path = PathBuf::from(s);
    }
    if ui.button("Browse…").clicked() {
        let picked = if folder {
            rfd::FileDialog::new().pick_folder()
        } else {
            rfd::FileDialog::new()
                .add_filter("program", &["exe", "cmd", ""])
                .pick_file()
        };
        if let Some(p) = picked {
            *path = p;
        }
    }
    ui.end_row();
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_read_as_words() -> anyhow::Result<()> {
        let r = recipe::parse(include_str!("../../recipes/portrait-retouch.toml"))?;
        let lines: Vec<String> = r
            .stages
            .iter()
            .flat_map(|s| s.steps())
            .map(describe_step)
            .collect();
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Remove skin blemishes")
        );
        assert!(lines.iter().any(|l| l == "Select “teeth”"));
        assert_eq!(words("findBlemishes"), "find blemishes");
        Ok(())
    }

    #[test]
    fn progress_counts_down_from_timed_photos() {
        let mut p = Progress {
            test: false,
            total: 4,
            done: 0,
            started: Instant::now(),
            photo_started: None,
            current: None,
            took: Vec::new(),
            guess: 60.0,
            phase: String::new(),
        };
        assert!((p.remaining() - 240.0).abs() < 1.0);
        p.took = vec![10.0, 30.0];
        p.done = 2;
        assert!((p.remaining() - 40.0).abs() < 1.0);
        assert!((p.fraction() - 0.5).abs() < 0.01);
    }

    #[test]
    fn fit_keeps_the_shape() {
        let s = fit(Vec2::new(3064.0, 5456.0), Vec2::new(196.0, 147.0));
        assert!((s.y - 147.0).abs() < 0.01 && s.x < 196.0);
    }
}
