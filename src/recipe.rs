//! A recipe: the stages each photo goes through, in order. Every stage is one Craft app run
//! headless on one file; its output is the next stage's input.
//!
//! ```toml
//! [[stage]]
//! app = "photocraft"          # clean-up, pixel work
//! out_ext = "tif"
//! steps = [{ cmd = "select.subject" }]
//!
//! [[stage]]
//! app = "lightcraft"          # masks, colour
//! steps = [{ cmd = "mask.add" }]
//! export = { format = "jpg", quality = 92 }
//! ```

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Recipe {
    /// Only for `kimera pipeline`: how photos leave the running LightCraft's catalog first.
    #[serde(default)]
    pub export: Export,
    #[serde(rename = "stage", default)]
    pub stages: Vec<Stage>,
    /// Several results per photo: each variant runs its own stages after the shared ones
    /// (colour and B&W, feed and stories…).
    #[serde(rename = "variant", default)]
    pub variants: Vec<Variant>,
}

#[derive(Debug, Deserialize)]
pub struct Variant {
    /// Folder-safe name: `feed`, `stories`, `bw`.
    pub name: String,
    #[serde(rename = "stage", default)]
    pub stages: Vec<Stage>,
}

impl Recipe {
    /// Every full chain a photo goes through: `(variant name, shared stages + its own)`.
    /// Without variants there is one unnamed chain.
    pub fn chains(&self) -> Vec<(Option<&str>, Vec<&Stage>)> {
        if self.variants.is_empty() {
            return vec![(None, self.stages.iter().collect())];
        }
        self.variants
            .iter()
            .map(|v| {
                (
                    Some(v.name.as_str()),
                    self.stages.iter().chain(&v.stages).collect(),
                )
            })
            .collect()
    }

    /// Every non-optional step with its app and its `kind` parameter (masks), for model checks
    /// (an optional step that fails is left out, so it never stops a photo).
    pub fn steps_with_kind(&self) -> Vec<(&'static str, &str, Option<String>)> {
        self.stages
            .iter()
            .chain(self.variants.iter().flat_map(|v| &v.stages))
            .flat_map(|s| {
                s.steps()
                    .iter()
                    .filter(|st| st.optional.is_none())
                    .map(move |st| {
                        let kind = st
                            .params
                            .as_ref()
                            .and_then(|p| p.get("kind"))
                            .and_then(|k| k.as_str())
                            .map(str::to_owned);
                        (s.app(), st.cmd.as_str(), kind)
                    })
            })
            .collect()
    }

    /// Every command id the recipe uses, with its app.
    pub fn commands(&self) -> Vec<(&'static str, &str)> {
        self.stages
            .iter()
            .chain(self.variants.iter().flat_map(|v| &v.stages))
            .flat_map(|s| s.steps().iter().map(move |st| (s.app(), st.cmd.as_str())))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "app", rename_all = "lowercase")]
pub enum Stage {
    Photocraft(PhotoCraftStage),
    Lightcraft(LightCraftStage),
    Vectorcraft(VectorCraftStage),
}

impl Stage {
    pub fn app(&self) -> &'static str {
        match self {
            Stage::Photocraft(_) => "photocraft",
            Stage::Lightcraft(_) => "lightcraft",
            Stage::Vectorcraft(_) => "vectorcraft",
        }
    }

    /// Extension of the file this stage writes.
    pub fn out_ext(&self) -> &str {
        match self {
            Stage::Photocraft(s) => &s.out_ext,
            Stage::Lightcraft(s) => &s.export.format,
            Stage::Vectorcraft(s) => &s.out_ext,
        }
    }

    pub fn steps(&self) -> &[Step] {
        match self {
            Stage::Photocraft(s) => &s.steps,
            Stage::Lightcraft(s) => &s.steps,
            Stage::Vectorcraft(s) => &s.steps,
        }
    }
}

/// Catalog export of a running LightCraft (`app.export` over the control channel).
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Export {
    /// `tif`, `png`, `jpg`…: what LightCraft's `app.export` writes.
    pub format: String,
    pub bit_depth: u8,
    /// 0 = full size (cropped, native resolution).
    pub long_edge: u32,
    pub color_space: Option<String>,
}

impl Default for Export {
    fn default() -> Self {
        Self {
            format: "tif".into(),
            bit_depth: 16,
            long_edge: 0,
            color_space: None,
        }
    }
}

impl Export {
    /// Params for LightCraft's `app.export` (see its docs/mcp.md).
    pub fn params(&self, ids: &[u64], dir: &Path) -> Value {
        let mut p = Map::new();
        p.insert("ids".into(), json!(ids));
        p.insert("dir".into(), json!(dir));
        p.insert("format".into(), json!(self.format));
        p.insert("bitDepth".into(), json!(self.bit_depth));
        p.insert("longEdge".into(), json!(self.long_edge));
        if let Some(cs) = &self.color_space {
            p.insert("colorSpace".into(), json!(cs));
        }
        Value::Object(p)
    }
}

#[derive(Debug, Deserialize)]
pub struct PhotoCraftStage {
    /// Extension of the saved result: `tif`/`png`/`jpg` flatten, `pcraft` keeps layers
    /// (only as the last stage: LightCraft cannot read it).
    #[serde(default = "default_pc_ext")]
    pub out_ext: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// A layout stage: the photo (or a template placing it through `{photo}`) exported as
/// `out_ext` (`pdf`, `png`, `jpg`, `svg`…).
#[derive(Debug, Deserialize)]
pub struct VectorCraftStage {
    #[serde(default)]
    pub template: Option<PathBuf>,
    #[serde(default = "default_vc_ext")]
    pub out_ext: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

fn default_vc_ext() -> String {
    "pdf".into()
}

#[derive(Debug, Deserialize)]
pub struct LightCraftStage {
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub export: FileExport,
}

/// `app.export` of one headless LightCraft file; the format follows the extension.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct FileExport {
    /// `jpg`, `tif`, `png`, `webp`, `avif`, `jxl`, `psd` or `dng`.
    pub format: String,
    /// 0 = full size.
    pub long_edge: u32,
    pub bit_depth: Option<u8>,
    pub quality: Option<u8>,
    pub color_space: Option<String>,
    /// Any other `app.export` option, under LightCraft's own name: `watermark`, `limitKb`,
    /// `sharpen`, `metadata`, `removeLocation`… (not the ones that pick where files go).
    pub options: toml::Table,
}

/// `app.export` options Kimera owns: where the file goes and how it is named.
const RESERVED_EXPORT_KEYS: &[&str] = &[
    "path",
    "dir",
    "ids",
    "format",
    "destinationMode",
    "subfolder",
    "naming",
    "startNumber",
    "conflict",
    "addToPhotos",
    "photosAlbum",
    "longEdge",
    "bitDepth",
    "quality",
    "colorSpace",
];

impl Default for FileExport {
    fn default() -> Self {
        Self {
            format: "jpg".into(),
            long_edge: 0,
            bit_depth: None,
            quality: None,
            color_space: None,
            options: toml::Table::new(),
        }
    }
}

impl FileExport {
    pub fn params(&self, out: &Path) -> Result<Value> {
        let path = out
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("{} is not valid UTF-8", out.display()))?;
        let mut p = Map::new();
        for (k, v) in &self.options {
            if RESERVED_EXPORT_KEYS.contains(&k.as_str()) {
                bail!("export option {k:?} is set by Kimera");
            }
            p.insert(k.clone(), serde_json::to_value(v)?);
        }
        p.insert("path".into(), json!(path));
        p.insert("longEdge".into(), json!(self.long_edge));
        if let Some(b) = self.bit_depth {
            p.insert("bitDepth".into(), json!(b));
        }
        if let Some(q) = self.quality {
            p.insert("quality".into(), json!(q));
        }
        if let Some(cs) = &self.color_space {
            p.insert("colorSpace".into(), json!(cs));
        }
        Ok(Value::Object(p))
    }
}

#[derive(Debug, Deserialize)]
pub struct Step {
    /// An engine command id of the stage's app (`<app>-cli commands`).
    pub cmd: String,
    #[serde(default)]
    pub params: Option<toml::Value>,
    /// Group name: when a step of the group fails (a prompt mask that finds nothing, no
    /// subject…), every step of that group is skipped for that photo and the stage runs again.
    #[serde(default)]
    pub optional: Option<String>,
}

impl Step {
    pub fn params_json(&self) -> Result<String> {
        match &self.params {
            None => Ok("{}".into()),
            Some(v) => Ok(serde_json::to_string(v)?),
        }
    }
}

fn default_pc_ext() -> String {
    "tif".into()
}

pub fn load(path: &Path) -> Result<Recipe> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("in {}", path.display()))
}

pub fn parse(text: &str) -> Result<Recipe> {
    let r: Recipe = toml::from_str(text)?;
    validate(&r)?;
    Ok(r)
}

/// Recipes may come from an AI: refuse anything that could escape a folder or a command line.
fn validate(r: &Recipe) -> Result<()> {
    if r.stages.is_empty() && r.variants.is_empty() {
        bail!("the recipe has no [[stage]]");
    }
    let mut names = std::collections::HashSet::new();
    for v in &r.variants {
        if v.name.is_empty()
            || v.name.len() > 40
            || !v
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("variant name {:?} must be letters, digits, - or _", v.name);
        }
        if !names.insert(v.name.to_lowercase()) {
            bail!("variant {:?} appears twice", v.name);
        }
        if v.stages.is_empty() && r.stages.is_empty() {
            bail!("variant {:?} has no [[variant.stage]]", v.name);
        }
    }
    for (name, chain) in r.chains() {
        let at = |n: usize| match name {
            Some(v) => format!("variant {v}, stage {n}"),
            None => format!("stage {n}"),
        };
        let last = chain.len().saturating_sub(1);
        for (i, s) in chain.iter().enumerate() {
            let n = i.saturating_add(1);
            let ext = s.out_ext();
            if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                bail!(
                    "{}: output extension {ext:?} must be bare, such as \"tif\" or \"jpg\"",
                    at(n)
                );
            }
            if let Stage::Lightcraft(l) = s
                && let Some(k) = l
                    .export
                    .options
                    .keys()
                    .find(|k| RESERVED_EXPORT_KEYS.contains(&k.as_str()))
            {
                bail!(
                    "{}: export option {k:?} is set by Kimera; use the export fields",
                    at(n)
                );
            }
            if i < last && ext.eq_ignore_ascii_case("pcraft") {
                bail!(
                    "{}: .pcraft can only be the last stage's output; use \"tif\"",
                    at(n)
                );
            }
            for st in s.steps() {
                if !is_command_id(&st.cmd) {
                    bail!("{}: {:?} is not a command id", at(n), st.cmd);
                }
                if let Some(p) = &st.params
                    && !p.is_table()
                {
                    bail!("{}: params of {} must be a table", at(n), st.cmd);
                }
            }
        }
    }
    Ok(())
}

/// `select.subject`, `mask.add`, `layer.newAdjustmentLayer.selectiveColor`…
fn is_command_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_recipes_parse() -> Result<()> {
        for text in [
            include_str!("../recipes/subject-mask.toml"),
            include_str!("../recipes/cleanup-cinematic.toml"),
        ] {
            let r = parse(text)?;
            assert!(!r.stages.is_empty());
        }
        Ok(())
    }

    #[test]
    fn stages_keep_their_order_and_apps() -> Result<()> {
        let r = parse(
            "[[stage]]\napp = \"photocraft\"\n[[stage]]\napp = \"lightcraft\"\nexport = { format = \"tif\", bit_depth = 16 }\n",
        )?;
        let apps: Vec<_> = r.stages.iter().map(Stage::app).collect();
        assert_eq!(apps, ["photocraft", "lightcraft"]);
        assert_eq!(r.stages[0].out_ext(), "tif");
        assert_eq!(r.stages[1].out_ext(), "tif");
        Ok(())
    }

    #[test]
    fn export_params_use_lightcraft_names() {
        let p = Export::default().params(&[7, 9], Path::new("out"));
        assert_eq!(p["ids"], json!([7, 9]));
        assert_eq!(p["bitDepth"], json!(16));
        assert_eq!(p["longEdge"], json!(0));
        assert!(p.get("colorSpace").is_none());
    }

    #[test]
    fn file_export_params() -> Result<()> {
        let r = parse(
            "[[stage]]\napp = \"lightcraft\"\nexport = { quality = 90, options = { limitKb = 500, watermark = { text = \"(c) M\", opacity = 0.6 } } }\n",
        )?;
        let Stage::Lightcraft(l) = &r.stages[0] else {
            bail!("expected a LightCraft stage");
        };
        let p = l.export.params(Path::new("o/a.jpg"))?;
        assert_eq!(p["path"], json!("o/a.jpg"));
        assert_eq!(p["quality"], json!(90));
        assert_eq!(p["limitKb"], json!(500));
        assert_eq!(p["watermark"]["text"], json!("(c) M"));
        assert!(p.get("bitDepth").is_none());
        Ok(())
    }

    #[test]
    fn step_params_become_json() -> Result<()> {
        let r = parse(
            "[[stage]]\napp = \"photocraft\"\nsteps = [{ cmd = \"a\", params = { x = 1 } }, { cmd = \"b\" }]\n",
        )?;
        let steps = r.stages[0].steps();
        assert_eq!(steps[0].params_json()?, r#"{"x":1}"#);
        assert_eq!(steps[1].params_json()?, "{}");
        Ok(())
    }

    #[test]
    fn variants_make_one_chain_each() -> Result<()> {
        let r = parse(
            "[[stage]]
app = \"photocraft\"
[[variant]]
name = \"feed\"
[[variant.stage]]
app = \"lightcraft\"
[[variant]]
name = \"stories\"
[[variant.stage]]
app = \"lightcraft\"
steps = [{ cmd = \"mask.add\", optional = \"teeth\" }]
",
        )?;
        let c = r.chains();
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].0, Some("stories"));
        assert_eq!(c[1].1.len(), 2);
        assert_eq!(r.commands(), [("lightcraft", "mask.add")]);
        assert_eq!(
            r.variants[1].stages[0].steps()[0].optional.as_deref(),
            Some("teeth")
        );
        assert!(
            parse(
                "[[variant]]
name = \"a/b\"
[[variant.stage]]
app = \"lightcraft\"
"
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn unsafe_recipes_are_refused() {
        for bad in [
            "",
            "[[stage]]\napp = \"pdfcraft\"\n",
            "[[stage]]\napp = \"photocraft\"\nout_ext = \"../x\"\n",
            "[[stage]]\napp = \"lightcraft\"\nexport = { format = \"j.pg\" }\n",
            "[[stage]]\napp = \"photocraft\"\nsteps = [{ cmd = \"a=b\" }]\n",
            "[[stage]]\napp = \"photocraft\"\nsteps = [{ cmd = \"--out\" }]\n",
            "[[stage]]\napp = \"photocraft\"\nsteps = [{ cmd = \"a\", params = 3 }]\n",
            "[[stage]]\napp = \"photocraft\"\nout_ext = \"pcraft\"\n[[stage]]\napp = \"lightcraft\"\n",
            "[[stage]]\napp = \"lightcraft\"\nexport = { options = { path = \"C:/x.jpg\" } }\n",
            "[[stage]]\napp = \"lightcraft\"\nexport = { options = { conflict = \"overwrite\" } }\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
