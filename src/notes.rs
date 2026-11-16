//! Notes on one photo: the user circles a part and says what to do there ("remove this sign",
//! "brighten her face"). The AI sees the photo with the circles drawn and turns each note into
//! steps for that photo only, placed by coordinates.

use crate::ai::{Client, Part, Turn};
use crate::analysis;
use crate::catalog::Catalog;
use crate::engine::{Extras, PhotoExtras};
use crate::recipe::Step;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// A circled region (normalized 0..1 from the top-left, a closed loop) and the request for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub points: Vec<[f32; 2]>,
    pub text: String,
}

/// Notes per photo path, as saved by the window or written by hand for `--notes`. Sorted,
/// so the saved file keeps a stable order.
pub type NotesFile = BTreeMap<PathBuf, Vec<Note>>;

/// Colours of note 1, 2, 3… on the photo the AI sees (and in the window).
pub const COLORS: [[u8; 3]; 6] = [
    [255, 40, 40],
    [0, 220, 255],
    [255, 220, 0],
    [255, 0, 220],
    [60, 255, 60],
    [255, 140, 0],
];

/// Long edge of the marked photo the AI sees: larger than the look, so small objects stay visible.
const NOTE_PREVIEW_SIZE: u32 = 1280;

/// Points of each loop written into the prompt (the drawing carries the exact shape).
const LOOP_POINTS: usize = 24;

pub const COLOR_NAMES: [&str; 6] = ["red", "cyan", "yellow", "magenta", "green", "orange"];

impl Note {
    /// Smallest box around the loop: `[x0, y0, x1, y1]`.
    pub fn bbox(&self) -> [f32; 4] {
        let mut b = [1.0f32, 1.0, 0.0, 0.0];
        for [x, y] in &self.points {
            b[0] = b[0].min(*x);
            b[1] = b[1].min(*y);
            b[2] = b[2].max(*x);
            b[3] = b[3].max(*y);
        }
        b
    }

    /// At most `n` evenly spaced points of the loop, rounded to 3 decimals for the prompt.
    pub fn simplified(&self, n: usize) -> Vec<[f32; 2]> {
        let len = self.points.len();
        if len == 0 {
            return Vec::new();
        }
        let step = len.div_ceil(n.max(1)).max(1);
        self.points
            .iter()
            .step_by(step)
            .map(|[x, y]| [(x * 1000.0).round() / 1000.0, (y * 1000.0).round() / 1000.0])
            .collect()
    }
}

pub fn load(path: &Path) -> Result<NotesFile> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("reading notes from {}", path.display()))
}

pub fn save(path: &Path, notes: &NotesFile) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(notes)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// Where the window keeps the notes of a folder (in the work folder, never beside the photos).
pub fn store_for(work: &Path, folder: &Path) -> PathBuf {
    let key = folder.to_string_lossy().to_lowercase();
    let h = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    work.join(".kimera")
        .join("notes")
        .join(format!("{h:016x}.json"))
}

/// Draws each note's loop in its colour on a JPEG copy of `preview`.
pub fn draw_overlay(preview: &Path, notes: &[Note], out: &Path) -> Result<()> {
    let mut img = image::open(preview)
        .with_context(|| format!("opening {}", preview.display()))?
        .to_rgb8();
    let (w, h) = (img.width() as f32, img.height() as f32);
    let thick = ((w.max(h) / 400.0).round() as i32).max(2);
    for (i, n) in notes.iter().enumerate() {
        let c = image::Rgb(COLORS[i % COLORS.len()]);
        let pts: Vec<(f32, f32)> = n.points.iter().map(|[x, y]| (x * w, y * h)).collect();
        for (a, b) in pts.iter().zip(pts.iter().cycle().skip(1)).take(pts.len()) {
            line(&mut img, *a, *b, thick, c);
        }
    }
    img.save_with_format(out, image::ImageFormat::Jpeg)
        .with_context(|| format!("writing {}", out.display()))
}

fn line(img: &mut image::RgbImage, a: (f32, f32), b: (f32, f32), thick: i32, c: image::Rgb<u8>) {
    let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as i32).max(1);
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        for dy in -thick / 2..=thick / 2 {
            for dx in -thick / 2..=thick / 2 {
                let (px, py) = (x as i32 + dx, y as i32 + dy);
                if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                    img.put_pixel(px as u32, py as u32, c);
                }
            }
        }
    }
}

/// The AI's answer for one photo: steps per app.
#[derive(Debug, Default, Deserialize)]
struct Fragment {
    #[serde(default)]
    lightcraft: Section,
    #[serde(default)]
    photocraft: Section,
}

#[derive(Debug, Default, Deserialize)]
struct Section {
    #[serde(default)]
    steps: Vec<Step>,
}

const ASK: &str = r#"# Notes on one photo

The user circled parts of THIS photo (attached, loops drawn in colour) and wrote what to do in
each. Turn the notes into steps for this photo only. Answer with at most 3 short lines for the
user, in their language, then ONE fenced `toml` block in exactly this shape (leave out an app
you do not need):

```toml
[lightcraft]
steps = [ { cmd = "spot.add", params = { mode = "fill", points = [[0.41, 0.62], [0.45, 0.60]], size = 0.04 }, optional = "note-1" } ]

[photocraft]
steps = [ ]
```

How to place things:
- Prefer LightCraft. Its coordinates are normalized [x, y], 0..1 from the top-left of the photo.
  - Remove or clean something: `spot.add` with `mode` "fill" (content-aware) or "heal",
    `points` tracing the circled object, `size` (fraction of the long edge) about its width.
  - Change one thing (brighter, warmer, sharper, blurred…): `mask.add` with kind "object" and
    `points` inside it (needs SAM 3), or "radial" (`center`, `rx`, `ry`, `feather`) around the
    loop, or "brush" followed by `mask.brushStroke` with the loop points; then `mask.adjust`.
  - Red eyes: `redeye.add` with `center`, `rx`, `ry`.
- PhotoCraft only for pixel work LightCraft cannot do (text, placing an image, liquify);
  its coordinates are pixels of the full-size photo, whose size is given below.
- Give every step of note N `optional = "note-N"`, so a step that fails leaves the rest of the
  photo's edit intact.
- Do not edit anything outside the circled regions, and never invent text or file paths.
"#;

/// What turning notes into steps needs: the AI, the apps' command lists, LightCraft for
/// previews, a scratch folder, and the run's cancel flag.
#[derive(Clone, Copy)]
pub struct NotesJob<'a> {
    pub client: &'a Client,
    pub catalogs: &'a [Catalog],
    pub lightcraft: &'a Path,
    pub scratch: &'a Path,
    pub cancel: &'a AtomicBool,
}

impl NotesJob<'_> {
    /// Steps for every photo that has notes (keyed by index in `files`), merged into
    /// `extras`. A photo whose notes cannot be turned into steps is reported and runs without
    /// them.
    ///
    /// # Errors
    ///
    /// Only when the run is cancelled.
    pub fn plan_all(
        &self,
        files: &[PathBuf],
        notes: &HashMap<usize, Vec<Note>>,
        extras: &mut Extras,
        progress: &mut dyn FnMut(String),
    ) -> Result<()> {
        let system = crate::writer::system_prompt(self.catalogs);
        let mut ids: Vec<&usize> = notes.keys().collect();
        ids.sort_unstable();
        for &i in ids {
            let (Some(photo), Some(n)) = (files.get(i), notes.get(&i)) else {
                continue;
            };
            if n.is_empty() {
                continue;
            }
            let name = photo
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            progress(format!("Reading your {} note(s) on {name}…", n.len()));
            match self.plan_photo(&system, photo, n) {
                Ok(x) => {
                    progress(format!(
                        "{name}: {} LightCraft / {} PhotoCraft step(s) from the notes",
                        x.lightcraft.len(),
                        x.photocraft.len()
                    ));
                    extras.entry(i).or_default().merge(x);
                }
                Err(e) if crate::procrun::is_cancelled(&e) => return Err(e),
                Err(e) => progress(format!("{name}: notes not applied ({e:#})")),
            }
        }
        Ok(())
    }

    /// Asks the AI for the steps of one photo's notes; `system` is the recipe writer's system
    /// prompt (cached by the provider).
    ///
    /// # Errors
    ///
    /// When the preview cannot be made, the AI cannot be reached, or three answers in a row do
    /// not check out.
    pub fn plan_photo(&self, system: &str, photo: &Path, notes: &[Note]) -> Result<PhotoExtras> {
        plan_photo(self, system, photo, notes)
    }
}

fn plan_photo(
    job: &NotesJob<'_>,
    system: &str,
    photo: &Path,
    notes: &[Note],
) -> Result<PhotoExtras> {
    let NotesJob {
        client,
        catalogs,
        lightcraft,
        scratch,
        cancel,
    } = *job;
    if notes.is_empty() {
        return Ok(PhotoExtras::default());
    }
    std::fs::create_dir_all(scratch)?;
    let stem = photo
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prev = scratch.join(format!("note-{stem}-{}.jpg", std::process::id()));
    let over = scratch.join(format!("note-{stem}-{}-marked.jpg", std::process::id()));
    analysis::preview(lightcraft, photo, &prev, NOTE_PREVIEW_SIZE, cancel)?;
    draw_overlay(&prev, notes, &over)?;
    let size = image::image_dimensions(photo)
        .map(|(w, h)| format!("{w}×{h} px"))
        .unwrap_or_else(|_| "unknown (raw file: use LightCraft only)".into());
    let mut text = format!("{ASK}\nFull-size photo: {size}.\n\nNotes:\n");
    for (i, n) in notes.iter().enumerate() {
        let b = n.bbox();
        let _ = writeln!(
            text,
            "- note-{} ({} loop): \"{}\" — box [{:.3}, {:.3}, {:.3}, {:.3}], loop {:?}",
            i + 1,
            COLOR_NAMES[i % COLOR_NAMES.len()],
            n.text.trim(),
            b[0],
            b[1],
            b[2],
            b[3],
            n.simplified(LOOP_POINTS)
        );
    }
    let mut turns = vec![Turn::user(vec![Part::Jpeg(over.clone()), Part::Text(text)])];
    let mut last = String::new();
    let mut result = Err(anyhow::anyhow!("no answer"));
    for _ in 0..3 {
        let reply = client.ask(system, &turns, cancel)?;
        match parse_fragment(&reply, catalogs) {
            Ok(x) => {
                result = Ok(x);
                break;
            }
            Err(e) => {
                last = format!("{e:#}");
                turns.push(Turn::assistant(reply));
                turns.push(Turn::user(vec![Part::Text(format!(
                    "Kimera refused that: {last}. Answer again in the same shape."
                ))]));
            }
        }
    }
    let _ = std::fs::remove_file(&prev);
    let _ = std::fs::remove_file(&over);
    result.map_err(|_| anyhow::anyhow!("the AI could not turn the notes into steps: {last}"))
}

fn parse_fragment(reply: &str, catalogs: &[Catalog]) -> Result<PhotoExtras> {
    let (_, toml) = crate::writer::split(reply);
    let toml = toml.context("the answer has no ```toml block")?;
    let f: Fragment = toml::from_str(&toml)?;
    let mut out = PhotoExtras {
        lightcraft: f.lightcraft.steps,
        photocraft: f.photocraft.steps,
    };
    for (app, steps) in [
        (crate::apps::App::LightCraft, &mut out.lightcraft),
        (crate::apps::App::PhotoCraft, &mut out.photocraft),
    ] {
        for s in steps.iter_mut() {
            crate::recipe::check_step(s).map_err(|e| anyhow::anyhow!("{app}: {e}"))?;
            if let Some(c) = catalogs.iter().find(|c| c.app == app)
                && !c.has(&s.cmd)
            {
                bail!("{app} has no command {}", s.cmd);
            }
            // A note never stops the photo: every step is in some optional group.
            if s.optional.is_none() {
                s.optional = Some("notes".into());
            }
        }
    }
    if out.lightcraft.is_empty() && out.photocraft.is_empty() {
        bail!("no steps for the notes");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(app: crate::apps::App, ids: &[&str]) -> Catalog {
        Catalog {
            app,
            version: "1".into(),
            commands: ids
                .iter()
                .map(|i| crate::catalog::Cmd {
                    id: (*i).into(),
                    label: String::new(),
                    params: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn fragment_steps_become_optional_and_checked() -> Result<()> {
        let cats = [cat(crate::apps::App::LightCraft, &["spot.add", "mask.add"])];
        let reply = "Vou remover a placa.\n```toml\n[lightcraft]\nsteps = [{ cmd = \"spot.add\", params = { mode = \"fill\", points = [[0.4, 0.6]] } }]\n```";
        let x = parse_fragment(reply, &cats)?;
        assert_eq!(x.lightcraft.len(), 1);
        assert_eq!(x.lightcraft[0].optional.as_deref(), Some("notes"));
        let bad = "```toml\n[lightcraft]\nsteps = [{ cmd = \"magic.wand\" }]\n```";
        assert!(parse_fragment(bad, &cats).is_err());
        assert!(parse_fragment("```toml\n```", &cats).is_err());
        Ok(())
    }

    #[test]
    fn bbox_and_simplify() {
        let n = Note {
            points: (0..100).map(|i| [i as f32 / 100.0, 0.5]).collect(),
            text: "x".into(),
        };
        assert_eq!(n.bbox(), [0.0, 0.5, 0.99, 0.5]);
        assert!(n.simplified(24).len() <= 25);
    }

    #[test]
    fn overlay_is_drawn() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("kimera-notes-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let src = dir.join("a.jpg");
        image::RgbImage::from_pixel(100, 80, image::Rgb([0, 0, 0])).save(&src)?;
        let out = dir.join("b.jpg");
        let n = Note {
            points: vec![[0.1, 0.1], [0.9, 0.1], [0.5, 0.9]],
            text: "x".into(),
        };
        draw_overlay(&src, &[n], &out)?;
        let img = image::open(&out)?.to_rgb8();
        let red = img
            .pixels()
            .filter(|p| p.0[0] > 150 && p.0[1] < 100)
            .count();
        std::fs::remove_dir_all(&dir)?;
        assert!(red > 50, "{red} red pixels");
        Ok(())
    }

    #[test]
    fn store_is_in_the_work_folder() {
        let p = store_for(Path::new("W"), Path::new("D:/Shoot"));
        assert!(p.starts_with(Path::new("W").join(".kimera").join("notes")));
        assert_eq!(p, store_for(Path::new("W"), Path::new("d:/shoot")));
    }
}
