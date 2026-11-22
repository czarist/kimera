//! The conveyor: every photo goes through the recipe's stages in order, one Craft app at a time,
//! each stage writing into its own folder of the run. Originals are only read. A photo that fails
//! stops there and the others go on. Progress is reported as [`Event`]s, so the CLI prints them
//! and the window draws them.
//!
//! Stages are laid out as *slots* ([`plan`]): the shared stages first, then each variant's own
//! stages, which start from the shared result.

use crate::apps::{App, StageFailed, lightcraft, photocraft, vectorcraft};
use crate::procrun;
use crate::recipe::{Recipe, Stage, Step};
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Files a folder contributes (its top level only; run folders inside it are skipped).
pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "avif", "jxl", "heic", "heif", "psd", "dng",
    "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "pef", "srw",
];

/// Raw camera files: LightCraft develops them, PhotoCraft cannot open them.
pub const RAW_EXTS: &[&str] = &[
    "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "pef", "srw",
];

#[derive(Debug, Clone, Default)]
pub struct Apps {
    pub photocraft: PathBuf,
    pub lightcraft: PathBuf,
    pub vectorcraft: Option<PathBuf>,
}

/// One stage position in a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    /// `None` for shared stages.
    pub variant: Option<String>,
    pub app: App,
    /// Folder name inside the run: `1-photocraft`, `3-stories-lightcraft`.
    pub dir: String,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// The photo's turn has come: its per-photo preparation (AI look, finishing) starts.
    PhotoStarted {
        photo: usize,
    },
    StageStarted {
        photo: usize,
        slot: usize,
    },
    StageDone {
        photo: usize,
        slot: usize,
        out: PathBuf,
        ms: u128,
    },
    /// An optional step group failed and was left out for this photo.
    StepsSkipped {
        photo: usize,
        slot: usize,
        group: String,
        reason: String,
    },
    PhotoFailed {
        photo: usize,
        slot: usize,
        error: String,
    },
    /// Every chain of the photo ended; `outs` holds the finished files (one per variant).
    PhotoDone {
        photo: usize,
        outs: Vec<PathBuf>,
    },
}

#[derive(Debug, Default)]
pub struct Summary {
    /// Every finished file.
    pub done: Vec<PathBuf>,
    /// Photos with at least one failed chain.
    pub failed: usize,
    pub skipped_groups: usize,
    pub cancelled: bool,
}

/// Extra steps for one photo (the AI's look, the user's notes), run at the start of the first
/// stage of that app in each chain, and the per-photo finish ([`crate::look`]) run at the end of
/// the first LightCraft stage, over the recipe's own look. They should be in optional groups.
#[derive(Debug, Clone, Default)]
pub struct PhotoExtras {
    pub lightcraft: Vec<Step>,
    pub photocraft: Vec<Step>,
    pub lightcraft_finish: Vec<Step>,
}

impl PhotoExtras {
    pub fn merge(&mut self, other: PhotoExtras) {
        self.lightcraft.extend(other.lightcraft);
        self.photocraft.extend(other.photocraft);
        self.lightcraft_finish.extend(other.lightcraft_finish);
    }

    pub fn is_empty(&self) -> bool {
        self.lightcraft.is_empty()
            && self.photocraft.is_empty()
            && self.lightcraft_finish.is_empty()
    }
}

/// Photo index → its extra steps.
pub type Extras = HashMap<usize, PhotoExtras>;

/// Adds the stages per-photo extras need when the recipe has none of that app: a LightCraft
/// stage (16-bit TIFF) or a PhotoCraft stage (TIFF) at the start, LightCraft first when raw
/// photos need developing before PhotoCraft. Returns what was added, for the log.
pub fn ensure_apps(recipe: &mut Recipe, extras: &Extras, photos: &[PathBuf]) -> Vec<App> {
    let lc = extras
        .values()
        .any(|x| !x.lightcraft.is_empty() || !x.lightcraft_finish.is_empty());
    let pc = extras.values().any(|x| !x.photocraft.is_empty());
    ensure_stages(recipe, lc, pc, photos)
}

/// [`ensure_apps`] for steps known only later (made just before each photo's turn): adds a
/// LightCraft stage when `lightcraft` steps will come, a PhotoCraft one for `photocraft`.
pub fn ensure_stages(
    recipe: &mut Recipe,
    lightcraft: bool,
    photocraft: bool,
    photos: &[PathBuf],
) -> Vec<App> {
    use crate::recipe::{FileExport, LightCraftStage, PhotoCraftStage};
    let all: Vec<&Stage> = slot_stages(recipe);
    let has_lc = all.iter().any(|s| matches!(s, Stage::Lightcraft(_)));
    let has_pc = all.iter().any(|s| matches!(s, Stage::Photocraft(_)));
    let need_lc = lightcraft && !has_lc;
    let need_pc = photocraft && !has_pc;
    let lc = || {
        Stage::Lightcraft(LightCraftStage {
            steps: Vec::new(),
            export: FileExport {
                format: "tif".into(),
                bit_depth: Some(16),
                ..FileExport::default()
            },
        })
    };
    let mut added = Vec::new();
    let mut front: Vec<Stage> = Vec::new();
    if need_lc || (need_pc && photos.iter().any(|p| is_raw(p)) && !has_lc) {
        front.push(lc());
        added.push(App::LightCraft);
    }
    if need_pc {
        front.push(Stage::Photocraft(PhotoCraftStage {
            out_ext: "tif".into(),
            steps: Vec::new(),
        }));
        added.push(App::PhotoCraft);
    }
    if !front.is_empty() {
        front.append(&mut recipe.stages);
        recipe.stages = front;
    }
    added
}

/// Image files from `paths`: files as given, folders expanded (sorted), duplicates dropped.
pub fn collect(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for p in paths {
        if p.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(p)
                .with_context(|| format!("reading {}", p.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.is_file() && is_image(f))
                .collect();
            files.sort();
            for f in files {
                if seen.insert(f.clone()) {
                    out.push(f);
                }
            }
        } else if p.is_file() {
            if seen.insert(p.clone()) {
                out.push(p.clone());
            }
        } else {
            bail!("not found: {}", p.display());
        }
    }
    Ok(out)
}

fn ext_in(p: &Path, list: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| list.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub fn is_image(p: &Path) -> bool {
    ext_in(p, IMAGE_EXTS)
}

pub fn is_raw(p: &Path) -> bool {
    ext_in(p, RAW_EXTS)
}

/// One output name per photo: its stem, with `-2`, `-3`… when two photos share one
/// (`a.jpg` and `a.png`), compared without case as Windows does.
pub fn out_names(photos: &[PathBuf]) -> Vec<String> {
    let mut used = HashSet::new();
    photos
        .iter()
        .map(|p| {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "photo".into());
            let mut name = stem.clone();
            let mut n = 1u32;
            while !used.insert(name.to_lowercase()) {
                n = n.saturating_add(1);
                name = format!("{stem}-{n}");
            }
            name
        })
        .collect()
}

/// The slots of a recipe, in run order.
pub fn plan(recipe: &Recipe) -> Vec<Slot> {
    let mut slots: Vec<Slot> = recipe
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| Slot {
            variant: None,
            app: s.app(),
            dir: format!("{}-{}", i.saturating_add(1), s.app()),
        })
        .collect();
    let base = recipe.stages.len();
    for v in &recipe.variants {
        for (k, s) in v.stages.iter().enumerate() {
            slots.push(Slot {
                variant: Some(v.name.clone()),
                app: s.app(),
                dir: format!(
                    "{}-{}-{}",
                    base.saturating_add(k).saturating_add(1),
                    v.name,
                    s.app()
                ),
            });
        }
    }
    slots
}

/// Stage of each slot, aligned with [`plan`].
fn slot_stages(recipe: &Recipe) -> Vec<&Stage> {
    recipe
        .stages
        .iter()
        .chain(recipe.variants.iter().flat_map(|v| &v.stages))
        .collect()
}

/// Chains as lists of slot indexes: the shared prefix, then each variant's slots.
fn chains(recipe: &Recipe) -> (Vec<usize>, Vec<Vec<usize>>) {
    let shared: Vec<usize> = (0..recipe.stages.len()).collect();
    let mut next = recipe.stages.len();
    let mut variants = Vec::new();
    for v in &recipe.variants {
        let own: Vec<usize> = (next..next.saturating_add(v.stages.len())).collect();
        next = next.saturating_add(v.stages.len());
        variants.push(own);
    }
    (shared, variants)
}

/// Slots that are the first stage of their app in some chain (where per-photo extras go).
fn first_slots(recipe: &Recipe) -> HashSet<usize> {
    let stages = slot_stages(recipe);
    let (shared, variants) = chains(recipe);
    let mut out = HashSet::new();
    for is_app in [
        (|s: &Stage| matches!(s, Stage::Lightcraft(_))) as fn(&Stage) -> bool,
        |s: &Stage| matches!(s, Stage::Photocraft(_)),
    ] {
        let first_in = |ids: &[usize]| {
            ids.iter()
                .copied()
                .find(|i| stages.get(*i).is_some_and(|s| is_app(s)))
        };
        if let Some(i) = first_in(&shared) {
            out.insert(i);
        } else {
            out.extend(variants.iter().filter_map(|v| first_in(v)));
        }
    }
    out
}

struct Ctx<'a> {
    apps: &'a Apps,
    stages: Vec<&'a Stage>,
    dirs: Vec<PathBuf>,
    extras_at: HashSet<usize>,
    cancel: &'a AtomicBool,
}

/// Runs every photo through every chain. `cancel` stops the running app at once.
pub fn run(
    apps: &Apps,
    recipe: &Recipe,
    photos: &[PathBuf],
    run_dir: &Path,
    extras: &Extras,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(Event),
) -> Result<Summary> {
    run_prepared(
        apps,
        recipe,
        photos,
        run_dir,
        &mut |i| extras.get(&i).cloned().unwrap_or_default(),
        cancel,
        on,
    )
}

/// [`run`], with each photo's extras made just before its turn by `prepare` (so per-photo AI
/// work and the conveyor alternate, and the first result comes early). `prepare` reports its
/// own problems and returns what it could make.
pub fn run_prepared(
    apps: &Apps,
    recipe: &Recipe,
    photos: &[PathBuf],
    run_dir: &Path,
    prepare: &mut dyn FnMut(usize) -> PhotoExtras,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(Event),
) -> Result<Summary> {
    if photos.is_empty() {
        bail!("no photos to process");
    }
    let slots = plan(recipe);
    let dirs: Vec<PathBuf> = slots.iter().map(|s| run_dir.join(&s.dir)).collect();
    for d in &dirs {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let cx = Ctx {
        apps,
        stages: slot_stages(recipe),
        dirs,
        extras_at: first_slots(recipe),
        cancel,
    };
    let (shared, variants) = chains(recipe);
    let names = out_names(photos);
    let mut sum = Summary::default();
    for (photo, (input, name)) in photos.iter().zip(&names).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            sum.cancelled = true;
            break;
        }
        on(Event::PhotoStarted { photo });
        let extra = prepare(photo);
        if cancel.load(Ordering::Relaxed) {
            sum.cancelled = true;
            break;
        }
        let mut failed = false;
        let mut outs = Vec::new();
        let px = PhotoRun {
            photo,
            name,
            extra: &extra,
        };
        match run_chain(&cx, &px, input, &shared, &mut sum, on) {
            Ok(base) => {
                if variants.is_empty() {
                    outs.push(base);
                } else {
                    for v in &variants {
                        match run_chain(&cx, &px, &base, v, &mut sum, on) {
                            Ok(out) => outs.push(out),
                            Err(Stop::Failed) => failed = true,
                            Err(Stop::Cancelled) => {
                                sum.cancelled = true;
                                break;
                            }
                        }
                    }
                }
            }
            Err(Stop::Failed) => failed = true,
            Err(Stop::Cancelled) => sum.cancelled = true,
        }
        if failed {
            sum.failed = sum.failed.saturating_add(1);
        }
        if !outs.is_empty() {
            sum.done.extend(outs.iter().cloned());
            on(Event::PhotoDone { photo, outs });
        }
        if sum.cancelled {
            break;
        }
    }
    Ok(sum)
}

enum Stop {
    Failed,
    Cancelled,
}

/// The photo a chain runs for.
struct PhotoRun<'a> {
    photo: usize,
    /// Output file stem.
    name: &'a str,
    extra: &'a PhotoExtras,
}

fn run_chain(
    cx: &Ctx,
    px: &PhotoRun<'_>,
    input: &Path,
    slots: &[usize],
    sum: &mut Summary,
    on: &mut dyn FnMut(Event),
) -> std::result::Result<PathBuf, Stop> {
    let PhotoRun { photo, name, extra } = *px;
    let mut cur = input.to_path_buf();
    for &slot in slots {
        let (Some(stage), Some(dir)) = (cx.stages.get(slot), cx.dirs.get(slot)) else {
            continue;
        };
        if cx.cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }
        on(Event::StageStarted { photo, slot });
        let out = dir.join(format!("{name}.{}", stage.out_ext()));
        let t = Instant::now();
        let job = StageJob {
            photo,
            slot,
            extra,
            stage,
            input: &cur,
            out: &out,
        };
        match run_stage(cx, &job, sum, on) {
            Ok(()) => {
                on(Event::StageDone {
                    photo,
                    slot,
                    out: out.clone(),
                    ms: t.elapsed().as_millis(),
                });
                cur = out;
            }
            Err(e) if procrun::is_cancelled(&e) => return Err(Stop::Cancelled),
            Err(e) => {
                on(Event::PhotoFailed {
                    photo,
                    slot,
                    error: format!("{e:#}"),
                });
                return Err(Stop::Failed);
            }
        }
    }
    Ok(cur)
}

/// One photo in one slot: what [`run_stage`] works on.
struct StageJob<'a> {
    photo: usize,
    slot: usize,
    extra: &'a PhotoExtras,
    stage: &'a Stage,
    input: &'a Path,
    out: &'a Path,
}

/// Runs one stage, leaving out optional groups that fail, one by one.
fn run_stage(
    cx: &Ctx,
    job: &StageJob<'_>,
    sum: &mut Summary,
    on: &mut dyn FnMut(Event),
) -> Result<()> {
    let StageJob {
        photo,
        slot,
        extra: x,
        stage,
        input,
        out,
    } = *job;
    let (before, after): (&[Step], &[Step]) = if cx.extras_at.contains(&slot) {
        match stage {
            Stage::Lightcraft(_) => (&x.lightcraft, &x.lightcraft_finish),
            Stage::Photocraft(_) => (&x.photocraft, &[]),
            Stage::Vectorcraft(_) => (&[], &[]),
        }
    } else {
        (&[], &[])
    };
    let mut skip: HashSet<String> = HashSet::new();
    loop {
        let steps: Vec<&Step> = before
            .iter()
            .chain(stage.steps())
            .chain(after)
            .filter(|s| s.optional.as_ref().is_none_or(|g| !skip.contains(g)))
            .collect();
        let r = match stage {
            Stage::Photocraft(_) if is_raw(input) => Err(anyhow::anyhow!(
                "PhotoCraft cannot open raw files like {}: put a LightCraft stage first",
                input.display()
            )),
            Stage::Photocraft(_) => {
                photocraft::run(&cx.apps.photocraft, input, out, &steps, cx.cancel)
            }
            Stage::Lightcraft(s) => lightcraft::run_file(
                &cx.apps.lightcraft,
                input,
                out,
                &steps,
                &s.export,
                cx.cancel,
            ),
            Stage::Vectorcraft(s) => match &cx.apps.vectorcraft {
                Some(cli) => {
                    vectorcraft::run(cli, input, s.template.as_deref(), out, &steps, cx.cancel)
                }
                None => Err(anyhow::anyhow!(
                    "a VectorCraft stage needs [vectorcraft] cli in kimera.toml"
                )),
            },
        };
        let Err(e) = r else { return Ok(()) };
        let group = e
            .downcast_ref::<StageFailed>()
            .and_then(|f| f.step)
            .and_then(|i| steps.get(i))
            .and_then(|s| s.optional.clone());
        match group {
            Some(g) if skip.insert(g.clone()) => {
                sum.skipped_groups = sum.skipped_groups.saturating_add(1);
                on(Event::StepsSkipped {
                    photo,
                    slot,
                    group: g,
                    reason: format!("{e:#}"),
                });
            }
            _ => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_stems_get_distinct_names() {
        let p: Vec<PathBuf> = ["x/a.jpg", "y/A.png", "a.tif", "b.jpg"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(out_names(&p), ["a", "A-2", "a-3", "b"]);
    }

    #[test]
    fn plan_numbers_shared_then_variant_stages() -> Result<()> {
        let r = crate::recipe::parse(
            "[[stage]]\napp = \"photocraft\"\n[[variant]]\nname = \"feed\"\n[[variant.stage]]\napp = \"lightcraft\"\n[[variant]]\nname = \"bw\"\n[[variant.stage]]\napp = \"lightcraft\"\n",
        )?;
        let dirs: Vec<String> = plan(&r).into_iter().map(|s| s.dir).collect();
        assert_eq!(
            dirs,
            ["1-photocraft", "2-feed-lightcraft", "2-bw-lightcraft"]
        );
        let (shared, variants) = chains(&r);
        assert_eq!(shared, [0]);
        assert_eq!(variants, [vec![1], vec![2]]);
        let mut first: Vec<usize> = first_slots(&r).into_iter().collect();
        first.sort();
        assert_eq!(first, [0, 1, 2]); // PhotoCraft in the shared part, LightCraft in each variant
        Ok(())
    }

    #[test]
    fn bundled_recipe_plan() -> Result<()> {
        let r = crate::recipe::parse(include_str!("../recipes/cleanup-cinematic.toml"))?;
        let dirs: Vec<String> = plan(&r).into_iter().map(|s| s.dir).collect();
        assert_eq!(dirs, ["1-lightcraft"]);
        assert_eq!(first_slots(&r).into_iter().collect::<Vec<_>>(), [0]);
        Ok(())
    }

    #[test]
    fn missing_apps_are_added_only_when_extras_need_them() -> Result<()> {
        let lc_only = "[[stage]]\napp = \"lightcraft\"\n";
        let step = || Step {
            cmd: "x.y".into(),
            params: None,
            optional: Some("n".into()),
        };
        let jpg = [PathBuf::from("a.jpg")];

        // LightCraft extras on a LightCraft recipe: nothing to add.
        let mut r = crate::recipe::parse(lc_only)?;
        let ex: Extras = [(
            0,
            PhotoExtras {
                lightcraft: vec![step()],
                photocraft: vec![],
                lightcraft_finish: vec![],
            },
        )]
        .into();
        assert!(ensure_apps(&mut r, &ex, &jpg).is_empty());

        // PhotoCraft extras: a PhotoCraft stage goes first, the recipe's output stays the same.
        let ex: Extras = [(
            0,
            PhotoExtras {
                lightcraft: vec![],
                photocraft: vec![step()],
                lightcraft_finish: vec![],
            },
        )]
        .into();
        assert_eq!(ensure_apps(&mut r, &ex, &jpg), [App::PhotoCraft]);
        let dirs: Vec<String> = plan(&r).into_iter().map(|s| s.dir).collect();
        assert_eq!(dirs, ["1-photocraft", "2-lightcraft"]);
        assert_eq!(first_slots(&r).len(), 2);

        // Raw photo + PhotoCraft extras on a recipe without LightCraft: develop first.
        let mut r = crate::recipe::parse("[[stage]]\napp = \"vectorcraft\"\n")?;
        assert_eq!(
            ensure_apps(&mut r, &ex, &[PathBuf::from("a.cr3")]),
            [App::LightCraft, App::PhotoCraft]
        );
        Ok(())
    }

    #[test]
    fn folders_give_their_images_only() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("kimera-collect-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("run-1"))?;
        for f in ["b.JPG", "a.tif", "notes.txt"] {
            std::fs::write(dir.join(f), b"x")?;
        }
        let got = collect(&[dir.clone(), dir.join("a.tif")]);
        std::fs::remove_dir_all(&dir)?;
        assert_eq!(got?, [dir.join("a.tif"), dir.join("b.JPG")]);
        Ok(())
    }

    #[test]
    fn raw_files_are_recognised() {
        assert!(is_raw(Path::new("a.CR3")));
        assert!(!is_raw(Path::new("a.jpg")));
    }

    #[test]
    fn missing_path_is_an_error() {
        assert!(collect(&[PathBuf::from("does/not/exist")]).is_err());
    }
}
