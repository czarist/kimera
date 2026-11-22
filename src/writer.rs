//! Request → recipe: the AI reads the playbook and the apps' real command lists, answers with
//! notes for the user and one TOML recipe; Kimera checks the recipe and sends errors back once
//! or twice before giving up.

use crate::ai::{Client, Part, Turn};
use crate::catalog::{self, Catalog};
use crate::recipe::{self, Recipe};
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

pub const PLAYBOOK: &str = include_str!("../prompts/playbook.md");

const CONTRACT: &str = r#"# Your job

You are Kimera's recipe writer. A photographer, designer or marketer describes an edit; you
answer with a recipe Kimera runs headless on their photos, one Craft app at a time.

Answer in exactly this shape, and nothing else:

1. Notes for the user, in the language they wrote in, at most 8 short lines: what the recipe
   does, which photos may stop and why, what to test on one photo first, what cannot be done.
2. One fenced code block marked `toml` with the recipe.

## Recipe format

```toml
[[stage]]                       # shared stages, in order; each is one app pass per photo
app = "lightcraft"              # "lightcraft" | "photocraft" | "vectorcraft"
steps = [
  { cmd = "mask.add", params = { kind = "prompt", text = "teeth" }, optional = "teeth" },
  { cmd = "mask.adjust", params = { values = { exposure = 0.15 } }, optional = "teeth" },
]
export = { format = "jpg", long_edge = 0, quality = 92, color_space = "srgb", options = { } }

[[stage]]
app = "photocraft"
out_ext = "tif"                 # "pcraft" only on a last stage
steps = [{ cmd = "select.subject" }]

[[variant]]                     # optional: several results per photo
name = "stories"                # letters, digits, - or _
[[variant.stage]]
app = "lightcraft"
steps = [{ cmd = "crop.aspect", params = { aspect = [9, 16] } }]
export = { format = "jpg", long_edge = 1920 }
```

Rules:
- Turn on only the apps the request needs; the others stay off. Most colour, retouch, mask,
  crop and export work is LightCraft alone. Add PhotoCraft only for its pixel tools (object
  removal it does better, cut-outs, white backgrounds, canvas, text, placed images, liquify) and
  VectorCraft only for layouts. Never add a stage without steps just to convert a file, except
  a LightCraft stage whose export is the point (resize, watermark, format).
- Say in the notes which apps the recipe uses.
- Use only command ids from the lists below, with the parameters they document.
- `optional = "<group>"` on every step of a group that may find nothing on some photos (prompt or
  object masks, select.subject, removeBackground…): if one step of the group fails, Kimera
  leaves the whole group out for that photo instead of failing it.
- A LightCraft stage always exports; `export.options` takes any other `app.export` option
  (watermark, limitKb, sharpen, metadata, removeLocation…) but never path, dir or conflict.
- Raw files must go through a LightCraft stage before any PhotoCraft stage.
- Never invent text, logos, file paths or studio names the user did not give. If the recipe needs
  one, leave that step out and say so in the notes.
"#;

pub struct Draft {
    pub notes: String,
    pub toml: String,
    pub recipe: Recipe,
}

/// The system prompt: contract, playbook and command lists (stable, so providers cache it).
pub fn system_prompt(catalogs: &[Catalog]) -> String {
    let detailed = catalog::mentioned_ids(PLAYBOOK);
    let mut s = format!("{CONTRACT}\n\n---\n\n{PLAYBOOK}\n\n---\n\n# Command lists\n\n");
    for c in catalogs {
        s.push_str(&c.prompt_text(&detailed));
        s.push('\n');
    }
    s
}

/// "12 photos: 10 .jpg, 2 .cr3 (raw)".
pub fn describe(photos: &[PathBuf]) -> String {
    let mut by_ext: BTreeMap<String, usize> = BTreeMap::new();
    for p in photos {
        let e = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "?".into());
        *by_ext.entry(e).or_default() += 1;
    }
    let parts: Vec<String> = by_ext
        .iter()
        .map(|(e, n)| {
            let raw = crate::engine::RAW_EXTS.contains(&e.as_str());
            format!("{n} .{e}{}", if raw { " (raw)" } else { "" })
        })
        .collect();
    format!("{} photo(s): {}", photos.len(), parts.join(", "))
}

/// Splits a reply into notes and the first ```toml block.
pub fn split(reply: &str) -> (String, Option<String>) {
    let Some(start) = reply.find("```toml") else {
        return (reply.trim().to_owned(), None);
    };
    let body = &reply[start + "```toml".len()..];
    let Some(end) = body.find("```") else {
        return (
            reply[..start].trim().to_owned(),
            Some(body.trim().to_owned()),
        );
    };
    let notes = format!("{}\n{}", &reply[..start], &body[end + 3..]);
    (notes.trim().to_owned(), Some(body[..end].trim().to_owned()))
}

/// Every command id the recipe uses must exist in its app's list, and every app the recipe
/// turns on must do something: a PhotoCraft stage without steps only converts the file, so it
/// is refused (LightCraft can export any format itself).
pub fn check(r: &Recipe, catalogs: &[Catalog]) -> Result<()> {
    for (name, chain) in r.chains() {
        if let Some(i) = chain
            .iter()
            .position(|s| matches!(s, crate::recipe::Stage::Photocraft(_)) && s.steps().is_empty())
        {
            bail!(
                "{}stage {} turns PhotoCraft on without steps: leave PhotoCraft off",
                name.map(|v| format!("variant {v}, ")).unwrap_or_default(),
                i + 1
            );
        }
    }
    let mut unknown = Vec::new();
    for (app, cmd) in r.commands() {
        if let Some(c) = catalogs.iter().find(|c| c.app == app)
            && !c.has(cmd)
        {
            unknown.push(format!("{app}: {cmd}"));
        }
    }
    if !unknown.is_empty() {
        bail!("unknown command ids: {}", unknown.join(", "));
    }
    Ok(())
}

/// What the recipe is asked for.
#[derive(Debug, Default)]
pub struct Request<'a> {
    /// The user's words.
    pub text: &'a str,
    pub photos: &'a [PathBuf],
    /// A recipe the user wants changed.
    pub previous: Option<&'a str>,
    /// This computer (installed models).
    pub context: &'a str,
    /// Small JPEGs of the user's finished photos whose look the recipe should carry.
    pub references: &'a [PathBuf],
    /// Whether the per-photo finish ([`crate::look`]) runs after the recipe.
    pub finishing: bool,
}

/// Tells the writer what the per-photo finish takes care of.
const FINISHING_NOTE: &str = "After this recipe, Kimera's per-photo finish runs: the AI looks at every photo as the recipe renders it and sets LightCraft's develop sliders for that photo (exposure, white balance, contrast, tone curve, colour mixer, grading, clarity, vignette, grain), matching the reference photos when there are any. So the recipe should carry what is NOT per-photo slider work: retouching, masks and local work, black & white, a creative look the request asks for, crops, formats and variants. Keep any develop.set values for the general look moderate; don't fight the finish with strong global tone values.";

/// Asks for a recipe, retrying twice with the error when the answer does not check out.
pub fn write(
    client: &Client,
    catalogs: &[Catalog],
    req: &Request<'_>,
    cancel: &AtomicBool,
) -> Result<Draft> {
    let system = system_prompt(catalogs);
    let mut first = format!(
        "{}\n{}\n\nRequest: {}",
        describe(req.photos),
        req.context,
        req.text
    );
    if req.finishing {
        let _ = write!(first, "\n\n{FINISHING_NOTE}");
    }
    if let Some(p) = req.previous {
        let _ = write!(
            first,
            "\n\nChange this recipe rather than starting over:\n```toml\n{p}\n```"
        );
    }
    let mut parts = Vec::new();
    if !req.references.is_empty() {
        parts.push(Part::Text(format!(
            "The photographer's finished work ({} photo(s)) shows the look they want. Read its style (colour or black & white, mood, contrast, grading, grain, crop and format) and build the recipe's look from it:",
            req.references.len()
        )));
        parts.extend(req.references.iter().cloned().map(Part::Jpeg));
    }
    parts.push(Part::Text(first));
    let mut turns = vec![Turn::user(parts)];
    let mut last_err = String::new();
    for _ in 0..3 {
        let reply = client.ask(&system, &turns, cancel)?;
        let (notes, toml) = split(&reply);
        let result = match &toml {
            None => Err(anyhow::anyhow!("the answer has no ```toml block")),
            Some(t) => recipe::parse(t).and_then(|r| check(&r, catalogs).map(|()| r)),
        };
        match (result, toml) {
            (Ok(recipe), Some(toml)) => {
                return Ok(Draft {
                    notes,
                    toml,
                    recipe,
                });
            }
            (Err(e), _) => {
                last_err = format!("{e:#}");
                turns.push(Turn::assistant(reply));
                turns.push(Turn::user(vec![Part::Text(format!(
                    "Kimera refused that recipe: {last_err}. Answer again in the same shape, fixing it."
                ))]));
            }
            (Ok(_), None) => {}
        }
    }
    bail!("the AI did not produce a valid recipe: {last_err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Cmd;

    #[test]
    fn reply_is_split_into_notes_and_toml() {
        let (n, t) = split(
            "Vou limpar a pele.\n```toml\n[[stage]]\napp = \"lightcraft\"\n```\nTeste em uma foto.",
        );
        assert_eq!(t.as_deref(), Some("[[stage]]\napp = \"lightcraft\""));
        assert!(n.contains("Vou limpar") && n.contains("Teste em uma foto"));
        assert_eq!(split("no recipe").1, None);
    }

    #[test]
    fn unknown_ids_are_refused() -> Result<()> {
        let cats = [Catalog {
            app: crate::apps::App::LightCraft,
            version: "1".into(),
            commands: vec![Cmd {
                id: "mask.add".into(),
                label: String::new(),
                params: String::new(),
            }],
        }];
        let ok =
            recipe::parse("[[stage]]\napp = \"lightcraft\"\nsteps = [{ cmd = \"mask.add\" }]\n")?;
        assert!(check(&ok, &cats).is_ok());
        let bad =
            recipe::parse("[[stage]]\napp = \"lightcraft\"\nsteps = [{ cmd = \"mask.magic\" }]\n")?;
        assert!(check(&bad, &cats).is_err());
        Ok(())
    }

    #[test]
    fn photos_are_described_by_kind() {
        let p: Vec<PathBuf> = ["a.JPG", "b.jpg", "c.cr3"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(describe(&p), "3 photo(s): 1 .cr3 (raw), 2 .jpg");
    }

    #[test]
    fn system_prompt_carries_playbook_and_lists() {
        let cats = [Catalog {
            app: crate::apps::App::PhotoCraft,
            version: "2".into(),
            commands: vec![],
        }];
        let s = system_prompt(&cats);
        assert!(
            s.contains("Recipe format")
                && s.contains("Kimera playbook")
                && s.contains("photocraft 2 commands")
        );
    }
}
