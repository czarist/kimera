//! The Craft apps' command registries, read from their CLIs and cached per app version. The AI
//! gets them in its prompt, and every recipe it writes is checked against them.

use crate::apps::{App, lightcraft};
use crate::procrun;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cmd {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub params: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub app: App,
    pub version: String,
    pub commands: Vec<Cmd>,
}

/// Commands that only drive the app's own window: never useful in a recipe.
const UI_ONLY: &[&str] = &[
    "view.",
    "window.",
    "help.",
    "prefs.",
    "updates.",
    "plugin.",
    "slideshow.",
    "print.",
    "tools.",
    "tool.",
    "workspace.",
    "ui.",
    "app.quit",
    "notes.",
    "measurementLog.",
    "edit.preferences",
    "file.scripts.browse",
    "file.scripts.scriptEventsManager",
];

impl Catalog {
    pub fn ids(&self) -> HashSet<&str> {
        self.commands.iter().map(|c| c.id.as_str()).collect()
    }

    pub fn has(&self, id: &str) -> bool {
        self.commands.iter().any(|c| c.id == id)
    }

    /// Prompt text: full detail for the ids in `detailed`, ids only for the rest.
    pub fn prompt_text(&self, detailed: &HashSet<String>) -> String {
        let mut full = String::new();
        let mut rest = Vec::new();
        for c in &self.commands {
            if UI_ONLY.iter().any(|p| c.id.starts_with(p)) {
                continue;
            }
            if detailed.contains(&c.id) {
                let params = if c.params.is_empty() { "{}" } else { &c.params };
                let _ = writeln!(full, "- `{}` ({}): {}", c.id, c.label, one_line(params));
            } else {
                rest.push(c.id.as_str());
            }
        }
        format!(
            "### {} {} commands\n\nDetailed:\n{full}\nAlso available (ids only; ask for nothing, use only when sure of the parameters): {}\n",
            self.app,
            self.version,
            rest.join(", ")
        )
    }
}

/// Longest parameter text per command in the prompt; the few longer ones (app.export) are cut.
const PARAMS_LIMIT: usize = 700;

fn one_line(s: &str) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > PARAMS_LIMIT {
        let cut: String = s.chars().take(PARAMS_LIMIT).collect();
        format!("{cut}…")
    } else {
        s
    }
}

/// Command ids written in backticks in a text (the playbook): those get full detail.
pub fn mentioned_ids(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for part in text.split('`').skip(1).step_by(2) {
        let id = part.split([' ', '{', '(']).next().unwrap_or("");
        if id.contains('.')
            && id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            && !id.ends_with('.')
        {
            out.insert(id.to_owned());
        }
    }
    out
}

/// The app's catalog, from `cache_dir` when this version was read before.
pub fn load(app: App, cli: &Path, cache_dir: &Path, cancel: &AtomicBool) -> Result<Catalog> {
    let version = crate::apps::version(cli)?;
    let safe: String = version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cache = cache_dir.join(format!("commands-{app}-{safe}.json"));
    if let Ok(text) = std::fs::read_to_string(&cache)
        && let Ok(c) = serde_json::from_str::<Catalog>(&text)
    {
        return Ok(c);
    }
    let commands = read(app, cli, cancel).with_context(|| format!("reading {app}'s commands"))?;
    let cat = Catalog {
        app,
        version,
        commands,
    };
    if std::fs::create_dir_all(cache_dir).is_ok() {
        let _ = std::fs::write(&cache, serde_json::to_string(&cat)?);
    }
    Ok(cat)
}

fn read(app: App, cli: &Path, cancel: &AtomicBool) -> Result<Vec<Cmd>> {
    let list: Value = match app {
        App::LightCraft => lightcraft::query(
            cli,
            &[("engine.commands", Value::Object(Default::default()))],
            cancel,
        )?
        .into_iter()
        .next()
        .unwrap_or(Value::Null),
        _ => {
            let mut cmd = Command::new(cli);
            cmd.arg("commands");
            if app == App::PhotoCraft {
                cmd.arg("--json");
            }
            let o = procrun::run(cmd, None, cancel)?;
            procrun::check(&o, &format!("{app}-cli commands"))?;
            serde_json::from_str(&o.stdout).context("the command list is not JSON")?
        }
    };
    let Value::Array(items) = list else {
        bail!("{app}: the command list is not a JSON array");
    };
    Ok(items
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Cmd>(v).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playbook_ids_are_found() {
        let ids = mentioned_ids(
            "use `spot.findBlemishes {sensitivity: 50}` and `mask.add` but not `lc.cinematic` x",
        );
        assert!(ids.contains("spot.findBlemishes"));
        assert!(ids.contains("mask.add"));
        // preset ids look like commands; harmless, they just match nothing.
        assert!(ids.contains("lc.cinematic"));
        assert!(mentioned_ids(include_str!("../prompts/playbook.md")).contains("paint.remove"));
    }

    #[test]
    fn prompt_lists_detail_then_ids_and_skips_ui() {
        let c = Catalog {
            app: App::LightCraft,
            version: "1.0".into(),
            commands: vec![
                Cmd {
                    id: "mask.add".into(),
                    label: "Create New Mask".into(),
                    params: "{kind}".into(),
                },
                Cmd {
                    id: "crop.reset".into(),
                    label: "Reset".into(),
                    params: "{}".into(),
                },
                Cmd {
                    id: "view.zoomIn".into(),
                    label: "Zoom".into(),
                    params: "{}".into(),
                },
            ],
        };
        let t = c.prompt_text(&["mask.add".to_owned()].into());
        assert!(t.contains("- `mask.add` (Create New Mask): {kind}"));
        assert!(t.contains("crop.reset"));
        assert!(!t.contains("view.zoomIn"));
    }
}
