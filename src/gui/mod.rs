//! The Kimera window: set the apps and the AI up, pick a folder, describe the edit, check the
//! recipe, test it on one photo, then watch the conveyor run on the photos you chose.

mod jobs;
mod thumbs;

use crate::analysis::Report;
use crate::config::{self, Config, Provider};
use crate::engine::{self, Slot};
use crate::notes::{self, Note};
use crate::recipe;
use crate::status::{Check, Level};
use eframe::egui::{self, Color32, RichText, TextureHandle, Vec2};
use jobs::Msg;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Kimera"),
        ..Default::default()
    };
    eframe::run_native(
        "Kimera",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone())))),
    )
    .map_err(|e| anyhow::anyhow!("could not open the window: {e}"))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Conveyor,
    Setup,
}

/// Where a photo is on the board.
#[derive(Clone, Debug, PartialEq)]
enum Place {
    Idle,
    Queued,
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
    place: Place,
    report: Option<Report>,
    outs: Vec<PathBuf>,
    skipped: Vec<String>,
    /// Circled areas with what to do there.
    notes: Vec<Note>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Busy {
    Checking,
    Writing,
    Running,
    Testing,
    Installing,
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

    busy: Option<Busy>,
    cancel: Arc<AtomicBool>,
    plan: Vec<Slot>,
    run_dir: Option<PathBuf>,
    log: Vec<String>,
    compare: Option<(TextureHandle, Option<TextureHandle>, String)>,
    /// The photo whose result the before / after shows.
    tested: Option<usize>,

    /// Note editor: the photo open in it, its large picture, the loop being drawn, a closed
    /// loop waiting for its text.
    annotating: Option<usize>,
    big: Option<(PathBuf, TextureHandle)>,
    stroke: Vec<[f32; 2]>,
    pending: Option<Vec<[f32; 2]>>,
    note_text: String,
    /// Put the cursor in the note box once, when a loop closes.
    focus_note: bool,
}

impl App {
    fn new(ctx: egui::Context) -> Self {
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
        let recipe_text = include_str!("../../recipes/cleanup-cinematic.toml").to_owned();
        let mut app = Self {
            ctx,
            tx,
            rx,
            tab: if first_run { Tab::Setup } else { Tab::Conveyor },
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
            busy: None,
            cancel: Arc::new(AtomicBool::new(false)),
            plan: Vec::new(),
            run_dir: None,
            log: Vec::new(),
            compare: None,
            tested: None,
            annotating: None,
            big: None,
            stroke: Vec::new(),
            pending: None,
            note_text: String::new(),
            focus_note: false,
        };
        app.start_checks();
        // `kimera-gui <folder>`: "Open with" or a folder dropped on the program.
        if let Some(dir) = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
        {
            app.open_folder(dir);
            app.tab = Tab::Conveyor;
        }
        app
    }

    fn say(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 2000 {
            self.log.drain(..500);
        }
    }

    fn start_checks(&mut self) {
        self.busy = Some(Busy::Checking);
        jobs::checks(self.cfg.clone(), self.tx.clone(), self.ctx.clone());
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
                self.photos = files
                    .into_iter()
                    .map(|path| Photo {
                        path,
                        selected: true,
                        thumb: None,
                        thumb_failed: false,
                        place: Place::Idle,
                        report: None,
                        outs: Vec::new(),
                        skipped: Vec::new(),
                        notes: Vec::new(),
                    })
                    .collect();
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
                self.folder = Some(dir);
            }
            Err(e) => self.say(format!("Cannot open the folder: {e:#}")),
        }
    }

    fn parsed(&self) -> Result<recipe::Recipe, String> {
        recipe::parse(&self.recipe_text).map_err(|e| format!("{e:#}"))
    }

    fn start_run(&mut self, test: bool) {
        let r = match self.parsed() {
            Ok(r) => r,
            Err(e) => {
                self.say(format!("The recipe has a problem: {e}"));
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
        self.tested = picked.first().copied();
        if picked.is_empty() {
            self.say("Select at least one photo.");
            return;
        }
        self.plan = engine::plan(&r);
        for p in &mut self.photos {
            p.place = Place::Idle;
            p.outs.clear();
            p.skipped.clear();
        }
        for &i in &picked {
            if let Some(p) = self.photos.get_mut(i) {
                p.place = Place::Queued;
            }
        }
        self.compare = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = Some(if test { Busy::Testing } else { Busy::Running });
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
            Msg::Thumb(path, img) => {
                if let Some(p) = self.photos.iter_mut().find(|p| p.path == path) {
                    match img {
                        Some(img) => {
                            p.thumb = Some(self.ctx.load_texture(
                                format!("thumb-{}", path.display()),
                                img,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                        None => p.thumb_failed = true,
                    }
                }
            }
            Msg::Draft(r) => {
                self.busy = None;
                match r {
                    Ok((notes, toml)) => {
                        // Plain text: the notes may carry markdown emphasis and code marks.
                        self.notes = notes.replace("**", "").replace('`', "");
                        self.recipe_text = toml;
                        self.recipe_path = None;
                        self.say("The AI wrote a recipe: check it, then test it on one photo.");
                    }
                    Err(e) => self.say(format!("The AI could not write a recipe: {e}")),
                }
            }
            Msg::RunDir(d) => self.run_dir = Some(d),
            // The run turned an app on for per-photo steps: the board follows the real stages.
            Msg::Plan(p) => self.plan = p,
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
            Msg::Event(index, ev) => self.on_event(&index, ev),
            Msg::Finished { test, result } => {
                self.busy = None;
                match result {
                    Ok(line) => self.say(line),
                    Err(e) => self.say(format!("Stopped: {e}")),
                }
                for p in &mut self.photos {
                    match p.place {
                        Place::Queued => p.place = Place::Idle,
                        // Stopped in the middle of a stage (cancel or a stopped run).
                        Place::At(_) => p.place = Place::Failed("cancelled".into()),
                        _ => {}
                    }
                }
                if test {
                    // Show before / after even when the test started from the note editor.
                    self.annotating = None;
                    self.load_compare();
                }
            }
            Msg::Installed(r) => {
                self.busy = None;
                match r {
                    Ok(name) => self.say(format!("{name} installed.")),
                    Err(e) => self.say(format!("Install failed: {e}")),
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
                    }
                    Err(e) => self.say(format!("Proof failed: {e}")),
                }
            }
            Msg::Log(l) => self.say(l),
        }
    }

    fn on_event(&mut self, index: &[usize], ev: engine::Event) {
        use engine::Event;
        let at = |photo: usize| index.get(photo).copied();
        let slot_name =
            |plan: &[Slot], s: usize| plan.get(s).map(|s| s.dir.clone()).unwrap_or_default();
        match ev {
            Event::StageStarted { photo, slot } => {
                if let Some(p) = at(photo).and_then(|i| self.photos.get_mut(i)) {
                    p.place = Place::At(slot);
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
                    slot_name(&self.plan, slot)
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
                    slot_name(&self.plan, slot)
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
                    p.outs = outs;
                }
            }
        }
    }

    /// Before/after of the tested photo.
    fn load_compare(&mut self) {
        let Some(p) = self.tested.and_then(|i| self.photos.get(i)) else {
            return;
        };
        let before = thumbs::load_rgba(&p.path, &self.cfg.lightcraft.cli, 1400);
        let after_path = p.outs.iter().find(|o| thumbs::is_viewable(o)).cloned();
        let after = after_path
            .as_ref()
            .and_then(|o| thumbs::load_rgba(o, &self.cfg.lightcraft.cli, 1400));
        let Some(before) = before else {
            self.say("Cannot show the original for comparison.");
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
        self.compare = Some((b, a, label));
    }

    // ---------- drawing ----------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Kimera").strong());
            ui.add_space(12.0);
            ui.selectable_value(&mut self.tab, Tab::Conveyor, "Conveyor");
            ui.selectable_value(&mut self.tab, Tab::Setup, "Setup");
            ui.add_space(12.0);
            let fails = self
                .checks
                .iter()
                .filter(|c| c.level == Level::Fail)
                .count();
            if self.busy == Some(Busy::Checking) {
                ui.spinner();
                ui.label("checking…");
            } else if fails > 0 {
                ui.colored_label(
                    Color32::from_rgb(220, 80, 70),
                    format!("{fails} setup problem(s)"),
                );
            } else if !self.checks.is_empty() {
                ui.colored_label(Color32::from_rgb(80, 180, 110), "ready");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(b) = self.busy {
                    if matches!(b, Busy::Running | Busy::Testing | Busy::Writing)
                        && ui.button("Cancel").clicked()
                    {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.say("Cancelling…");
                    }
                    ui.spinner();
                    ui.label(format!("{b:?}…"));
                }
            });
        });
    }

    fn setup(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            if !self.cfg_note.is_empty() {
                ui.colored_label(Color32::from_rgb(230, 170, 60), &self.cfg_note);
            }
            ui.label(format!("Settings file: {}", self.cfg_path.display()));
            ui.add_space(8.0);
            egui::Grid::new("paths").num_columns(3).spacing([10.0, 8.0]).show(ui, |ui| {
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
                ui.label("only for catalog export and model installs");
                ui.end_row();
            });
            ui.separator();
            ui.heading("AI");
            egui::Grid::new("ai").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
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
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    match config::save(&self.cfg_path, &self.cfg) {
                        Ok(()) => {
                            self.cfg_note.clear();
                            self.thumb_queue.set_lightcraft(self.cfg.lightcraft.cli.clone());
                            self.say(format!("Settings saved to {}", self.cfg_path.display()));
                            self.start_checks();
                        }
                        Err(e) => self.say(format!("Could not save: {e:#}")),
                    }
                }
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("Check again")).clicked() {
                    self.start_checks();
                }
            });
            ui.separator();
            ui.heading("Checks");
            for c in self.checks.clone() {
                ui.horizontal(|ui| {
                    let (txt, col) = match c.level {
                        Level::Ok => ("ok", Color32::from_rgb(80, 180, 110)),
                        Level::Info => ("--", Color32::from_rgb(200, 160, 60)),
                        Level::Fail => ("FAIL", Color32::from_rgb(220, 80, 70)),
                    };
                    ui.colored_label(col, RichText::new(txt).monospace());
                    ui.label(RichText::new(c.name).strong());
                    ui.label(&c.detail);
                    let model = match c.name {
                        "AI masks" => Some(("sam3", "SAM 3 (about 3.4 GB, Meta's SAM License)")),
                        "Face detector" => Some(("yunet", "YuNet (232 KB, MIT)")),
                        "Lens database" => Some(("lensdb", "lensfun database (CC BY-SA 3.0)")),
                        _ => None,
                    };
                    if let (Level::Info, Some((name, licence))) = (c.level, model)
                        && ui
                            .add_enabled(self.busy.is_none(), egui::Button::new(format!("Install {name}")))
                            .on_hover_text(format!(
                                "Downloads {licence} through the running LightCraft (lightcraft --control). Clicking means you accept its licence."
                            ))
                            .clicked()
                    {
                        self.busy = Some(Busy::Installing);
                        jobs::install(self.cfg.clone(), name.to_owned(), self.tx.clone(), self.ctx.clone());
                    }
                });
            }
        });
    }

    fn conveyor(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("left")
            .resizable(true)
            .default_size(430.0)
            .size_range(340.0..=560.0)
            .show(ui, |ui| self.left(ui));
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(150.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Log");
                    if let Some(d) = &self.run_dir {
                        ui.label(format!("· run folder: {}", d.display()));
                    }
                });
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.label(RichText::new(l).monospace().size(12.0));
                        }
                    });
            });
        egui::CentralPanel::default().show(ui, |ui| self.right(ui));
    }

    fn left(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("1 · Photos");
            ui.horizontal(|ui| {
                if ui.button("Choose folder…").clicked()
                    && let Some(d) = rfd::FileDialog::new().pick_folder()
                {
                    self.open_folder(d);
                }
                if let Some(f) = &self.folder {
                    ui.add(egui::Label::new(RichText::new(f.display().to_string()).small()).truncate())
                        .on_hover_text(f.display().to_string());
                }
            });
            if !self.photos.is_empty() {
                let n = self.photos.iter().filter(|p| p.selected).count();
                ui.horizontal(|ui| {
                    ui.label(format!("{n} of {} selected", self.photos.len()));
                    if ui.small_button("All").clicked() {
                        self.photos.iter_mut().for_each(|p| p.selected = true);
                    }
                    if ui.small_button("None").clicked() {
                        self.photos.iter_mut().for_each(|p| p.selected = false);
                    }
                    if ui.small_button("Invert").clicked() {
                        self.photos.iter_mut().for_each(|p| p.selected = !p.selected);
                    }
                });
            }
            ui.add_space(10.0);
            ui.heading("2 · What should happen?");
            ui.add(
                egui::TextEdit::multiline(&mut self.prompt)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. remove pimples, whiten teeth and give it a cinematic look; feed and stories versions"),
            );
            ui.horizontal(|ui| {
                let can = self.busy.is_none() && !self.prompt.trim().is_empty();
                if ui.add_enabled(can, egui::Button::new("Write recipe with AI")).clicked() {
                    self.busy = Some(Busy::Writing);
                    self.cancel = Arc::new(AtomicBool::new(false));
                    self.say(format!("Asking {} for a recipe…", self.cfg.ai.provider.label()));
                    jobs::write(
                        self.cfg.clone(),
                        self.prompt.clone(),
                        self.selected(),
                        None,
                        self.cancel.clone(),
                        self.tx.clone(),
                        self.ctx.clone(),
                    );
                }
                if ui
                    .add_enabled(can && !self.recipe_text.trim().is_empty(), egui::Button::new("Change current recipe"))
                    .on_hover_text("Ask the AI to adjust the recipe below instead of starting over")
                    .clicked()
                {
                    self.busy = Some(Busy::Writing);
                    self.cancel = Arc::new(AtomicBool::new(false));
                    jobs::write(
                        self.cfg.clone(),
                        self.prompt.clone(),
                        self.selected(),
                        Some(self.recipe_text.clone()),
                        self.cancel.clone(),
                        self.tx.clone(),
                        self.ctx.clone(),
                    );
                }
            });
            if !self.notes.is_empty() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(&self.notes);
                });
            }
            ui.add_space(10.0);
            ui.heading("3 · Recipe");
            ui.horizontal(|ui| {
                if ui.button("Open…").clicked()
                    && let Some(f) = rfd::FileDialog::new().add_filter("recipe", &["toml"]).set_directory("recipes").pick_file()
                {
                    match std::fs::read_to_string(&f) {
                        Ok(t) => {
                            self.recipe_text = t;
                            self.notes.clear();
                            self.recipe_path = Some(f);
                        }
                        Err(e) => self.say(format!("Cannot read the recipe: {e}")),
                    }
                }
                if ui.button("Save as…").clicked()
                    && let Some(f) = rfd::FileDialog::new().add_filter("recipe", &["toml"]).set_file_name("recipe.toml").save_file()
                {
                    match std::fs::write(&f, &self.recipe_text) {
                        Ok(()) => {
                            self.say(format!("Recipe saved to {}", f.display()));
                            self.recipe_path = Some(f);
                        }
                        Err(e) => self.say(format!("Cannot save the recipe: {e}")),
                    }
                }
                if let Some(p) = &self.recipe_path {
                    ui.label(RichText::new(file_name(p)).small());
                }
            });
            match self.parsed() {
                Ok(r) => {
                    let flow: Vec<String> = engine::plan(&r).iter().map(|s| s.dir.clone()).collect();
                    ui.colored_label(Color32::from_rgb(80, 180, 110), format!("✔ {}", flow.join(" → ")));
                    // Which apps this recipe turns on; the others stay off.
                    let used = r.apps_used();
                    ui.horizontal(|ui| {
                        for (app, label) in [
                            ("lightcraft", "LightCraft"),
                            ("photocraft", "PhotoCraft"),
                            ("vectorcraft", "VectorCraft"),
                        ] {
                            if used.contains(&app) {
                                ui.colored_label(Color32::from_rgb(80, 180, 110), format!("● {label} on"));
                            } else {
                                ui.label(RichText::new(format!("○ {label} off")).weak());
                            }
                        }
                    });
                }
                Err(e) => {
                    ui.colored_label(Color32::from_rgb(220, 80, 70), format!("✖ {e}"));
                }
            }
            ui.add(
                egui::TextEdit::multiline(&mut self.recipe_text)
                    .code_editor()
                    .desired_rows(12)
                    .desired_width(f32::INFINITY),
            );
        });
    }

    /// Always on top of the photos: test, run, and the AI look.
    fn run_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let can = self.busy.is_none() && self.photos.iter().any(|p| p.selected);
            if ui
                .add_enabled(can, egui::Button::new("Test on 1 photo"))
                .on_hover_text("Runs the recipe on the first selected photo and shows before / after")
                .clicked()
            {
                self.start_run(true);
            }
            let n = self.photos.iter().filter(|p| p.selected).count();
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(RichText::new(format!("Run on {n} photo(s)")).strong())
                        .fill(Color32::from_rgb(40, 110, 70)),
                )
                .clicked()
            {
                self.start_run(false);
            }
            ui.checkbox(&mut self.analyse_first, "AI looks at each photo first")
                .on_hover_text("Finds red eyes (fixed in LightCraft) and closed eyes (flagged for review, with nearby frames to swap from)");
            if let Some(d) = self.run_dir.clone()
                && ui.button("Open run folder").clicked()
            {
                let _ = std::process::Command::new("explorer").arg(d).spawn();
            }
            let finished: Vec<PathBuf> = self
                .photos
                .iter()
                .flat_map(|p| p.outs.iter())
                .filter(|o| thumbs::is_viewable(o))
                .cloned()
                .collect();
            if let Some(d) = self.run_dir.clone()
                && !finished.is_empty()
                && ui
                    .add_enabled(self.busy.is_none(), egui::Button::new("Client proof (PDF)"))
                    .on_hover_text("A4 contact sheets of the results, 20 per page, in the run folder")
                    .clicked()
            {
                self.busy = Some(Busy::Installing);
                self.say(format!("Making the client proof of {} file(s)…", finished.len()));
                jobs::proof(self.cfg.clone(), finished, d.join("proof"), self.tx.clone(), self.ctx.clone());
            }
        });
        ui.separator();
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
            self.say(format!("Could not save the notes: {e:#}"));
        }
    }

    /// One photo, large: drag to circle an area, then say what should happen there.
    fn annotate(&mut self, ui: &mut egui::Ui, idx: usize) {
        let Some(path) = self.photos.get(idx).map(|p| p.path.clone()) else {
            self.annotating = None;
            return;
        };
        if self.big.as_ref().is_none_or(|(p, _)| *p != path) {
            match thumbs::load_rgba(&path, &self.cfg.lightcraft.cli, 1600) {
                Some(img) => {
                    let t = self
                        .ctx
                        .load_texture("note-photo", img, egui::TextureOptions::LINEAR);
                    self.big = Some((path.clone(), t));
                }
                None => {
                    self.say(format!("Cannot show {} for notes.", path.display()));
                    self.annotating = None;
                    return;
                }
            }
        }
        let Some((_, tex)) = self.big.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.heading(format!("Notes on {}", file_name(&path)));
            if ui.button("Done").clicked() {
                self.annotating = None;
                self.pending = None;
                self.stroke.clear();
            }
            ui.label(
                RichText::new(
                    "Drag on the photo to circle an area, then say what should happen there.",
                )
                .weak(),
            );
        });
        // Picture, fitted above the notes list.
        let [w, h] = tex.size();
        let room = Vec2::new(
            ui.available_width(),
            (ui.available_height() - 190.0).max(200.0),
        );
        let scale = (room.x / w.max(1) as f32).min(room.y / h.max(1) as f32);
        let size = Vec2::new(w as f32 * scale, h as f32 * scale);
        let resp = ui.add(
            egui::Image::new(&tex)
                .fit_to_exact_size(size)
                .sense(egui::Sense::drag()),
        );
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
                painter.text(
                    *p,
                    egui::Align2::CENTER_CENTER,
                    format!("{}", i + 1),
                    egui::FontId::proportional(20.0),
                    color(i),
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
                if (ui.add_enabled(ok, egui::Button::new("Add")).clicked() || (enter && ok))
                    && let Some(points) = self.pending.take()
                {
                    let text = std::mem::take(&mut self.note_text).trim().to_owned();
                    if let Some(p) = self.photos.get_mut(idx) {
                        p.notes.push(Note { points, text });
                        p.selected = true;
                    }
                    self.save_notes();
                }
                if ui.button("Discard").clicked() {
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
                ui.label(RichText::new("No notes yet. They are applied when this photo runs, on top of the recipe.").weak());
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
        if !self.photos.is_empty() {
            self.run_bar(ui);
        }
        if let Some(i) = self.annotating {
            self.annotate(ui, i);
            return;
        }
        if let Some((before, after, label)) = &self.compare {
            let (before, after, label) = (before.clone(), after.clone(), label.clone());
            ui.horizontal(|ui| {
                ui.heading("Test result");
                if ui.button("Close").clicked() {
                    self.compare = None;
                }
            });
            ui.label(RichText::new(label).small());
            let w = (ui.available_width() / 2.0 - 8.0).max(100.0);
            let max_h = (ui.available_height() * 0.6).max(220.0);
            ui.horizontal(|ui| {
                show_fit(ui, &before, w, max_h, "Before");
                match &after {
                    Some(a) => show_fit(ui, a, w, max_h, "After"),
                    None => {
                        ui.label("The result is not an image the window can show (PDF, PCRAFT…).");
                    }
                }
            });
            ui.separator();
        }
        if self.photos.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Choose a folder of photos to start.").size(18.0));
            });
            return;
        }
        let running = !self.plan.is_empty() && self.photos.iter().any(|p| p.place != Place::Idle);
        if running {
            self.board(ui);
        } else {
            self.grid(ui);
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let per_row = ((ui.available_width() / 190.0).floor() as usize).max(1);
        let mut open_notes = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (r, row) in self.photos.chunks_mut(per_row).enumerate() {
                ui.horizontal(|ui| {
                    for (c, p) in row.iter_mut().enumerate() {
                        let idx = r * per_row + c;
                        egui::Frame::group(ui.style())
                            .fill(if p.selected {
                                ui.visuals().selection.bg_fill.gamma_multiply(0.35)
                            } else {
                                ui.visuals().panel_fill
                            })
                            .show(ui, |ui| {
                                ui.set_width(170.0);
                                ui.vertical(|ui| {
                                    let resp = match &p.thumb {
                                        Some(t) => ui.add(
                                            egui::Image::new(t)
                                                .fit_to_exact_size(Vec2::new(160.0, 120.0))
                                                .sense(egui::Sense::click()),
                                        ),
                                        None => {
                                            let (r, resp) = ui.allocate_exact_size(
                                                Vec2::new(160.0, 120.0),
                                                egui::Sense::click(),
                                            );
                                            ui.painter().rect_filled(
                                                r,
                                                4.0,
                                                ui.visuals().extreme_bg_color,
                                            );
                                            let msg =
                                                if p.thumb_failed { "no preview" } else { "…" };
                                            ui.painter().text(
                                                r.center(),
                                                egui::Align2::CENTER_CENTER,
                                                msg,
                                                egui::FontId::proportional(13.0),
                                                ui.visuals().weak_text_color(),
                                            );
                                            resp
                                        }
                                    };
                                    if resp.clicked() {
                                        p.selected = !p.selected;
                                    }
                                    if resp.double_clicked() {
                                        open_notes = Some(idx);
                                    }
                                    ui.checkbox(
                                        &mut p.selected,
                                        RichText::new(file_name(&p.path)).small(),
                                    );
                                    let label = if p.notes.is_empty() {
                                        "✏ Notes…".to_owned()
                                    } else {
                                        format!("✏ {} note(s)", p.notes.len())
                                    };
                                    if ui
                                        .small_button(label)
                                        .on_hover_text(
                                            "Circle a part of this photo and say what to do there",
                                        )
                                        .clicked()
                                    {
                                        open_notes = Some(idx);
                                    }
                                    if let Some(r) = &p.report {
                                        if r.closed_eyes {
                                            ui.colored_label(
                                                Color32::from_rgb(230, 170, 60),
                                                "closed eyes: review",
                                            );
                                        }
                                        if !r.red_eye.is_empty() {
                                            ui.small(format!("{} red eye(s)", r.red_eye.len()));
                                        }
                                    }
                                });
                            });
                    }
                });
            }
        });
        if let Some(i) = open_notes {
            self.annotating = Some(i);
            self.pending = None;
            self.stroke.clear();
        }
    }

    fn board(&mut self, ui: &mut egui::Ui) {
        let mut cols: Vec<(String, Vec<usize>)> = vec![("Queue".into(), Vec::new())];
        for s in &self.plan {
            cols.push((s.dir.clone(), Vec::new()));
        }
        cols.push(("Done".into(), Vec::new()));
        cols.push(("Failed".into(), Vec::new()));
        let last = cols.len().saturating_sub(1);
        for (i, p) in self.photos.iter().enumerate() {
            let c = match &p.place {
                Place::Idle => continue,
                Place::Queued => 0,
                Place::At(s) => s.saturating_add(1).min(last.saturating_sub(2)),
                Place::Done => last.saturating_sub(1),
                Place::Failed(_) => last,
            };
            if let Some(col) = cols.get_mut(c) {
                col.1.push(i);
            }
        }
        ui.horizontal(|ui| {
            ui.heading("Conveyor");
            if ui.button("Back to photos").clicked() && self.busy.is_none() {
                for p in &mut self.photos {
                    p.place = Place::Idle;
                }
            }
        });
        let w = ((ui.available_width() - 8.0 * cols.len() as f32) / cols.len() as f32).max(120.0);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for (title, items) in &cols {
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.strong(format!("{title} ({})", items.len()));
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt(title)
                            .max_height(ui.available_height())
                            .show(ui, |ui| {
                                for &i in items {
                                    let Some(p) = self.photos.get(i) else {
                                        continue;
                                    };
                                    egui::Frame::group(ui.style()).show(ui, |ui| {
                                        ui.set_width(w - 12.0);
                                        if let Some(t) = &p.thumb {
                                            ui.add(egui::Image::new(t).fit_to_exact_size(
                                                Vec2::new(w - 16.0, (w - 16.0) * 0.66),
                                            ));
                                        }
                                        ui.small(file_name(&p.path));
                                        if !p.skipped.is_empty() {
                                            ui.small(format!("skipped: {}", p.skipped.join(", ")));
                                        }
                                        if let Place::Failed(e) = &p.place {
                                            ui.colored_label(
                                                Color32::from_rgb(220, 80, 70),
                                                RichText::new(e).small(),
                                            );
                                        }
                                        if let Some(r) = &p.report
                                            && r.closed_eyes
                                        {
                                            ui.colored_label(
                                                Color32::from_rgb(230, 170, 60),
                                                "closed eyes: review",
                                            );
                                        }
                                    });
                                }
                            });
                    });
                }
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Setup => self.setup(ui),
            Tab::Conveyor => self.conveyor(ui),
        });
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

fn show_fit(ui: &mut egui::Ui, tex: &TextureHandle, width: f32, max_h: f32, caption: &str) {
    ui.vertical(|ui| {
        ui.strong(caption);
        let [w, h] = tex.size();
        let scale = width / (w.max(1) as f32);
        let scale = scale.min(max_h / (h.max(1) as f32));
        ui.add(
            egui::Image::new(tex).fit_to_exact_size(Vec2::new(w as f32 * scale, h as f32 * scale)),
        );
    });
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
