//! The per-photo finish: the AI looks at each photo as the recipe renders it, with measurements
//! Kimera takes itself (brightness, black and white points, contrast, saturation, colour cast),
//! and sets LightCraft's develop sliders for that photo; Kimera renders the result, measures it
//! again, and the AI corrects its own work. With reference photos (the photographer's finished
//! work) the target is their look, shown and measured the same way; without them, a polished,
//! natural result true to the request, kept consistent along the series.
//!
//! Skin is measured apart (the lit skin-toned pixels), so a reference's backdrop never sets the
//! target for a face. Out of this come the develop sliders (`develop.set`, clamped to the ranges
//! LightCraft reports) and up to three masks that need no model: the skin (a colour range sampled
//! on the face), a light on the face and the background (outside an ellipse around the subject).
//! All of it is in the optional group `finish`, run after the recipe's own steps.

use crate::ai::{Client, Part, Turn};
use crate::portrait::{self, Area, Scan};
use crate::procrun;
use crate::recipe::{Recipe, Stage, Step};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

/// Long edge of the renders the AI judges: tone and colour read well at this size, and the
/// render stays quick.
const LOOK_SIZE: u32 = 768;

/// Long edge of the reference pictures sent along.
const REFERENCE_SIZE: u32 = 640;

/// Reference pictures sent with each question (each one costs input tokens on every call).
pub const MAX_REFERENCE_IMAGES: usize = 3;

/// References measured for the target numbers (rendering them is cheap and cached).
pub const MAX_REFERENCES: usize = 12;

/// Pixels sampled per measurement: plenty for percentiles, fast on a big picture.
const SAMPLE_PIXELS: u64 = 300_000;

/// The optional group of the finishing steps.
pub const GROUP: &str = "finish";

/// How many times the AI sets the sliders: once, or once more after seeing its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Passes {
    Off,
    One,
    #[default]
    Two,
}

impl Passes {
    pub const ALL: [Passes; 3] = [Passes::Off, Passes::One, Passes::Two];

    pub fn count(self) -> usize {
        match self {
            Passes::Off => 0,
            Passes::One => 1,
            Passes::Two => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Passes::Off => "Off",
            Passes::One => "Quick (1 pass)",
            Passes::Two => "Careful (2 passes)",
        }
    }
}

// ---------- measurements ----------

/// What Kimera measures on a render (CIELAB, D65; L in 0..100).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    pub mean_l: f64,
    pub p02: f64,
    pub p50: f64,
    pub p98: f64,
    /// Standard deviation of L: global contrast.
    pub contrast: f64,
    /// Mean chroma (√(a²+b²)).
    pub chroma: f64,
    /// Mean a* of the midtones: green (−) to magenta (+).
    pub tint: f64,
    /// Mean b* of the midtones: blue (−) to yellow (+).
    pub warmth: f64,
    /// Share of pixels at pure black / pure white, in %.
    pub clip_low: f64,
    pub clip_high: f64,
    /// The skin-toned pixels, measured apart: the part of a portrait whose look carries over
    /// from one photo to another, whatever the backdrop.
    pub skin: Skin,
}

/// Skin tones of a picture (CIELAB means); `share` is 0 when none were found.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Skin {
    /// Share of the picture, in %.
    pub share: f64,
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

impl Skin {
    /// Below this share of the picture the skin measurement is noise.
    const MIN_SHARE: f64 = 0.5;

    pub fn found(&self) -> bool {
        self.share >= Skin::MIN_SHARE
    }

    pub fn chroma(&self) -> f64 {
        self.a.hypot(self.b)
    }
}

/// Whether a CIELAB colour reads as skin: mid-to-light, moderately saturated, between red and
/// yellow (hue 25°–62°). Beige walls (hue past 65°) and saturated yellow or red backdrops
/// (chroma past 48) stay out.
pub fn is_skin([l, a, b]: [f64; 3]) -> bool {
    let c = a.hypot(b);
    let h = b.atan2(a).to_degrees();
    (30.0..=95.0).contains(&l) && (7.0..=48.0).contains(&c) && (25.0..=62.0).contains(&h)
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn lab_f(t: f64) -> f64 {
    if t > 216.0 / 24389.0 {
        t.cbrt()
    } else {
        (24389.0 / 27.0 * t + 16.0) / 116.0
    }
}

/// CIELAB of an sRGB pixel.
pub fn lab(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(srgb_to_linear);
    let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = (0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b) / 1.088_83;
    let (fx, fy, fz) = (lab_f(x), lab_f(y), lab_f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

impl Stats {
    pub fn of_file(path: &Path) -> Result<Stats> {
        let img = image::open(path)
            .with_context(|| format!("reading {} to measure it", path.display()))?
            .to_rgb8();
        Stats::of_pixels(
            img.pixels().map(|p| p.0),
            u64::from(img.width()) * u64::from(img.height()),
        )
    }

    /// Measures `count` pixels, sampling at most [`SAMPLE_PIXELS`] of them.
    pub fn of_pixels(pixels: impl Iterator<Item = [u8; 3]>, count: u64) -> Result<Stats> {
        const BINS: usize = 1001;
        let step = usize::try_from(count / SAMPLE_PIXELS).unwrap_or(1).max(1);
        let mut hist = vec![0u64; BINS];
        let (mut n, mut sum_l, mut sum_l2, mut sum_c) = (0u64, 0.0, 0.0, 0.0);
        let (mut mid, mut sum_a, mut sum_b) = (0u64, 0.0, 0.0);
        let (mut low, mut high) = (0u64, 0u64);
        let mut skin: Vec<[f64; 3]> = Vec::new();
        for px in pixels.step_by(step) {
            let [l, a, b] = lab(px);
            if is_skin([l, a, b]) {
                skin.push([l, a, b]);
            }
            let bin = ((l.clamp(0.0, 100.0) * 10.0).round() as usize).min(BINS - 1);
            if let Some(h) = hist.get_mut(bin) {
                *h += 1;
            }
            n += 1;
            sum_l += l;
            sum_l2 += l * l;
            sum_c += a.hypot(b);
            if (20.0..=85.0).contains(&l) {
                mid += 1;
                sum_a += a;
                sum_b += b;
            }
            if px.iter().all(|&c| c <= 2) {
                low += 1;
            }
            if px.iter().any(|&c| c >= 254) {
                high += 1;
            }
        }
        if n == 0 {
            bail!("an empty picture cannot be measured");
        }
        let nf = n as f64;
        let pct = |q: f64| {
            let want = (q * nf).ceil() as u64;
            let mut seen = 0u64;
            for (i, h) in hist.iter().enumerate() {
                seen += h;
                if seen >= want.max(1) {
                    return i as f64 / 10.0;
                }
            }
            100.0
        };
        let mean_l = sum_l / nf;
        let midf = (mid.max(1)) as f64;
        Ok(Stats {
            mean_l,
            p02: pct(0.02),
            p50: pct(0.5),
            p98: pct(0.98),
            contrast: (sum_l2 / nf - mean_l * mean_l).max(0.0).sqrt(),
            chroma: sum_c / nf,
            tint: sum_a / midf,
            warmth: sum_b / midf,
            clip_low: low as f64 * 100.0 / nf,
            clip_high: high as f64 * 100.0 / nf,
            skin: Skin::lit(skin, nf),
        })
    }

    /// The mean of several measurements (the target a set of references gives).
    pub fn mean(all: &[Stats]) -> Option<Stats> {
        if all.is_empty() {
            return None;
        }
        let k = all.len() as f64;
        let avg = |f: fn(&Stats) -> f64| all.iter().map(f).sum::<f64>() / k;
        Some(Stats {
            mean_l: avg(|s| s.mean_l),
            p02: avg(|s| s.p02),
            p50: avg(|s| s.p50),
            p98: avg(|s| s.p98),
            contrast: avg(|s| s.contrast),
            chroma: avg(|s| s.chroma),
            tint: avg(|s| s.tint),
            warmth: avg(|s| s.warmth),
            clip_low: avg(|s| s.clip_low),
            clip_high: avg(|s| s.clip_high),
            skin: Skin::mean(all),
        })
    }

    /// One line for the AI.
    pub fn describe(&self) -> String {
        let skin = if self.skin.found() {
            format!(
                "; SKIN (lit skin; {:.0} % of the picture is skin-toned): L {:.0}, a* {:+.1}, b* {:+.1}, chroma {:.1}",
                self.skin.share,
                self.skin.l,
                self.skin.a,
                self.skin.b,
                self.skin.chroma()
            )
        } else {
            "; no skin tones found".to_owned()
        };
        let whole = format!(
            "mean L {:.0}, median L {:.0}, black point (2nd percentile) {:.0}, white point (98th percentile) {:.0}, contrast (L std-dev) {:.1}, saturation (mean chroma) {:.1}, cast of the midtones a* {:+.1} (green − / magenta +) b* {:+.1} (blue − / yellow +), pure black {:.1} %, clipped white {:.1} %",
            self.mean_l,
            self.p50,
            self.p02,
            self.p98,
            self.contrast,
            self.chroma,
            self.tint,
            self.warmth,
            self.clip_low,
            self.clip_high
        );
        whole + &skin
    }
}

impl Skin {
    /// The lit skin among `pixels` (the brighter half): skin in shadow, under sheer stockings or
    /// tinted by a dark costume would drag the measurement away from how the face looks.
    fn lit(mut pixels: Vec<[f64; 3]>, of: f64) -> Skin {
        if pixels.is_empty() {
            return Skin::default();
        }
        let share = pixels.len() as f64 * 100.0 / of;
        pixels.sort_by(|x, y| y[0].total_cmp(&x[0]));
        pixels.truncate(pixels.len().div_ceil(2));
        let k = pixels.len() as f64;
        let sum = pixels
            .iter()
            .fold([0.0; 3], |s, p| [s[0] + p[0], s[1] + p[1], s[2] + p[2]]);
        Skin {
            share,
            l: sum[0] / k,
            a: sum[1] / k,
            b: sum[2] / k,
        }
    }

    /// The mean skin of the measurements that found some.
    fn mean(all: &[Stats]) -> Skin {
        let with: Vec<&Skin> = all.iter().map(|s| &s.skin).filter(|s| s.found()).collect();
        if with.is_empty() {
            return Skin::default();
        }
        let k = with.len() as f64;
        let avg = |f: fn(&Skin) -> f64| with.iter().map(|s| f(s)).sum::<f64>() / k;
        Skin {
            share: avg(|s| s.share),
            l: avg(|s| s.l),
            a: avg(|s| s.a),
            b: avg(|s| s.b),
        }
    }
}

// ---------- references ----------

/// One finished photo the look should match.
#[derive(Debug, Clone)]
pub struct Reference {
    pub name: String,
    /// A small render of it, sent to the AI.
    pub preview: PathBuf,
    pub stats: Stats,
}

/// A stable short key for a file: its path, size and modification time.
pub(crate) fn file_key(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let len = meta.as_ref().map(std::fs::Metadata::len).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:016x}",
        fnv(format!("{}|{len}|{mtime}", path.display()).as_bytes())
    )
}

/// FNV-1a: a short stable hash for cache file names.
pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Renders and measures the first [`MAX_REFERENCES`] of `files` (raw included), cached in
/// `scratch`. A reference that cannot be read is reported through `on` and left out.
pub fn references(
    lightcraft: &Path,
    files: &[PathBuf],
    scratch: &Path,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(String),
) -> Result<Vec<Reference>> {
    let dir = scratch.join("references");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut out = Vec::new();
    for f in files.iter().take(MAX_REFERENCES) {
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let preview = dir.join(format!("{}.jpg", file_key(f)));
        let made = if preview.is_file() {
            Ok(())
        } else {
            crate::analysis::preview(lightcraft, f, &preview, REFERENCE_SIZE, cancel)
        };
        match made.and_then(|()| Stats::of_file(&preview)) {
            Ok(stats) => out.push(Reference {
                name,
                preview,
                stats,
            }),
            Err(e) if procrun::is_cancelled(&e) => return Err(e),
            Err(e) => on(format!("Reference {name} left out: {e:#}")),
        }
    }
    Ok(out)
}

// ---------- LightCraft renders ----------

/// One develop slider as LightCraft reports it for a photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub id: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
}

/// Sliders the finish may set: tone, colour, grading, presence, vignette, grain and sharpening.
/// Geometry, optics, crop and HDR stay as the recipe left them.
fn tunable(id: &str) -> bool {
    const PREFIXES: &[&str] = &["light.", "wb.", "color.", "mixer.", "grading.", "effects."];
    const IDS: &[&str] = &[
        "curve.highlights",
        "curve.lights",
        "curve.darks",
        "curve.shadows",
        "vignette.amount",
        "vignette.midpoint",
        "vignette.feather",
        "grain.amount",
        "grain.size",
        "detail.sharpenAmount",
    ];
    PREFIXES.iter().any(|p| id.starts_with(p)) || IDS.contains(&id)
}

/// The recipe's steps a finishing render repeats: the whole-photo look of the first LightCraft
/// stage (develop settings, presets, profiles). Masks and retouching are left out: they change
/// little of the overall tone and some take a minute each.
pub fn look_steps(recipe: &Recipe) -> Vec<Step> {
    let first = recipe
        .stages
        .iter()
        .chain(recipe.variants.iter().flat_map(|v| &v.stages))
        .find_map(|s| match s {
            Stage::Lightcraft(l) => Some(&l.steps),
            _ => None,
        });
    first
        .map(|steps| {
            steps
                .iter()
                .filter(|s| {
                    s.optional.is_none()
                        && (s.cmd.starts_with("develop.") || s.cmd.starts_with("preset.apply"))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the PhotoCraft retouch can run: it goes into a PhotoCraft stage Kimera puts first, on
/// the photo at the size it was scanned. A recipe with PhotoCraft stages of its own would get the
/// retouch in its first one, after whatever cropping or resizing came before it.
pub fn retouch_fits(recipe: &Recipe) -> bool {
    !recipe
        .stages
        .iter()
        .chain(recipe.variants.iter().flat_map(|v| &v.stages))
        .any(|s| matches!(s, Stage::Photocraft(_)))
}

/// A render of `photo` with `steps` and the sliders it ends with.
struct Render {
    jpeg: PathBuf,
    controls: Vec<Control>,
}

fn render(
    lightcraft: &Path,
    photo: &Path,
    steps: &[Step],
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<Render> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut cmds: Vec<(&str, Value)> = Vec::new();
    for s in steps {
        cmds.push((
            &s.cmd,
            s.params
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?
                .unwrap_or_else(|| json!({})),
        ));
    }
    cmds.push(("develop.controls", json!({})));
    cmds.push((
        "app.export",
        json!({"dir": dir, "format": "jpeg", "longEdge": LOOK_SIZE, "quality": 85}),
    ));
    let mut cmd = Command::new(lightcraft);
    cmd.args(["run", "--keep-going", "--import"])
        .arg(photo)
        .args(["--script", "-"]);
    let o = procrun::run(cmd, Some(portrait::script(&cmds)?), cancel)?;
    let mut controls = None;
    let mut jpeg = None;
    for v in o
        .stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
    {
        let ok = v.get("ok").and_then(Value::as_bool) == Some(true);
        match v.get("command").and_then(Value::as_str) {
            Some("develop.controls") if ok => controls = v.get("result").map(parse_controls),
            Some("app.export") if ok => {
                jpeg = v
                    .pointer("/result/files/0/path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from);
            }
            _ => {}
        }
    }
    let jpeg = jpeg.filter(|p| p.is_file()).ok_or_else(|| {
        anyhow!(
            "LightCraft made no finishing render of {} ({}): {}",
            photo.display(),
            o.status,
            o.stderr.trim()
        )
    })?;
    Ok(Render {
        jpeg,
        controls: controls.unwrap_or_default(),
    })
}

fn parse_controls(v: &Value) -> Vec<Control> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(Control {
                        id: c.get("id")?.as_str()?.to_owned(),
                        min: c.get("min")?.as_f64()?,
                        max: c.get("max")?.as_f64()?,
                        value: c.get("value")?.as_f64()?,
                    })
                })
                .filter(|c| tunable(&c.id))
                .collect()
        })
        .unwrap_or_default()
}

// ---------- the AI ----------

const SYSTEM: &str = r#"You are a senior portrait retoucher finishing photos for a professional photographer in LightCraft (a raw developer with Lightroom-style sliders and local masks) and PhotoCraft. You finish ONE photo so it looks the way a top-tier photographer would deliver it.

You get the photo as it renders right now (with the recipe's look already applied), Kimera's measurements of it, the current value of every global slider you may set, and the local areas Kimera found on it (faces from a face detector, the person and hair cut out by a segmentation model).

REFERENCE PHOTOS, when attached, are the photographer's own finished work. Match their STYLE, not their content:
- the skin: its brightness, colour and finish (the SKIN measurements are the most important target numbers);
- the eyes and lips: how much they pop;
- how the subject is lit and separated from the background;
- contrast, black point and highlight roll-off, saturation, colour grading, clarity.
Their backdrops, costumes and wigs are content: a yellow backdrop in a reference does NOT mean this photo should turn yellow.

Without references, aim for a polished result true to the photographer's request: correct exposure and white balance, a full tonal range, flattering clean skin, eyes that hold the viewer, rich but believable colour.

You work in layers, like a retoucher:
1. "values": the GLOBAL develop sliders (absolute final positions; only the ones you want different from now).
2. "local": masked adjustments, one per area Kimera lists, each optional: {"<area>": {"values": {...}}}. Mask sliders are relative changes inside the mask: exposure [−2..2] (stops), contrast, highlights, shadows, whites, blacks, temp, tint, texture, clarity, dehaze, saturation, sharpness [−100..100].
3. "retouch": the PhotoCraft skin retouch strength, when offered.

Rules of the craft:
- Use only the global slider ids listed, inside their ranges, and only the areas listed.
- wb.temp / wb.tint: as listed now is how the camera saw it. On JPEGs, 6500/0 means as shot; small moves (±300 K, ±10) go a long way.
- Fix skin on its own layers (skin, flash, retouch), not by bending the whole photo's white balance. Ruddy or orange skin: skin saturation −10..−30 and tint/temp on the skin layer.
- Direct flash: tame the hot spots on "flash", lift the face gently on "face", and darken the background so the light looks shaped, not flat.
- Eyes: they make a portrait; clarity and a little exposure on "eyes", never a colour shift of the iris.
- Global: prefer exposure, whites/blacks, highlights/shadows and contrast; curve.* for fine tonal shape; vibrance before saturation. Global effects.clarity at most +15, effects.texture at most +10 in portraits.
- "Cinematic" means: controlled highlights, deep but not crushed blacks, colour grading (cool or teal shadows, warm highlights), a darker background with the subject lit — not orange skin.
- Subtle beats strong: every local value about half of what first comes to mind. A result a client would print, with nothing that looks edited.

Answer with JSON only, no prose around it:
{"values": {"<slider id>": <number>, ...},
 "local": {"<area>": {"values": {...}}, ...},
 "retouch": <0..100>,
 "assessment": "<one short sentence in the photographer's language: what you changed and why>"}"#;

/// Everything the finish needs besides the photo.
pub struct FinishJob<'a> {
    pub client: &'a Client,
    pub lightcraft: &'a Path,
    /// Where renders go (one folder per photo, removed afterwards).
    pub scratch: &'a Path,
    /// The photographer's request, in their words (may be empty).
    pub brief: &'a str,
    /// The recipe's whole-photo look ([`look_steps`]).
    pub base: &'a [Step],
    pub references: &'a [Reference],
    pub passes: Passes,
    /// Where the scans are cached ([`portrait::scratch`]).
    pub scans: &'a Path,
    /// Cut out the person and the hair with SAM 3 (about a minute per photo, once).
    pub sam: bool,
    /// Offer the PhotoCraft skin retouch.
    pub retouch: bool,
    pub cancel: &'a AtomicBool,
}

/// What the finish decided for one photo.
#[derive(Debug, Clone)]
pub struct Finish {
    pub steps: Vec<Step>,
    /// The PhotoCraft skin retouch, run on the photo before the look.
    pub photocraft: Vec<Step>,
    /// The AI's last sentence about it.
    pub summary: String,
    /// Everything the AI set, for the run's record.
    pub reply: Reply,
    /// The last render the AI saw and its measurements: an anchor for the rest of a series shot
    /// without references.
    pub anchor: Option<Reference>,
}

impl FinishJob<'_> {
    /// Finishes one photo. `anchor` is an earlier photo of the series to stay consistent with
    /// (used when there are no references). `on` hears what is going on.
    pub fn finish(
        &self,
        photo: &Path,
        anchor: Option<&Reference>,
        on: &mut dyn FnMut(String),
    ) -> Result<Finish> {
        let passes = self.passes.count();
        if passes == 0 {
            bail!("the finish is off");
        }
        let name = photo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = self
            .scratch
            .join(format!("finish-{}-{}", std::process::id(), file_key(photo)));
        let mut r = self.passes_on(photo, &name, &dir, anchor, passes, on);
        // The anchor's picture outlives the render folder.
        if let Ok(Finish {
            anchor: Some(a), ..
        }) = &mut r
        {
            let keep = self.scratch.join(format!("anchor-{}.jpg", file_key(photo)));
            if std::fs::copy(&a.preview, &keep).is_ok() {
                a.preview = keep;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn passes_on(
        &self,
        photo: &Path,
        name: &str,
        dir: &Path,
        anchor: Option<&Reference>,
        passes: usize,
        on: &mut dyn FnMut(String),
    ) -> Result<Finish> {
        on(format!(
            "{name}: finding the face{}",
            if self.sam {
                ", the person and the hair (the first time takes about a minute)"
            } else {
                ""
            }
        ));
        let scan = portrait::scan(self.lightcraft, photo, self.scans, self.sam, self.cancel)?;
        on(format!("{name}: {}", scan.describe()));
        on(format!("{name}: rendering the recipe's look"));
        let mut cur = render(
            self.lightcraft,
            photo,
            self.base,
            &dir.join("0"),
            self.cancel,
        )?;
        if cur.controls.is_empty() {
            bail!(
                "LightCraft listed no develop sliders for {}",
                photo.display()
            );
        }
        let mut turns: Vec<Turn> = Vec::new();
        let mut chosen = Reply::default();
        for pass in 1..=passes {
            let stats = Stats::of_file(&cur.jpeg)?;
            let mut parts = Vec::new();
            if pass == 1 {
                parts.extend(self.reference_parts(anchor));
                parts.push(Part::Text(format!(
                    "The photographer's request: {}\n\nThe photo to finish ({name}), as it renders now:",
                    if self.brief.trim().is_empty() {
                        "(none: make it look its best)"
                    } else {
                        self.brief.trim()
                    }
                )));
                parts.push(Part::Jpeg(cur.jpeg.clone()));
                parts.push(Part::Text(format!(
                    "Its measurements: {}\n{}\nSliders you may set (id [min..max] now):\n{}\n{}",
                    stats.describe(),
                    self.target_line(anchor),
                    sliders(&cur.controls),
                    self.areas_text(&scan)
                )));
            } else {
                parts.push(Part::Text("Here is the photo with your values:".to_owned()));
                parts.push(Part::Jpeg(cur.jpeg.clone()));
                parts.push(Part::Text(format!(
                    "Its measurements now: {}\n{}\nLook hard at it (next to the references when there are any). Correct what is still off — or keep what works — and give the COMPLETE set of values you want, in the same JSON shape (they replace your previous ones).",
                    stats.describe(),
                    self.target_line(anchor)
                )));
            }
            turns.push(Turn::user(parts));
            on(format!(
                "{name}: the AI sets the sliders (pass {pass} of {passes})"
            ));
            let mut reply = self.client.ask(SYSTEM, &turns, self.cancel)?;
            let mut parsed = parse_reply(&reply, &cur.controls);
            if let Err(e) = &parsed {
                // Once more, told what was wrong.
                turns.push(Turn::assistant(reply));
                turns.push(Turn::user(vec![Part::Text(format!(
                    "Kimera could not read that ({e:#}). Answer again with the JSON object only."
                ))]));
                reply = self.client.ask(SYSTEM, &turns, self.cancel)?;
                parsed = parse_reply(&reply, &cur.controls);
            }
            let got = match parsed {
                Ok(x) => x,
                Err(e) if pass > 1 => {
                    on(format!(
                        "{name}: pass {pass} kept the earlier values ({e:#})"
                    ));
                    break;
                }
                Err(e) => return Err(e.context(format!("reading the AI's finish of {name}"))),
            };
            on(format!("{name}: pass {pass}: {}", got.log_line()));
            chosen = got;
            turns.push(Turn::assistant(reply));
            if pass < passes {
                on(format!("{name}: rendering the AI's values"));
                let mut steps = self.base.to_vec();
                steps.extend(chosen.steps(&scan)?);
                cur = render(
                    self.lightcraft,
                    photo,
                    &steps,
                    &dir.join(pass.to_string()),
                    self.cancel,
                )?;
            }
        }
        let steps = chosen.steps(&scan)?;
        let photocraft = if self.retouch {
            portrait::retouch_steps(&scan, chosen.retouch)?
        } else {
            Vec::new()
        };
        // Without references, the first finished photo anchors the series.
        let anchor_out = if self.references.is_empty() && anchor.is_none() {
            Stats::of_file(&cur.jpeg).ok().map(|stats| Reference {
                name: name.to_owned(),
                preview: cur.jpeg.clone(),
                stats,
            })
        } else {
            None
        };
        Ok(Finish {
            steps,
            photocraft,
            summary: chosen.summary.clone(),
            reply: chosen,
            anchor: anchor_out,
        })
    }

    /// The local areas this photo offers and the retouch, for the AI.
    fn areas_text(&self, scan: &Scan) -> String {
        let mut t = format!(
            "Kimera found: {}. Local areas you may use (positions come from the detectors; you give only \"values\"):\n",
            scan.describe()
        );
        for a in Area::ALL.iter().filter(|a| a.available(scan)) {
            let _ = writeln!(t, "- \"{}\": {}", a.key(), a.about());
        }
        if scan.person.is_none() {
            t.push_str("  (no cut-out of the person: \"background\" also needs \"x\", \"y\", \"rx\", \"ry\" — the ellipse around the person, normalized; outside it is the background)\n");
        }
        if self.retouch && scan.face().is_some() {
            t.push_str("\"retouch\": 0..100, the strength of a frequency-separation skin retouch of the face in PhotoCraft (evens tone and blotches, keeps pores; eyes, brows and mouth untouched). About 30 natural, 50 polished, 70 beauty; 0 for none (men's stubble, older faces that should keep character).\n");
        }
        t
    }

    fn reference_parts(&self, anchor: Option<&Reference>) -> Vec<Part> {
        let mut parts = Vec::new();
        if !self.references.is_empty() {
            parts.push(Part::Text(format!(
                "Reference photos: the photographer's finished work ({} of {}):",
                self.references.len().min(MAX_REFERENCE_IMAGES),
                self.references.len()
            )));
            for r in self.references.iter().take(MAX_REFERENCE_IMAGES) {
                parts.push(Part::Jpeg(r.preview.clone()));
            }
        } else if let Some(a) = anchor {
            parts.push(Part::Text(format!(
                "An earlier photo of the same series ({}), already finished: keep this one consistent with it (same white balance feel, contrast and palette), adapting to its own light.",
                a.name
            )));
            parts.push(Part::Jpeg(a.preview.clone()));
        }
        parts
    }

    fn target_line(&self, anchor: Option<&Reference>) -> String {
        let all: Vec<Stats> = self.references.iter().map(|r| r.stats).collect();
        match (Stats::mean(&all), anchor) {
            (Some(t), _) => format!("Target (mean of the references): {}", t.describe()),
            (None, Some(a)) => format!(
                "The earlier finished photo measured: {}",
                a.stats.describe()
            ),
            (None, None) => String::new(),
        }
    }
}

fn sliders(controls: &[Control]) -> String {
    let mut s = String::new();
    for c in controls {
        let _ = writeln!(s, "{} [{}..{}] {}", c.id, c.min, c.max, c.value);
    }
    s
}

/// The sliders of a LightCraft mask the finish may set, with the limit it may use either way.
const MASK_SLIDERS: &[(&str, f64)] = &[
    ("exposure", 2.0),
    ("contrast", 100.0),
    ("highlights", 100.0),
    ("shadows", 100.0),
    ("whites", 100.0),
    ("blacks", 100.0),
    ("temp", 100.0),
    ("tint", 100.0),
    ("texture", 100.0),
    ("clarity", 100.0),
    ("dehaze", 100.0),
    ("saturation", 100.0),
    ("sharpness", 100.0),
];

/// Smallest half-size of the AI's background ellipse, as a share of the picture.
const MIN_RADIUS: f64 = 0.03;

/// A masked adjustment: which area and what it changes there.
#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    pub area: Area,
    pub values: Map<String, Value>,
    /// `[x, y, rx, ry]`: the AI's ellipse around the subject, for a background without a cut-out.
    pub ellipse: Option<[f64; 4]>,
}

impl Local {
    /// The mask's commands, placed from the scan, then its adjustment; `None` when the scan
    /// cannot place it.
    fn steps(&self, scan: &Scan) -> Option<Result<Vec<Step>>> {
        let mut cmds = portrait::mask_commands(self.area, scan, self.ellipse)?;
        cmds.push(("mask.adjust", json!({"values": self.values})));
        Some(
            cmds.into_iter()
                .map(|(cmd, p)| {
                    Ok(Step {
                        cmd: cmd.into(),
                        params: Some(toml::Value::try_from(p)?),
                        optional: Some(GROUP.into()),
                    })
                })
                .collect(),
        )
    }
}

/// Strongest PhotoCraft skin retouch the finish asks for (0..100).
pub const MAX_RETOUCH: u8 = 100;

/// What the AI decided for one photo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub values: Map<String, Value>,
    pub local: Vec<Local>,
    /// Strength of the PhotoCraft skin retouch, 0 for none.
    pub retouch: u8,
    pub summary: String,
}

impl Reply {
    /// The LightCraft steps: the global sliders, then the masks the scan can place; all in the
    /// finish group.
    pub fn steps(&self, scan: &Scan) -> Result<Vec<Step>> {
        let mut out = Vec::new();
        if !self.values.is_empty() {
            out.push(Step {
                cmd: "develop.set".into(),
                params: Some(toml::Value::try_from(json!({ "values": self.values }))?),
                optional: Some(GROUP.into()),
            });
        }
        for l in &self.local {
            if let Some(s) = l.steps(scan) {
                out.extend(s?);
            }
        }
        Ok(out)
    }

    /// One JSON line for the log and the run's record: what the AI set.
    pub fn log_line(&self) -> String {
        let mut obj = Map::new();
        obj.insert("values".into(), Value::Object(self.values.clone()));
        let mut local = Map::new();
        for l in &self.local {
            let mut a = Map::new();
            a.insert("values".into(), Value::Object(l.values.clone()));
            if let Some(e) = l.ellipse {
                a.insert("ellipse".into(), json!(e));
            }
            local.insert(l.area.key().into(), Value::Object(a));
        }
        obj.insert("local".into(), Value::Object(local));
        obj.insert("retouch".into(), json!(self.retouch));
        if !self.summary.is_empty() {
            obj.insert("assessment".into(), json!(self.summary));
        }
        serde_json::to_string(&obj).unwrap_or_default()
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn coord(v: &Value, key: &str) -> Option<f64> {
    v.get(key)
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .map(|x| round2(x.clamp(0.0, 1.0)))
}

fn local_of(area: Area, v: &Value) -> Option<Local> {
    let given = v.get("values").and_then(Value::as_object)?;
    let mut values = Map::new();
    for (id, lim) in MASK_SLIDERS {
        if let Some(x) = given
            .get(*id)
            .and_then(Value::as_f64)
            .filter(|x| x.is_finite())
        {
            values.insert((*id).into(), json!(round2(x.clamp(-lim, *lim))));
        }
    }
    if values.is_empty() {
        return None;
    }
    let ellipse = (area == Area::Background)
        .then(|| {
            Some([
                coord(v, "x")?,
                coord(v, "y")?,
                coord(v, "rx")?.max(MIN_RADIUS),
                coord(v, "ry")?.max(MIN_RADIUS),
            ])
        })
        .flatten();
    Some(Local {
        area,
        values,
        ellipse,
    })
}

/// The run's record of the finish, beside its output: one JSON line per photo.
pub const RECORD: &str = "finish.jsonl";

/// Adds one photo's finish to `<run_dir>/`[`RECORD`], so a run can be read back and repeated.
pub fn record(run_dir: &Path, photo: &str, reply: &Reply) -> Result<()> {
    use std::io::Write as _;
    let path = run_dir.join(RECORD);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(
        f,
        "{{\"photo\":{},\"finish\":{}}}",
        json!(photo),
        reply.log_line()
    )
    .with_context(|| format!("writing {}", path.display()))
}

/// The reply's global values (kept to listed sliders, clamped to their ranges), its masked
/// adjustments (clamped; incomplete ones dropped) and its sentence.
pub fn parse_reply(reply: &str, controls: &[Control]) -> Result<Reply> {
    // The first object that has `values`: replies sometimes add prose or a second block after it.
    let v = reply
        .match_indices('{')
        .filter_map(|(i, _)| {
            reply
                .get(i..)
                .and_then(|rest| {
                    serde_json::Deserializer::from_str(rest)
                        .into_iter::<Value>()
                        .next()
                })
                .and_then(Result::ok)
        })
        .find(|v| v.get("values").is_some())
        .context("no JSON object with `values` in the reply")?;
    let given = v
        .get("values")
        .and_then(Value::as_object)
        .context("the reply has no `values` object")?;
    let mut values = Map::new();
    let mut keys: Vec<&String> = given.keys().collect();
    keys.sort();
    for k in keys {
        let (Some(c), Some(x)) = (
            controls.iter().find(|c| &c.id == k),
            given.get(k).and_then(Value::as_f64),
        ) else {
            continue;
        };
        if !x.is_finite() {
            continue;
        }
        values.insert(k.clone(), json!(round2(x.clamp(c.min, c.max))));
    }
    let local = Area::ALL
        .iter()
        .filter_map(|&a| local_of(a, v.get("local")?.get(a.key())?))
        .collect();
    let retouch = v
        .get("retouch")
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, f64::from(MAX_RETOUCH)).round() as u8)
        .unwrap_or(0);
    let summary = v
        .get("assessment")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    Ok(Reply {
        values,
        local,
        retouch,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctl(id: &str, min: f64, max: f64) -> Control {
        Control {
            id: id.into(),
            min,
            max,
            value: 0.0,
        }
    }

    #[test]
    fn values_are_clamped_and_unknown_ids_dropped() -> Result<()> {
        let controls = [
            ctl("light.exposure", -5.0, 5.0),
            ctl("light.contrast", -100.0, 100.0),
        ];
        let r = parse_reply(
            "ok:
{\"values\": {\"light.exposure\": 9, \"light.contrast\": 12.345, \"crop.angle\": 3, \"x\": \"a\"}, \"assessment\": \"Mais contraste.\"}",
            &controls,
        )?;
        assert_eq!(r.values.get("light.exposure"), Some(&json!(5.0)));
        assert_eq!(r.values.get("light.contrast"), Some(&json!(12.35)));
        assert_eq!(r.values.len(), 2);
        assert_eq!(r.summary, "Mais contraste.");
        assert!(r.local.is_empty());
        assert!(parse_reply("no json", &controls).is_err());
        let r = parse_reply(
            "{\"values\": {\"light.contrast\": 5}} and then {\"note\": 1}}",
            &controls,
        )?;
        assert_eq!(r.values.get("light.contrast"), Some(&json!(5.0)));
        Ok(())
    }

    #[test]
    fn masked_adjustments_are_placed_by_the_scan() -> Result<()> {
        let r = parse_reply(
            r#"{"values": {}, "retouch": 140, "local": {
                "skin": {"values": {"texture": -30, "saturation": -150, "bogus": 3}},
                "eyes": {"values": {"exposure": 5}},
                "hair": {"values": {"saturation": 10}},
                "background": {"x": 0.5, "y": 0.5, "values": {"exposure": -0.5}}}}"#,
            &[],
        )?;
        assert_eq!(r.retouch, MAX_RETOUCH);
        assert_eq!(r.local.len(), 4);
        let skin = &r.local[0];
        assert_eq!(skin.area, Area::Skin);
        assert_eq!(skin.values.get("saturation"), Some(&json!(-100.0)));
        assert!(skin.values.get("bogus").is_none());
        assert_eq!(r.local[1].values.get("exposure"), Some(&json!(2.0)));
        // Nothing found: no face, no cut-out, and the background has no full ellipse.
        assert!(r.steps(&Scan::default())?.is_empty());
        let scan = Scan {
            width: 3000,
            height: 4500,
            faces: vec![portrait::Face {
                rect: [0.3, 0.1, 0.5, 0.25],
                eyes: [[0.37, 0.16], [0.44, 0.16]],
                nose: [0.4, 0.19],
                mouth: [[0.38, 0.22], [0.43, 0.22]],
            }],
            ..Scan::default()
        };
        let cmds: Vec<String> = r.steps(&scan)?.into_iter().map(|s| s.cmd).collect();
        assert_eq!(
            cmds,
            [
                "mask.add",
                "mask.sampleColor",
                "mask.sampleColor",
                "mask.adjust",
                "mask.add",
                "mask.addComponent",
                "mask.adjust"
            ]
        );
        let bg = parse_reply(
            r#"{"values": {}, "local": {"background": {"x": 0.5, "y": 0.5, "rx": 0.3, "ry": 0.45, "values": {"exposure": -0.5}}}}"#,
            &[],
        )?;
        let cmds: Vec<String> = bg
            .steps(&Scan::default())?
            .into_iter()
            .map(|s| s.cmd)
            .collect();
        assert_eq!(cmds, ["mask.add", "mask.invert", "mask.adjust"]);
        Ok(())
    }

    #[test]
    fn skin_is_told_from_backdrops() {
        // Light skin, a saturated yellow backdrop, a beige wall, a red wig.
        assert!(is_skin(lab([232, 196, 178])));
        assert!(is_skin(lab([198, 140, 110])));
        assert!(!is_skin(lab([235, 175, 20])));
        assert!(!is_skin(lab([196, 186, 160])));
        assert!(!is_skin(lab([220, 30, 25])));
    }

    #[test]
    fn only_look_sliders_are_tunable() {
        assert!(tunable("light.exposure") && tunable("mixer.orange.lum") && tunable("curve.darks"));
        assert!(!tunable("crop.angle") && !tunable("geometry.rotate") && !tunable("hdr.maxEv"));
        assert!(!tunable("curve.splitMid") && !tunable("optics.distortion"));
    }

    #[test]
    fn lab_of_greys_and_white() {
        let [l, a, b] = lab([255, 255, 255]);
        assert!((l - 100.0).abs() < 0.1 && a.abs() < 0.1 && b.abs() < 0.1);
        let [l, ..] = lab([0, 0, 0]);
        assert!(l.abs() < 0.1);
        let [l, ..] = lab([119, 119, 119]);
        assert!((l - 50.0).abs() < 1.0);
    }

    #[test]
    fn stats_of_a_split_picture() -> Result<()> {
        // Half black, half white: mean L 50, black and white points at the ends, both clipped.
        let px = (0..1000).map(|i| if i < 500 { [0u8; 3] } else { [255u8; 3] });
        let s = Stats::of_pixels(px, 1000)?;
        assert!((s.mean_l - 50.0).abs() < 0.5);
        assert!(s.p02 < 1.0 && s.p98 > 99.0);
        assert!((s.clip_low - 50.0).abs() < 0.1 && (s.clip_high - 50.0).abs() < 0.1);
        assert!((s.contrast - 50.0).abs() < 0.5);
        Ok(())
    }

    #[test]
    fn look_steps_keep_global_develop_steps_only() -> Result<()> {
        let r = crate::recipe::parse(include_str!("../recipes/cleanup-cinematic.toml"))?;
        let cmds: Vec<String> = look_steps(&r).into_iter().map(|s| s.cmd).collect();
        assert_eq!(cmds, ["develop.set"]);
        Ok(())
    }
}
