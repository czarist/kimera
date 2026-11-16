//! Per-photo look with the AI's vision: who blinked, red eyes, where the subject is. The result
//! adds per-photo steps (red-eye fixes) and flags photos for review (closed eyes, with a
//! sibling frame where the same people have their eyes open when there is one).

use crate::ai::{Client, Part, Turn};
use crate::procrun;
use crate::recipe::Step;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

const PROMPT: &str = r#"You check one photo for a photo-editing conveyor. Look at the image and answer with
JSON only, no prose, in exactly this shape:

{"faces": <number of human faces large enough to retouch>,
 "closed_eyes": <true when any of those faces has closed or half-closed eyes (a blink)>,
 "red_eye": [{"x": <0..1 from the left>, "y": <0..1 from the top>, "r": <pupil radius as a fraction of the image's long edge>}],
 "subject_box": [x0, y0, x1, y1] or null (the main subject, 0..1),
 "notes": "<at most one short sentence: anything a retoucher must know>"}

Coordinates are fractions of the image width and height. List red_eye only for pupils that are
clearly red from a flash. When unsure about a blink, say false."#;

/// Long edge of the preview the AI looks at: enough to see eyes, small enough to send fast.
const LOOK_SIZE: u32 = 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Report {
    pub faces: u32,
    pub closed_eyes: bool,
    pub red_eye: Vec<RedEye>,
    pub subject_box: Option<[f64; 4]>,
    pub notes: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RedEye {
    pub x: f64,
    pub y: f64,
    pub r: f64,
}

impl Report {
    /// Steps to run on this photo in its first LightCraft stage (optional group `analysis`).
    pub fn extra_steps(&self) -> Vec<Step> {
        self.red_eye
            .iter()
            .filter(|e| {
                (0.0..=1.0).contains(&e.x) && (0.0..=1.0).contains(&e.y) && e.r > 0.0 && e.r < 0.2
            })
            .map(|e| Step {
                cmd: "redeye.add".into(),
                params: toml::Value::try_from(serde_json::json!({
                    "center": [e.x, e.y], "rx": e.r, "ry": e.r
                }))
                .ok(),
                optional: Some("analysis".into()),
            })
            .collect()
    }
}

/// Renders a small JPEG of any photo LightCraft opens (raw included) for the AI to look at.
pub fn preview(
    lightcraft: &Path,
    input: &Path,
    out: &Path,
    size: u32,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut cmd = Command::new(lightcraft);
    cmd.arg("render").arg(input).arg("-o").arg(out).args([
        "--size",
        &size.to_string(),
        "--quality",
        "80",
    ]);
    let o = procrun::run(cmd, None, cancel)?;
    procrun::check(&o, &format!("LightCraft preview of {}", input.display()))?;
    if !out.is_file() {
        bail!("LightCraft wrote no preview for {}", input.display());
    }
    Ok(())
}

/// Looks at one photo. `scratch` holds the preview.
pub fn analyse(
    client: &Client,
    lightcraft: &Path,
    photo: &Path,
    scratch: &Path,
    cancel: &AtomicBool,
) -> Result<Report> {
    std::fs::create_dir_all(scratch)?;
    let stem = photo
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let jpg = scratch.join(format!("look-{stem}-{}.jpg", std::process::id()));
    preview(lightcraft, photo, &jpg, LOOK_SIZE, cancel)?;
    let reply = client.ask(
        PROMPT,
        &[Turn::user(vec![
            Part::Jpeg(jpg.clone()),
            Part::Text("Check this photo.".into()),
        ])],
        cancel,
    );
    let _ = std::fs::remove_file(&jpg);
    parse(&reply?).with_context(|| format!("reading the AI's look at {}", photo.display()))
}

/// The first JSON object in the reply.
pub fn parse(reply: &str) -> Result<Report> {
    let start = reply.find('{').context("no JSON in the reply")?;
    let end = reply.rfind('}').context("no JSON in the reply")?;
    if end < start {
        bail!("no JSON in the reply");
    }
    Ok(serde_json::from_str(&reply[start..=end])?)
}

/// Closed-eye photos and, for each, nearby frames (±3 in order) with the same number of faces
/// and open eyes: the swap donors a retoucher would try first.
pub fn review(photos: &[PathBuf], reports: &[Option<Report>]) -> Vec<(usize, Vec<usize>)> {
    let mut out = Vec::new();
    for (i, r) in reports.iter().enumerate() {
        let Some(r) = r else { continue };
        if !r.closed_eyes {
            continue;
        }
        let lo = i.saturating_sub(3);
        let hi = i.saturating_add(3).min(photos.len().saturating_sub(1));
        let donors = (lo..=hi)
            .filter(|&j| j != i)
            .filter(|&j| {
                reports
                    .get(j)
                    .and_then(Option::as_ref)
                    .is_some_and(|d| !d.closed_eyes && d.faces == r.faces && d.faces > 0)
            })
            .collect();
        out.push((i, donors));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_with_prose_around_json_is_read() -> Result<()> {
        let r = parse(
            "Sure:\n{\"faces\": 2, \"closed_eyes\": true, \"red_eye\": [{\"x\":0.4,\"y\":0.3,\"r\":0.01}], \"subject_box\": null, \"notes\": \"\"}\n",
        )?;
        assert_eq!(r.faces, 2);
        assert!(r.closed_eyes);
        let steps = r.extra_steps();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].cmd, "redeye.add");
        assert_eq!(
            steps[0].params_json()?,
            r#"{"center":[0.4,0.3],"rx":0.01,"ry":0.01}"#
        );
        assert!(parse("nothing").is_err());
        Ok(())
    }

    #[test]
    fn implausible_red_eyes_are_dropped() {
        let r = Report {
            red_eye: vec![
                RedEye {
                    x: 1.5,
                    y: 0.2,
                    r: 0.01,
                },
                RedEye {
                    x: 0.5,
                    y: 0.5,
                    r: 0.5,
                },
            ],
            ..Report::default()
        };
        assert!(r.extra_steps().is_empty());
    }

    #[test]
    fn donors_are_neighbours_with_open_eyes_and_the_same_faces() {
        let p: Vec<PathBuf> = (0..6).map(|i| PathBuf::from(format!("{i}.jpg"))).collect();
        let rep = |faces, closed| {
            Some(Report {
                faces,
                closed_eyes: closed,
                ..Report::default()
            })
        };
        let reports = vec![
            rep(2, false),
            rep(2, true),
            rep(3, false),
            rep(2, false),
            None,
            rep(2, false),
        ];
        assert_eq!(review(&p, &reports), [(1, vec![0, 3])]);
    }
}
