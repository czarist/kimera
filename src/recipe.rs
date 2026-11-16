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
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Recipe {
    /// Only for `kimera pipeline`: how photos leave the running LightCraft's catalog first.
    #[serde(default)]
    pub export: Export,
    #[serde(rename = "stage", default)]
    pub stages: Vec<Stage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "app", rename_all = "lowercase")]
pub enum Stage {
    Photocraft(PhotoCraftStage),
    Lightcraft(LightCraftStage),
}

impl Stage {
    pub fn app(&self) -> &'static str {
        match self {
            Stage::Photocraft(_) => "photocraft",
            Stage::Lightcraft(_) => "lightcraft",
        }
    }

    /// Extension of the file this stage writes.
    pub fn out_ext(&self) -> &str {
        match self {
            Stage::Photocraft(s) => &s.out_ext,
            Stage::Lightcraft(s) => &s.export.format,
        }
    }

    pub fn steps(&self) -> &[Step] {
        match self {
            Stage::Photocraft(s) => &s.steps,
            Stage::Lightcraft(s) => &s.steps,
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
    if r.stages.is_empty() {
        bail!("the recipe has no [[stage]]");
    }
    let last = r.stages.len().saturating_sub(1);
    for (i, s) in r.stages.iter().enumerate() {
        let n = i.saturating_add(1);
        let ext = s.out_ext();
        if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            bail!("stage {n}: output extension {ext:?} must be bare, such as \"tif\" or \"jpg\"");
        }
        if let Stage::Lightcraft(l) = s
            && let Some(k) = l
                .export
                .options
                .keys()
                .find(|k| RESERVED_EXPORT_KEYS.contains(&k.as_str()))
        {
            bail!("stage {n}: export option {k:?} is set by Kimera; use the export fields");
        }
        if i < last && ext.eq_ignore_ascii_case("pcraft") {
            bail!("stage {n}: .pcraft can only be the last stage's output; use \"tif\"");
        }
        for st in s.steps() {
            if !is_command_id(&st.cmd) {
                bail!("stage {n}: {:?} is not a command id", st.cmd);
            }
            if let Some(p) = &st.params
                && !p.is_table()
            {
                bail!("stage {n}: params of {} must be a table", st.cmd);
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
    fn unsafe_recipes_are_refused() {
        for bad in [
            "",
            "[[stage]]\napp = \"vectorcraft\"\n",
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
