//! Health checks shared by `kimera doctor` and the window's setup screen, plus installing the
//! LightCraft models some recipes need.

use crate::ai::Client;
use crate::apps::{self, lightcraft};
use crate::config::Config;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    /// Works without it, but some requests need it.
    Info,
    Fail,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
    /// The optional LightCraft model this check is about, when it can be installed.
    pub model: Option<Model>,
}

fn check(name: &'static str, r: Result<String>, missing: Level) -> Check {
    match r {
        Ok(detail) => Check {
            name,
            level: Level::Ok,
            detail,
            model: None,
        },
        Err(e) => Check {
            name,
            level: missing,
            detail: format!("{e:#}"),
            model: None,
        },
    }
}

/// Every check, in display order.
pub fn all(cfg: &Config, cancel: &AtomicBool) -> Vec<Check> {
    let mut out = vec![
        check(
            "LightCraft",
            lightcraft::version(&cfg.lightcraft.cli),
            Level::Fail,
        ),
        check(
            "PhotoCraft",
            apps::photocraft::version(&cfg.photocraft.cli),
            Level::Fail,
        ),
    ];
    if let Some(vc) = &cfg.vectorcraft {
        out.push(check("VectorCraft", apps::version(&vc.cli), Level::Fail));
    }
    out.push(check(
        "Work folder",
        std::fs::create_dir_all(&cfg.work.dir)
            .map(|()| cfg.work.dir.display().to_string())
            .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", cfg.work.dir.display())),
        Level::Fail,
    ));
    out.push(check(
        "AI",
        Client::new(cfg.ai.clone(), cfg.work.dir.join(".kimera")).check(),
        Level::Fail,
    ));
    if out.first().is_some_and(|c| c.level == Level::Ok) {
        out.extend(models(cfg, cancel));
    }
    out.push(check(
        "LC control",
        lightcraft::Control::new(&cfg.lightcraft.control)
            .and_then(|mut c| c.selection())
            .map(|s| format!("{} answers, {} photo(s) selected", cfg.lightcraft.control, s.len()))
            .map_err(|_| {
                anyhow::anyhow!(
                    "not running (only `kimera pipeline` and model installs need `lightcraft --control 7980`)"
                )
            }),
        Level::Info,
    ));
    out
}

/// Which optional LightCraft models are installed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Models {
    pub sam3: bool,
    pub yunet: bool,
    pub lensdb: bool,
}

impl Models {
    pub fn read(cfg: &Config, cancel: &AtomicBool) -> Option<Models> {
        let checks = models(cfg, cancel);
        let ok = |m: Model| {
            checks
                .iter()
                .any(|c| c.model == Some(m) && c.level == Level::Ok)
        };
        (checks.len() == 3).then(|| Models {
            sam3: ok(Model::Sam3),
            yunet: ok(Model::Yunet),
            lensdb: ok(Model::LensDb),
        })
    }

    /// One line for the AI's prompt.
    pub fn describe(&self) -> String {
        let yn = |b: bool| if b { "installed" } else { "NOT installed" };
        format!(
            "LightCraft models on this computer: SAM 3 AI masks {}, YuNet face detector {}, lens database {}. Do not use steps that need a missing model; say in the notes what would need it.",
            yn(self.sam3),
            yn(self.yunet),
            yn(self.lensdb)
        )
    }

    /// Steps of `r` that need a model this computer lacks: `(step, model, install name)`.
    pub fn missing_for(&self, r: &crate::recipe::Recipe) -> Vec<String> {
        let mut out = Vec::new();
        for (app, cmd, kind) in r.steps_with_kind() {
            if app != crate::apps::App::LightCraft {
                continue;
            }
            let needs_sam = cmd == "lensBlur.findSubject"
                || (cmd == "mask.add" || cmd == "mask.addComponent")
                    && matches!(
                        kind.as_deref(),
                        Some("subject" | "background" | "sky" | "object" | "prompt")
                    );
            let needs_yunet = matches!(cmd, "spot.findBlemishes" | "faces.detect");
            if needs_sam && !self.sam3 {
                out.push(format!(
                    "{cmd} needs SAM 3 (`kimera models install sam3 --accept`)"
                ));
            }
            if needs_yunet && !self.yunet {
                out.push(format!(
                    "{cmd} needs YuNet (`kimera models install yunet --accept`)"
                ));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// Lenses in the database of the running LightCraft, when its control channel answers.
fn running_lenses(cfg: &Config) -> Option<u64> {
    let mut lc = lightcraft::Control::new(&cfg.lightcraft.control).ok()?;
    if !lc.answers() {
        return None;
    }
    lc.execute("lens.databaseStatus", json!({}))
        .ok()?
        .get("lenses")
        .and_then(Value::as_u64)
}

/// The optional LightCraft models: AI masks (SAM 3), face detector (YuNet), lens database.
pub fn models(cfg: &Config, cancel: &AtomicBool) -> Vec<Check> {
    let q = lightcraft::query(
        &cfg.lightcraft.cli,
        &[
            ("segment.model.status", json!({})),
            ("faces.models.list", json!({})),
            ("lens.databaseStatus", json!({})),
        ],
        cancel,
    );
    let Ok(q) = q else {
        return vec![Check {
            model: None,
            name: "LC models",
            level: Level::Info,
            detail: "could not ask LightCraft about its models".into(),
        }];
    };
    let sam = q
        .first()
        .and_then(|v| v.get("installed"))
        .and_then(Value::as_bool)
        == Some(true);
    let yunet = q
        .get(1)
        .and_then(|v| v.get("models"))
        .and_then(Value::as_array)
        .is_some_and(|m| {
            m.iter().any(|x| {
                x.get("id").and_then(Value::as_str) == Some(YUNET)
                    && x.get("installed").and_then(Value::as_bool) == Some(true)
            })
        });
    // A headless demo session does not load the lens database, so it always counts 0 lenses;
    // the running app (when it answers) knows whether it is installed.
    let lenses = running_lenses(cfg).unwrap_or_else(|| {
        q.get(2)
            .and_then(|v| v.get("lenses"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });
    let model = |name, which: Model, ok: bool, yes: String, no: &str| Check {
        name,
        model: Some(which),
        level: if ok { Level::Ok } else { Level::Info },
        detail: if ok { yes } else { no.to_owned() },
    };
    vec![
        model(
            "AI masks",
            Model::Sam3,
            sam,
            "SAM 3 installed (subject, background, sky, prompt masks)".into(),
            "SAM 3 not installed: no subject/background/sky/prompt masks (`kimera models install sam3 --accept`, about 3.4 GB)",
        ),
        model(
            "Face detector",
            Model::Yunet,
            yunet,
            "YuNet installed (automatic blemish removal)".into(),
            "YuNet not installed: no automatic blemish removal (`kimera models install yunet --accept`, 232 KB)",
        ),
        model(
            "Lens database",
            Model::LensDb,
            lenses > 0,
            format!("{lenses} lens profiles"),
            "not downloaded: no lens profile corrections (`kimera models install lensdb --accept`)",
        ),
    ]
}

/// LightCraft's id for the YuNet face detector.
const YUNET: &str = "yunet-2023mar";

/// How long a model download may take before Kimera gives up waiting (SAM 3 is about 3.4 GB).
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(2 * 60 * 60);

/// How often the download's progress is asked for.
const POLL_EVERY: Duration = Duration::from_secs(2);

/// Downloads started before giving up: a dropped connection is common, a second try usually
/// gets through.
const DOWNLOAD_TRIES: u32 = 3;

/// Pause before trying a failed download again.
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// An optional LightCraft model some recipes need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// SAM 3 AI masks: subject, background, sky, object and prompt masks (about 3.4 GB).
    Sam3,
    /// YuNet face detector: automatic blemish removal (232 KB).
    Yunet,
    /// lensfun lens profiles.
    LensDb,
}

impl Model {
    /// The name used by `kimera models install`: `sam3`, `yunet`, `lensdb`.
    pub const fn id(self) -> &'static str {
        match self {
            Model::Sam3 => "sam3",
            Model::Yunet => "yunet",
            Model::LensDb => "lensdb",
        }
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

impl std::str::FromStr for Model {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sam3" => Ok(Model::Sam3),
            "yunet" => Ok(Model::Yunet),
            "lensdb" => Ok(Model::LensDb),
            other => bail!("unknown model {other:?}: sam3, yunet or lensdb"),
        }
    }
}

/// Installs a model through the running LightCraft (`lightcraft --control`) and waits for the
/// download. Call it only after the user accepted the model's licence.
///
/// # Errors
///
/// When LightCraft does not answer, the download fails, or it takes longer than
/// [`DOWNLOAD_LIMIT`].
pub fn install(cfg: &Config, which: Model, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let mut lc = lightcraft::Control::new(&cfg.lightcraft.control)?;
    type Done = fn(&Value) -> Option<Result<()>>;
    let (start, status, done): ((&str, Value), &str, Done) = match which {
        Model::Sam3 => (
            ("segment.model.download", json!({"acknowledged": true})),
            "segment.model.status",
            |v| {
                if v.get("installed").and_then(Value::as_bool) == Some(true) {
                    return Some(Ok(()));
                }
                v.pointer("/download/error")
                    .and_then(Value::as_str)
                    .map(|e| Err(anyhow::anyhow!("SAM 3 download failed: {e}")))
            },
        ),
        Model::Yunet => (
            (
                "faces.models.download",
                json!({"id": YUNET, "acknowledged": true}),
            ),
            "faces.models.list",
            |v| {
                let installed = v.get("models").and_then(Value::as_array).is_some_and(|m| {
                    m.iter().any(|x| {
                        x.get("id").and_then(Value::as_str) == Some(YUNET)
                            && x.get("installed").and_then(Value::as_bool) == Some(true)
                    })
                });
                installed.then_some(Ok(()))
            },
        ),
        Model::LensDb => (
            ("lens.downloadDatabase", json!({})),
            "lens.databaseStatus",
            |v| match v.pointer("/download/state").and_then(Value::as_str) {
                Some("done") => Some(Ok(())),
                Some("failed") | Some("cancelled") => {
                    let why = v
                        .pointer("/download/error")
                        .and_then(Value::as_str)
                        .map_or_else(|| v.to_string(), str::to_owned);
                    Some(Err(anyhow::anyhow!(
                        "lens database download stopped: {why}"
                    )))
                }
                _ => None,
            },
        ),
    };
    lc.execute(start.0, start.1.clone())?;
    let t = Instant::now();
    let mut tries = 1u32;
    loop {
        let v = lc.execute(status, json!({}))?;
        match done(&v) {
            // The downloads come from public mirrors that drop connections now and then.
            Some(Err(e)) if tries < DOWNLOAD_TRIES => {
                tries += 1;
                progress(&format!(
                    "{which}: {e:#}; trying again ({tries} of {DOWNLOAD_TRIES})"
                ));
                std::thread::sleep(RETRY_AFTER);
                lc.execute(start.0, start.1.clone())?;
                continue;
            }
            Some(r) => return r,
            None => {}
        }
        if t.elapsed() > DOWNLOAD_LIMIT {
            bail!("{which}: still downloading after 2 hours; check LightCraft");
        }
        if let (Some(d), Some(n)) = (
            v.pointer("/download/done").and_then(Value::as_u64),
            v.pointer("/download/total").and_then(Value::as_u64),
        ) && n > 0
        {
            progress(&format!(
                "{which}: {} of {} MB",
                d / 1_000_000,
                n / 1_000_000
            ));
        } else {
            progress(&format!("{which}: downloading…"));
        }
        std::thread::sleep(POLL_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_models_are_named_and_optional_steps_ignored() -> Result<()> {
        let r = crate::recipe::parse(
            "[[stage]]\napp = \"lightcraft\"\nsteps = [\n  { cmd = \"spot.findBlemishes\" },\n  { cmd = \"mask.add\", params = { kind = \"sky\" } },\n  { cmd = \"mask.add\", params = { kind = \"prompt\", text = \"teeth\" }, optional = \"t\" },\n  { cmd = \"mask.add\", params = { kind = \"radial\" } },\n]\n",
        )?;
        let none = Models::default();
        let m = none.missing_for(&r);
        assert_eq!(m.len(), 2, "{m:?}");
        assert!(m.iter().any(|s| s.contains("YuNet")));
        assert!(m.iter().any(|s| s.contains("SAM 3")));
        let all = Models {
            sam3: true,
            yunet: true,
            lensdb: true,
        };
        assert!(all.missing_for(&r).is_empty());
        Ok(())
    }
}
