//! `kimera.toml`: where the Craft apps live, where runs are written, and which AI writes recipes.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub lightcraft: LightCraft,
    pub photocraft: PhotoCraft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vectorcraft: Option<VectorCraft>,
    pub work: Work,
    #[serde(default)]
    pub ai: Ai,
    #[serde(default)]
    pub startup: Startup,
}

/// What Kimera does when it starts.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Startup {
    /// Open the Craft apps' windows (LightCraft with its control channel) if they are not open.
    #[serde(default = "yes")]
    pub open_apps: bool,
    /// How their windows open.
    #[serde(default)]
    pub window: WindowStyle,
}

impl Default for Startup {
    fn default() -> Self {
        Self {
            open_apps: true,
            window: WindowStyle::default(),
        }
    }
}

/// How the Craft apps' windows open when Kimera starts them (Windows; elsewhere always normal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowStyle {
    /// Running in the background, with no window and no taskbar button.
    #[default]
    Hidden,
    /// In the taskbar, minimized.
    Minimized,
    Normal,
}

impl WindowStyle {
    pub const ALL: [Self; 3] = [Self::Hidden, Self::Minimized, Self::Normal];

    pub fn label(self) -> &'static str {
        match self {
            Self::Hidden => "in the background (hidden)",
            Self::Minimized => "minimized",
            Self::Normal => "normal windows",
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LightCraft {
    /// `lightcraft-cli` executable.
    pub cli: PathBuf,
    /// Control channel of a running `lightcraft --control <port>` (only `pipeline` needs it).
    #[serde(default = "default_lc_control")]
    pub control: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PhotoCraft {
    /// `photocraft-cli` executable.
    pub cli: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VectorCraft {
    /// `vectorcraft-cli` executable.
    pub cli: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Work {
    /// Each run gets its own folder in here.
    pub dir: PathBuf,
}

/// Which AI turns a request into a recipe and looks at photos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    /// Anthropic API (key in `ANTHROPIC_API_KEY` by default).
    #[default]
    Anthropic,
    /// OpenAI API (key in `OPENAI_API_KEY` by default).
    Openai,
    /// The `claude` CLI (Claude Code), signed in on this computer.
    ClaudeCode,
    /// The `codex` CLI, signed in on this computer.
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 4] = [
        Provider::Anthropic,
        Provider::Openai,
        Provider::ClaudeCode,
        Provider::Codex,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic API",
            Provider::Openai => "OpenAI API",
            Provider::ClaudeCode => "Claude Code (claude CLI)",
            Provider::Codex => "Codex (codex CLI)",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-opus-5-5",
            Provider::Openai => "gpt-5",
            Provider::ClaudeCode | Provider::Codex => "",
        }
    }

    pub fn default_key_env(self) -> &'static str {
        match self {
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Openai => "OPENAI_API_KEY",
            Provider::ClaudeCode | Provider::Codex => "",
        }
    }

    pub fn uses_api_key(self) -> bool {
        matches!(self, Provider::Anthropic | Provider::Openai)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Ai {
    pub provider: Provider,
    /// Empty: the provider's default (`claude-opus-5-5` for Anthropic).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Environment variable holding the API key; empty: the provider's usual one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_key_env: String,
    /// The key itself, when an environment variable is not practical. Stored in plain text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// `claude` / `codex` executable when not on PATH.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<PathBuf>,
}

impl Ai {
    pub fn model(&self) -> &str {
        if self.model.is_empty() {
            self.provider.default_model()
        } else {
            &self.model
        }
    }

    pub fn key(&self) -> Result<String> {
        if !self.api_key.is_empty() {
            return Ok(self.api_key.clone());
        }
        let var = if self.api_key_env.is_empty() {
            self.provider.default_key_env()
        } else {
            &self.api_key_env
        };
        match std::env::var(var) {
            Ok(k) if !k.trim().is_empty() => Ok(k.trim().to_owned()),
            _ => bail!(
                "no API key for {}: set {var}, or [ai] api_key in kimera.toml",
                self.provider.label()
            ),
        }
    }
}

impl Default for Config {
    /// Usual install folders; the window's setup screen fixes what does not match.
    fn default() -> Self {
        let pf = |app: &str, exe: &str| PathBuf::from(format!(r"C:\Program Files\{app}\{exe}"));
        Self {
            lightcraft: LightCraft {
                cli: pf("LightCraft", "lightcraft-cli.exe"),
                control: default_lc_control(),
            },
            photocraft: PhotoCraft {
                cli: pf("PhotoCraft", "photocraft-cli.exe"),
            },
            vectorcraft: None,
            work: Work {
                dir: dirs_pictures().join("Kimera"),
            },
            ai: Ai::default(),
            startup: Startup::default(),
        }
    }
}

fn dirs_pictures() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join("Pictures"))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_lc_control() -> String {
    "127.0.0.1:7980".to_owned()
}

/// `--config`, else `./kimera.toml`, else `kimera.toml` next to the executable.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let here = PathBuf::from("kimera.toml");
    if here.is_file() {
        return Ok(here);
    }
    let beside = beside_exe();
    if beside.is_file() {
        return Ok(beside);
    }
    bail!(
        "no kimera.toml found: copy kimera.toml.example to kimera.toml (here or next to kimera.exe), or pass --config"
    )
}

/// `kimera.toml` next to the executable: where the window saves its settings.
pub fn beside_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map(|d| d.join("kimera.toml"))
        .unwrap_or_else(|| PathBuf::from("kimera.toml"))
}

pub fn load(path: &Path) -> Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("in {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    Ok(toml::from_str(text)?)
}

pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    let text = format!(
        "# Kimera settings (written by the Kimera window; safe to edit by hand).\n{}",
        toml::to_string_pretty(cfg)?
    );
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() -> Result<()> {
        let c = parse(include_str!("../kimera.toml.example"))?;
        assert_eq!(c.lightcraft.control, "127.0.0.1:7980");
        assert!(c.vectorcraft.is_some());
        assert_eq!(c.ai.provider, Provider::Anthropic);
        assert_eq!(c.ai.model(), "claude-opus-5-5");
        Ok(())
    }

    #[test]
    fn saved_config_reads_back() -> Result<()> {
        let mut c = Config::default();
        c.ai.provider = Provider::Codex;
        let text = toml::to_string_pretty(&c)?;
        let back = parse(&text)?;
        assert_eq!(back.ai.provider, Provider::Codex);
        assert_eq!(back.photocraft.cli, c.photocraft.cli);
        assert!(text.contains("provider = \"codex\""));
        Ok(())
    }

    #[test]
    fn missing_section_is_an_error_not_a_panic() {
        assert!(parse("[lightcraft]\ncli = 'x'\n").is_err());
    }
}
