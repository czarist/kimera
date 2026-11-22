//! What Kimera finds on a portrait before the AI looks at it, and the precise local work built on
//! it: the faces and their landmarks (LightCraft's YuNet detector), the person and the hair (SAM 3,
//! segmented once and stored, so later renders need no model), the masks placed from them, and a
//! frequency-separation skin retouch in PhotoCraft.
//!
//! Positions come from the detectors, never from the AI: the AI only says how much each area
//! changes.

use crate::procrun;
use crate::recipe::Step;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

/// The optional group of the PhotoCraft retouch: if PhotoCraft refuses a step, the photo goes on
/// without it.
pub const RETOUCH_GROUP: &str = "retouch";

/// Faces worked on per photo, largest first: a portrait's subjects, not a crowd.
const MAX_FACES: usize = 3;

/// Faces smaller than this share of the picture's width are background people.
const MIN_FACE_WIDTH: f64 = 0.04;

/// Mask names, so the stored segmentations are found again in `develop.get`.
const PERSON_MASK: &str = "Kimera person";
const HAIR_MASK: &str = "Kimera hair";

/// One face: normalized `[x, y]` from the top-left of the developed picture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Face {
    /// `[x0, y0, x1, y1]`.
    pub rect: [f64; 4],
    /// The eyes, the nose tip and the mouth corners, in YuNet's order.
    pub eyes: [[f64; 2]; 2],
    pub nose: [f64; 2],
    pub mouth: [[f64; 2]; 2],
}

/// What a scan of one photo found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Scan {
    /// The developed picture's size in pixels (PhotoCraft works in pixels).
    pub width: u32,
    pub height: u32,
    /// Largest first.
    pub faces: Vec<Face>,
    /// SAM 3's stored segmentations (LightCraft mask shapes), when it found them.
    pub person: Option<Value>,
    pub hair: Option<Value>,
    /// Whether SAM 3 was asked (a scan without it is not reused when it is wanted).
    pub sam: bool,
}

impl Scan {
    pub fn face(&self) -> Option<&Face> {
        self.faces.first()
    }

    /// One line for the AI and the log.
    pub fn describe(&self) -> String {
        let mut parts = vec![match self.faces.len() {
            0 => "no face found".to_owned(),
            1 => "1 face".to_owned(),
            n => format!("{n} faces (the masks follow the largest)"),
        }];
        if self.person.is_some() {
            parts.push("the person cut out".into());
        }
        if self.hair.is_some() {
            parts.push("the hair cut out".into());
        }
        parts.join(", ")
    }
}

/// Looks for faces (and, with `sam`, the person and the hair) on `photo` as LightCraft develops
/// it. Cached in `scratch` by the file's path, size and date: SAM 3 takes about a minute.
pub fn scan(
    lightcraft: &Path,
    photo: &Path,
    scratch: &Path,
    sam: bool,
    cancel: &AtomicBool,
) -> Result<Scan> {
    std::fs::create_dir_all(scratch).with_context(|| format!("creating {}", scratch.display()))?;
    let cache = scratch.join(format!("scan-{}.json", crate::look::file_key(photo)));
    if let Some(s) = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|t| serde_json::from_str::<Scan>(&t).ok())
        .filter(|s| s.sam || !sam)
    {
        return Ok(s);
    }
    let dir = scratch.join(format!("scan-{}", std::process::id()));
    let mut cmds: Vec<(&str, Value)> = vec![("faces.detect", json!({"apply": false}))];
    if sam {
        cmds.push((
            "mask.add",
            json!({"kind": "prompt", "text": "person", "name": PERSON_MASK}),
        ));
        cmds.push((
            "mask.add",
            json!({"kind": "prompt", "text": "hair", "name": HAIR_MASK}),
        ));
    }
    cmds.push(("develop.get", json!({})));
    // Full size, low quality: only its pixel size is wanted.
    cmds.push((
        "app.export",
        json!({"dir": dir, "format": "jpeg", "quality": 30}),
    ));
    let o = run_script(lightcraft, photo, &cmds, cancel);
    let _ = std::fs::remove_dir_all(&dir);
    let o = o?;
    let mut s = Scan {
        sam,
        ..Scan::default()
    };
    for v in o
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
    {
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let r = v.get("result").unwrap_or(&Value::Null);
        match v.get("command").and_then(Value::as_str) {
            Some("faces.detect") => s.faces = faces_of(r),
            Some("develop.get") => {
                s.person = stored_shape(r, PERSON_MASK);
                s.hair = stored_shape(r, HAIR_MASK);
            }
            Some("app.export") => {
                let px = |k: &str| {
                    r.pointer(&format!("/files/0/{k}"))
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0)
                };
                s.width = px("width");
                s.height = px("height");
            }
            _ => {}
        }
    }
    if s.width == 0 || s.height == 0 {
        bail!(
            "LightCraft could not develop {} to scan it",
            photo.display()
        );
    }
    if let Ok(t) = serde_json::to_string(&s) {
        let _ = std::fs::write(&cache, t);
    }
    Ok(s)
}

/// `lightcraft-cli run --keep-going --import <photo> --script -` with `cmds` on stdin: stored
/// segmentations run to tens of kilobytes, past what a Windows command line holds.
pub fn run_script(
    lightcraft: &Path,
    photo: &Path,
    cmds: &[(&str, Value)],
    cancel: &AtomicBool,
) -> Result<String> {
    let mut cmd = Command::new(lightcraft);
    cmd.args(["run", "--keep-going", "--import"])
        .arg(photo)
        .args(["--script", "-"]);
    let o = procrun::run(cmd, Some(script(cmds)?), cancel)?;
    Ok(o.stdout)
}

/// JSON lines for `--script`.
pub fn script(cmds: &[(&str, Value)]) -> Result<String> {
    let mut s = String::new();
    for (c, p) in cmds {
        s.push_str(&serde_json::to_string(&json!({"command": c, "params": p}))?);
        s.push('\n');
    }
    Ok(s)
}

fn point(v: Option<&Value>) -> Option<[f64; 2]> {
    let a = v?.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

fn faces_of(r: &Value) -> Vec<Face> {
    let mut out: Vec<Face> = r
        .pointer("/photos/0/faces")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|f| {
                    let rect = f.get("rect")?;
                    let g = |k: &str| rect.get(k).and_then(Value::as_f64);
                    let lm = f.get("landmarks")?.as_array()?;
                    Some(Face {
                        rect: [g("x0")?, g("y0")?, g("x1")?, g("y1")?],
                        eyes: [point(lm.first())?, point(lm.get(1))?],
                        nose: point(lm.get(2))?,
                        mouth: [point(lm.get(3))?, point(lm.get(4))?],
                    })
                })
                .filter(|f| f.rect[2] - f.rect[0] >= MIN_FACE_WIDTH)
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| area(b).total_cmp(&area(a)));
    out.truncate(MAX_FACES);
    out
}

fn area(f: &Face) -> f64 {
    (f.rect[2] - f.rect[0]) * (f.rect[3] - f.rect[1])
}

/// The first component's shape of the mask called `name`, nulls dropped (recipes are TOML).
fn stored_shape(r: &Value, name: &str) -> Option<Value> {
    let shape = r
        .get("masks")?
        .as_array()?
        .iter()
        .find(|m| m.get("name").and_then(Value::as_str) == Some(name))?
        .pointer("/components/0/shape")?;
    shape.pointer("/seg/data")?.as_str()?;
    Some(without_nulls(shape))
}

fn without_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, x)| !x.is_null())
                .map(|(k, x)| (k.clone(), without_nulls(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(without_nulls).collect()),
        x => x.clone(),
    }
}

// ---------- areas ----------

/// The places the finish can change on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    /// Skin everywhere, by colour, sampled on the cheeks.
    Skin,
    /// A soft light on the face.
    Face,
    /// The face's brightest tones: the flash's hot spots.
    Flash,
    Eyes,
    Lips,
    /// The person, cut out.
    Subject,
    Hair,
    /// Everything but the person (or outside an ellipse when SAM 3 found nobody).
    Background,
}

impl Area {
    pub const ALL: [Area; 8] = [
        Area::Skin,
        Area::Face,
        Area::Flash,
        Area::Eyes,
        Area::Lips,
        Area::Subject,
        Area::Hair,
        Area::Background,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Area::Skin => "skin",
            Area::Face => "face",
            Area::Flash => "flash",
            Area::Eyes => "eyes",
            Area::Lips => "lips",
            Area::Subject => "subject",
            Area::Hair => "hair",
            Area::Background => "background",
        }
    }

    /// What the AI is told about it.
    pub fn about(self) -> &'static str {
        match self {
            Area::Skin => {
                "all skin (face, neck, hands), selected by colour: tone, colour, smoothness (texture −10..−30, clarity −5..−15)"
            }
            Area::Face => "a soft light over the face: exposure +0.1..+0.4, shadows",
            Area::Flash => {
                "only the brightest tones of the face (flash hot spots, shine on forehead and cheeks): highlights −20..−60, exposure −0.1..−0.4"
            }
            Area::Eyes => {
                "the eyes (iris and lashes): clarity +10..+30, saturation +5..+25, exposure +0.1..+0.3, sharpness +10..+30, whites"
            }
            Area::Lips => "the lips: saturation, tint, exposure, a little clarity",
            Area::Subject => {
                "the whole person, cut out: exposure, contrast, clarity, to lift them from the background"
            }
            Area::Hair => {
                "the hair or wig, cut out: saturation, exposure, texture, clarity for shine"
            }
            Area::Background => {
                "everything but the person: exposure −0.2..−1.0, saturation, temp, clarity, to separate and set the mood"
            }
        }
    }

    /// Whether the scan makes this area possible (the background always is, as an ellipse).
    pub fn available(self, s: &Scan) -> bool {
        match self {
            Area::Skin | Area::Face | Area::Flash | Area::Eyes | Area::Lips => s.face().is_some(),
            Area::Subject => s.person.is_some(),
            Area::Hair => s.hair.is_some(),
            Area::Background => true,
        }
    }
}

/// The ellipse feather (0..100) of the soft areas.
const SOFT_FEATHER: f64 = 70.0;

/// Lowest lightness (0..1) the flash area takes: hot spots, not the whole face.
const FLASH_FROM: f64 = 0.62;

fn mid(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

fn r2(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// A radial mask shape: `rx` in widths, `ry` in heights, as LightCraft takes them.
fn radial(c: [f64; 2], rx: f64, ry: f64, feather: f64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), json!("radial"));
    m.insert("center".into(), json!([r2(c[0]), r2(c[1])]));
    m.insert("rx".into(), json!(r2(rx)));
    m.insert("ry".into(), json!(r2(ry)));
    m.insert("feather".into(), json!(feather));
    m
}

/// The cheeks: below each eye, a little outwards, clear of eyes, nose and mouth.
pub fn cheeks(f: &Face) -> [[f64; 2]; 2] {
    let down = (mid(f.mouth[0], f.mouth[1])[1] - mid(f.eyes[0], f.eyes[1])[1]) * 0.55;
    let d = (f.eyes[1][0] - f.eyes[0][0]).abs();
    let out = |e: [f64; 2], sign: f64| [e[0] + sign * d * 0.12, e[1] + down];
    let left = if f.eyes[0][0] <= f.eyes[1][0] {
        -1.0
    } else {
        1.0
    };
    [out(f.eyes[0], left), out(f.eyes[1], -left)]
}

/// The LightCraft commands that make `area`'s mask (before its `mask.adjust`), or `None` when the
/// scan cannot place it. `ellipse` is the AI's `[x, y, rx, ry]` for a background without a cut-out.
pub fn mask_commands(
    area: Area,
    s: &Scan,
    ellipse: Option<[f64; 4]>,
) -> Option<Vec<(&'static str, Value)>> {
    let name = json!(format!("Kimera {}", area.key()));
    let (w, h) = (f64::from(s.width.max(1)), f64::from(s.height.max(1)));
    let face = s.face();
    // Sizes from the face, in pixels, back to widths and heights.
    let (sx, sy) = (|px: f64| px / w, |px: f64| px / h);
    let with_name = |mut m: Map<String, Value>| {
        m.insert("name".into(), name.clone());
        Value::Object(m)
    };
    let cmds = match area {
        Area::Skin => {
            let [a, b] = cheeks(face?);
            vec![
                ("mask.add", json!({"kind": "colorRange", "name": name})),
                ("mask.sampleColor", json!({"x": r2(a[0]), "y": r2(a[1])})),
                (
                    "mask.sampleColor",
                    json!({"x": r2(b[0]), "y": r2(b[1]), "add": true}),
                ),
            ]
        }
        Area::Face | Area::Flash => {
            let f = face?;
            let c = [(f.rect[0] + f.rect[2]) / 2.0, (f.rect[1] + f.rect[3]) / 2.0];
            let (fw, fh) = (f.rect[2] - f.rect[0], f.rect[3] - f.rect[1]);
            let mut v = vec![(
                "mask.add",
                with_name(radial(c, fw * 0.62, fh * 0.62, SOFT_FEATHER)),
            )];
            if area == Area::Flash {
                v.push((
                    "mask.addComponent",
                    json!({"op": "intersect", "kind": "luminanceRange", "lo": FLASH_FROM,
                           "hi": 1.0, "lo_feather": 0.12, "hi_feather": 0.0}),
                ));
            }
            v
        }
        Area::Eyes => {
            let f = face?;
            let d = (f.eyes[1][0] - f.eyes[0][0]).hypot(f.eyes[1][1] - f.eyes[0][1]) * w;
            let (rx, ry) = (sx(d * 0.26), sy(d * 0.17));
            let mut second = radial(f.eyes[1], rx, ry, 55.0);
            second.insert("op".into(), json!("add"));
            vec![
                ("mask.add", with_name(radial(f.eyes[0], rx, ry, 55.0))),
                ("mask.addComponent", Value::Object(second)),
            ]
        }
        Area::Lips => {
            let f = face?;
            let mw = (f.mouth[1][0] - f.mouth[0][0]).hypot(f.mouth[1][1] - f.mouth[0][1]) * w;
            let m = mid(f.mouth[0], f.mouth[1]);
            let c = [m[0], m[1] + sy(mw * 0.08)];
            vec![
                (
                    "mask.add",
                    with_name(radial(c, sx(mw * 0.62), sy(mw * 0.34), 40.0)),
                ),
                (
                    "mask.addComponent",
                    json!({"op": "intersect", "kind": "colorRange"}),
                ),
                ("mask.sampleColor", json!({"x": r2(c[0]), "y": r2(c[1])})),
            ]
        }
        Area::Subject | Area::Hair => {
            let shape = if area == Area::Subject {
                s.person.as_ref()?
            } else {
                s.hair.as_ref()?
            };
            let mut m = shape.as_object()?.clone();
            m.insert("name".into(), name.clone());
            vec![("mask.add", Value::Object(m))]
        }
        Area::Background => match (&s.person, ellipse) {
            (Some(shape), _) => {
                let mut m = shape.as_object()?.clone();
                m.insert("name".into(), name.clone());
                vec![("mask.add", Value::Object(m)), ("mask.invert", json!({}))]
            }
            (None, Some([x, y, rx, ry])) => vec![
                ("mask.add", with_name(radial([x, y], rx, ry, SOFT_FEATHER))),
                ("mask.invert", json!({})),
            ],
            (None, None) => return None,
        },
    };
    Some(cmds)
}

// ---------- PhotoCraft skin retouch ----------

/// The frequency-separation skin retouch, in PhotoCraft, of the scan's faces (`strength`
/// 1..100). Tone and blotches are evened on a blurred copy while the pores stay on a second
/// layer, so skin keeps its texture; eyes, brows and mouth are left out. Runs on the photo at the
/// scan's size, before the look.
pub fn retouch_steps(s: &Scan, strength: u8) -> Result<Vec<Step>> {
    if s.faces.is_empty() || strength == 0 {
        return Ok(Vec::new());
    }
    let (w, h) = (f64::from(s.width), f64::from(s.height));
    let st = f64::from(strength.min(100));
    let widest = s
        .faces
        .iter()
        .map(|f| f.rect[2] - f.rect[0])
        .fold(0.0, f64::max)
        * w;
    // The split radius: pores and fine lines go to the detail layer.
    let split = (widest / 120.0).clamp(1.5, 40.0);
    let px = |p: [f64; 2]| json!([(p[0] * w).round(), (p[1] * h).round()]);
    let ell = |c: [f64; 2], rx: f64, ry: f64, mode: &str| {
        json!({"x": (c[0] * w - rx).round(), "y": (c[1] * h - ry).round(),
               "width": (2.0 * rx).round(), "height": (2.0 * ry).round(),
               "ellipse": true, "mode": mode, "feather": 0})
    };
    let mut cmds: Vec<(&str, Value)> = vec![
        ("layer.duplicate", json!({"name": "Kimera detail"})),
        ("layer.select", json!({"name": "Background"})),
        ("layer.duplicate", json!({"name": "Kimera tone"})),
        ("layer.select", json!({"name": "Kimera tone"})),
        ("filter.blur.gaussianBlur", json!({"radius": r2(split)})),
        ("layer.select", json!({"name": "Kimera detail"})),
        (
            "image.applyImage",
            json!({"source": {"layer": "Kimera tone"}, "blending": "subtract", "scale": 2, "offset": 128}),
        ),
        ("layer.setProps", json!({"blend": "LinearLight"})),
        ("layer.select", json!({"name": "Kimera tone"})),
    ];
    let mut samples = Vec::new();
    for (i, f) in s.faces.iter().enumerate() {
        let c = [(f.rect[0] + f.rect[2]) / 2.0, (f.rect[1] + f.rect[3]) / 2.0];
        let (fw, fh) = ((f.rect[2] - f.rect[0]) * w, (f.rect[3] - f.rect[1]) * h);
        let mode = if i == 0 { "replace" } else { "add" };
        cmds.push(("select.rect", ell(c, fw * 0.62, fh * 0.62, mode)));
        samples.extend(cheeks(f).map(px));
    }
    cmds.push((
        "select.colorRange",
        json!({"points": samples, "fuzziness": 45, "mode": "intersect"}),
    ));
    for f in &s.faces {
        let d = (f.eyes[1][0] - f.eyes[0][0]).hypot(f.eyes[1][1] - f.eyes[0][1]) * w;
        for e in f.eyes {
            cmds.push(("select.rect", ell(e, d * 0.36, d * 0.24, "subtract")));
        }
        let mw = (f.mouth[1][0] - f.mouth[0][0]).hypot(f.mouth[1][1] - f.mouth[0][1]) * w;
        let m = mid(f.mouth[0], f.mouth[1]);
        cmds.push(("select.rect", ell(m, mw * 0.72, mw * 0.42, "subtract")));
    }
    cmds.push(("select.modify.feather", json!({"radius": r2(split * 2.0)})));
    cmds.push((
        "filter.blur.surfaceBlur",
        json!({"radius": (split * (1.5 + st / 40.0)).round().clamp(1.0, 100.0),
               "threshold": (12.0 + st * 0.2).round()}),
    ));
    cmds.push(("select.deselect", json!({})));
    cmds.into_iter()
        .map(|(cmd, p)| {
            Ok(Step {
                cmd: cmd.into(),
                params: Some(toml::Value::try_from(p)?),
                optional: Some(RETOUCH_GROUP.into()),
            })
        })
        .collect()
}

/// Where the scans of a work folder are kept.
pub fn scratch(work: &Path) -> PathBuf {
    work.join(".kimera").join("scan")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face() -> Face {
        Face {
            rect: [0.32, 0.118, 0.513, 0.262],
            eyes: [[0.3997, 0.1687], [0.4852, 0.1689]],
            nose: [0.4604, 0.198],
            mouth: [[0.4037, 0.2235], [0.4791, 0.2219]],
        }
    }

    fn scan_with_face() -> Scan {
        Scan {
            width: 3064,
            height: 5456,
            faces: vec![face()],
            ..Scan::default()
        }
    }

    #[test]
    fn detections_are_read_largest_first() {
        let r = json!({"photos": [{"faces": [
            {"rect": {"x0": 0.1, "x1": 0.15, "y0": 0.1, "y1": 0.15}, "landmarks": [[0.11,0.12],[0.14,0.12],[0.125,0.13],[0.115,0.14],[0.135,0.14]]},
            {"rect": {"x0": 0.32, "x1": 0.513, "y0": 0.118, "y1": 0.262}, "landmarks": [[0.3997,0.1687],[0.4852,0.1689],[0.46,0.198],[0.4037,0.2235],[0.4791,0.2219]]},
            {"rect": {"x0": 0.9, "x1": 0.91, "y0": 0.9, "y1": 0.91}, "landmarks": [[0.9,0.9],[0.91,0.9],[0.905,0.905],[0.9,0.91],[0.91,0.91]]}
        ]}]});
        let f = faces_of(&r);
        assert_eq!(f.len(), 2, "the tiny face is dropped");
        assert!((f[0].rect[0] - 0.32).abs() < 1e-9);
    }

    #[test]
    fn cheeks_sit_between_eyes_and_mouth() {
        let [a, b] = cheeks(&face());
        assert!(a[0] < 0.3997 && b[0] > 0.4852);
        assert!(a[1] > 0.17 && a[1] < 0.222);
    }

    #[test]
    fn areas_need_what_they_stand_on() {
        let none = Scan::default();
        assert!(!Area::Eyes.available(&none) && !Area::Subject.available(&none));
        assert!(Area::Background.available(&none));
        assert!(mask_commands(Area::Background, &none, None).is_none());
        let s = scan_with_face();
        let eyes = mask_commands(Area::Eyes, &s, None).unwrap_or_default();
        let cmds: Vec<&str> = eyes.iter().map(|(c, _)| *c).collect();
        assert_eq!(cmds, ["mask.add", "mask.addComponent"]);
        let flash = mask_commands(Area::Flash, &s, None).unwrap_or_default();
        assert_eq!(
            flash.get(1).and_then(|(_, p)| p.get("kind")),
            Some(&json!("luminanceRange"))
        );
    }

    #[test]
    fn a_stored_cut_out_is_reused_without_nulls() {
        let r = json!({"masks": [{"name": PERSON_MASK, "components": [{"shape":
            {"kind": "prompt", "text": "person", "exclude": null, "seg": {"data": "abc", "side": 288}}}]}]});
        let shape = stored_shape(&r, PERSON_MASK);
        assert_eq!(
            shape,
            Some(json!({"kind": "prompt", "text": "person", "seg": {"data": "abc", "side": 288}}))
        );
        assert!(stored_shape(&r, HAIR_MASK).is_none());
        let s = Scan {
            person: shape,
            ..Scan::default()
        };
        let bg = mask_commands(Area::Background, &s, None).unwrap_or_default();
        let cmds: Vec<&str> = bg.iter().map(|(c, _)| *c).collect();
        assert_eq!(cmds, ["mask.add", "mask.invert"]);
    }

    #[test]
    fn retouch_separates_then_smooths_the_skin_only() -> Result<()> {
        let steps = retouch_steps(&scan_with_face(), 50)?;
        let cmds: Vec<&str> = steps.iter().map(|s| s.cmd.as_str()).collect();
        assert_eq!(cmds.first(), Some(&"layer.duplicate"));
        assert!(cmds.contains(&"image.applyImage") && cmds.contains(&"select.colorRange"));
        assert_eq!(cmds.last(), Some(&"select.deselect"));
        // Two eyes and a mouth are cut out of the selection.
        assert_eq!(
            steps
                .iter()
                .filter(|s| s.params_json().unwrap_or_default().contains("subtract")
                    && s.cmd == "select.rect")
                .count(),
            3
        );
        assert!(
            steps
                .iter()
                .all(|s| s.optional.as_deref() == Some(RETOUCH_GROUP))
        );
        assert!(retouch_steps(&Scan::default(), 50)?.is_empty());
        Ok(())
    }
}
