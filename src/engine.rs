//! The conveyor: every photo goes through the recipe's stages in order, one Craft app at a time,
//! each stage writing into its own folder of the run. Originals are only read. A photo that fails
//! stops there and the others go on. Progress is reported as [`Event`]s, so the CLI prints them
//! and a window can draw them.

use crate::apps::{lightcraft, photocraft};
use crate::recipe::{Recipe, Stage};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Files a folder contributes (its top level only; run folders inside it are skipped).
pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "avif", "jxl", "heic", "heif", "psd", "dng",
    "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "pef", "srw",
];

pub struct Apps {
    pub photocraft: PathBuf,
    pub lightcraft: PathBuf,
}

#[derive(Debug, Clone)]
pub enum Event {
    StageStarted {
        photo: usize,
        stage: usize,
    },
    StageDone {
        photo: usize,
        stage: usize,
        out: PathBuf,
        ms: u128,
    },
    PhotoFailed {
        photo: usize,
        stage: usize,
        error: String,
    },
    PhotoDone {
        photo: usize,
        out: PathBuf,
    },
}

#[derive(Debug, Default)]
pub struct Summary {
    /// Final file of every photo that went through all stages.
    pub done: Vec<PathBuf>,
    pub failed: usize,
    pub cancelled: bool,
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

pub fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// One output name per photo: its stem, with `-2`, `-3`… when two photos share one
/// (`a.jpg` and `a.png`), compared without case as Windows does.
fn out_names(photos: &[PathBuf]) -> Vec<String> {
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

/// `<run_dir>/<n>-<app>/`, one per stage.
pub fn stage_dirs(recipe: &Recipe, run_dir: &Path) -> Vec<PathBuf> {
    recipe
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| run_dir.join(format!("{}-{}", i.saturating_add(1), s.app())))
        .collect()
}

/// Runs every photo through every stage. `cancel` is checked before each stage.
pub fn run(
    apps: &Apps,
    recipe: &Recipe,
    photos: &[PathBuf],
    run_dir: &Path,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(Event),
) -> Result<Summary> {
    if photos.is_empty() {
        bail!("no photos to process");
    }
    let dirs = stage_dirs(recipe, run_dir);
    for d in &dirs {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let names = out_names(photos);
    let mut sum = Summary::default();
    'photos: for (photo, (input, name)) in photos.iter().zip(&names).enumerate() {
        let mut cur = input.clone();
        for (stage, (st, dir)) in recipe.stages.iter().zip(&dirs).enumerate() {
            if cancel.load(Ordering::Relaxed) {
                sum.cancelled = true;
                break 'photos;
            }
            on(Event::StageStarted { photo, stage });
            let out = dir.join(format!("{name}.{}", st.out_ext()));
            let t = Instant::now();
            let r = match st {
                Stage::Photocraft(s) => photocraft::run(&apps.photocraft, &cur, &out, s),
                Stage::Lightcraft(s) => lightcraft::run_file(&apps.lightcraft, &cur, &out, s),
            };
            match r {
                Ok(()) => {
                    on(Event::StageDone {
                        photo,
                        stage,
                        out: out.clone(),
                        ms: t.elapsed().as_millis(),
                    });
                    cur = out;
                }
                Err(e) => {
                    on(Event::PhotoFailed {
                        photo,
                        stage,
                        error: format!("{e:#}"),
                    });
                    sum.failed = sum.failed.saturating_add(1);
                    continue 'photos;
                }
            }
        }
        on(Event::PhotoDone {
            photo,
            out: cur.clone(),
        });
        sum.done.push(cur);
    }
    Ok(sum)
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
    fn stage_folders_are_numbered_in_order() -> Result<()> {
        let r = crate::recipe::parse(include_str!("../recipes/cleanup-cinematic.toml"))?;
        let d = stage_dirs(&r, Path::new("run"));
        assert_eq!(
            d,
            [
                Path::new("run").join("1-photocraft"),
                Path::new("run").join("2-lightcraft")
            ]
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
    fn missing_path_is_an_error() {
        assert!(collect(&[PathBuf::from("does/not/exist")]).is_err());
    }
}
