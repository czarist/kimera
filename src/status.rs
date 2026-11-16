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
}

fn check(name: &'static str, r: Result<String>, missing: Level) -> Check {
    match r {
        Ok(detail) => Check {
            name,
            level: Level::Ok,
            detail,
        },
        Err(e) => Check {
            name,
            level: missing,
            detail: format!("{e:#}"),
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
        let ok = |n: &str| checks.iter().any(|c| c.name == n && c.level == Level::Ok);
        (checks.len() == 3).then(|| Models {
            sam3: ok("AI masks"),
            yunet: ok("Face detector"),
            lensdb: ok("Lens database"),
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
            if app != "lightcraft" {
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
    let lenses = q
        .get(2)
        .and_then(|v| v.get("lenses"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let model = |name, ok: bool, yes: String, no: &str| Check {
        name,
        level: if ok { Level::Ok } else { Level::Info },
        detail: if ok { yes } else { no.to_owned() },
    };
    vec![
        model(
            "AI masks",
            sam,
            "SAM 3 installed (subject, background, sky, prompt masks)".into(),
            "SAM 3 not installed: no subject/background/sky/prompt masks (`kimera models install sam3 --accept`, about 3.4 GB)",
        ),
        model(
            "Face detector",
            yunet,
            "YuNet installed (automatic blemish removal)".into(),
            "YuNet not installed: no automatic blemish removal (`kimera models install yunet --accept`, 232 KB)",
        ),
        model(
            "Lens database",
            lenses > 0,
            format!("{lenses} lens profiles"),
            "not downloaded: no lens profile corrections (`kimera models install lensdb --accept`)",
        ),
    ]
}

const YUNET: &str = "yunet-2023mar";

/// Installs a model through the running LightCraft (`lightcraft --control`), waiting for the
/// download. `accept` says the user accepted the model's licence.
pub fn install(
    cfg: &Config,
    which: &str,
    accept: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<()> {
    if !accept {
        bail!(
            "installing {which} downloads a third-party model under its own licence; run again with --accept to agree"
        );
    }
    let mut lc = lightcraft::Control::new(&cfg.lightcraft.control)?;
    type Done = fn(&Value) -> Option<Result<()>>;
    let (start, status, done): ((&str, Value), &str, Done) = match which {
        "sam3" => (
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
        "yunet" => (
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
        "lensdb" => (
            ("lens.downloadDatabase", json!({})),
            "lens.databaseStatus",
            |v| match v.pointer("/download/state").and_then(Value::as_str) {
                Some("done") => Some(Ok(())),
                Some("failed") | Some("cancelled") => {
                    Some(Err(anyhow::anyhow!("lens database download stopped: {v}")))
                }
                _ => None,
            },
        ),
        other => bail!("unknown model {other:?}: sam3, yunet or lensdb"),
    };
    lc.execute(start.0, start.1)?;
    let t = Instant::now();
    loop {
        let v = lc.execute(status, json!({}))?;
        if let Some(r) = done(&v) {
            return r;
        }
        if t.elapsed() > Duration::from_secs(2 * 60 * 60) {
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
        std::thread::sleep(Duration::from_secs(2));
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
