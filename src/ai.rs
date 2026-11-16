//! One question to the user's AI: Anthropic or OpenAI over HTTPS, or the `claude` / `codex`
//! CLIs signed in on this computer. Text and JPEG images in, text out.

use crate::config::{self, Provider};
use crate::procrun;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const ANTHROPIC_VERSION: &str = "2026-10-21";

/// Room for notes plus a recipe; a recipe is a few hundred tokens, the notes far less.
const MAX_TOKENS: u32 = 16_000;

/// How hard Claude thinks: recipe writing is routine, so `medium` (Opus 5.5's default).
const EFFORT: &str = "medium";

/// Largest reply body read from an AI service (a reply is text; anything bigger is an error page or abuse).
const MAX_REPLY_BYTES: u64 = 64 * 1024 * 1024;

/// A whole AI answer, thinking included, must arrive within this.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// `<base>/<path>`, the base from `env` (a proxy or gateway) or the official one.
fn endpoint(env: &str, official: &str, path: &str) -> String {
    let base = std::env::var(env)
        .ok()
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| official.to_owned());
    format!("{}/{path}", base.trim().trim_end_matches('/'))
}

#[derive(Debug, Clone)]
pub enum Part {
    Text(String),
    /// A JPEG file; its bytes are sent to API providers, its path to the CLIs.
    Jpeg(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Turn {
    pub fn user(parts: Vec<Part>) -> Self {
        Self {
            role: Role::User,
            parts,
        }
    }

    pub fn assistant(text: String) -> Self {
        Self {
            role: Role::Assistant,
            parts: vec![Part::Text(text)],
        }
    }
}

pub struct Client {
    pub cfg: config::Ai,
    /// Where the CLIs run and temporary prompt files go.
    pub scratch: PathBuf,
}

impl Client {
    pub fn new(cfg: config::Ai, scratch: PathBuf) -> Self {
        Self { cfg, scratch }
    }

    /// Sends `system` (stable, cached where the provider allows) and the conversation;
    /// returns the reply text.
    pub fn ask(&self, system: &str, turns: &[Turn], cancel: &AtomicBool) -> Result<String> {
        match self.cfg.provider {
            Provider::Anthropic => self.anthropic(system, turns),
            Provider::Openai => self.openai(system, turns),
            Provider::ClaudeCode => self.claude_cli(system, turns, cancel),
            Provider::Codex => self.codex_cli(system, turns, cancel),
        }
    }

    /// Checks that the provider can be reached at all: a key is set or the CLI is found.
    pub fn check(&self) -> Result<String> {
        match self.cfg.provider {
            Provider::Anthropic | Provider::Openai => {
                self.cfg.key()?;
                Ok(format!(
                    "{}, model {}",
                    self.cfg.provider.label(),
                    self.cfg.model()
                ))
            }
            Provider::ClaudeCode | Provider::Codex => {
                let exe = self.cli_path()?;
                Ok(format!(
                    "{} at {}",
                    self.cfg.provider.label(),
                    exe.display()
                ))
            }
        }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn anthropic(&self, system: &str, turns: &[Turn]) -> Result<String> {
        let key = self.cfg.key()?;
        let messages: Vec<Value> = turns
            .iter()
            .map(|t| {
                let content: Result<Vec<Value>> = t
                    .parts
                    .iter()
                    .map(|p| match p {
                        Part::Text(s) => Ok(json!({"type": "text", "text": s})),
                        Part::Jpeg(path) => Ok(json!({
                            "type": "image",
                            "source": {"type": "base64", "media_type": "image/jpeg", "data": b64(path)?}
                        })),
                    })
                    .collect();
                Ok(json!({"role": role(t.role), "content": content?}))
            })
            .collect::<Result<_>>()?;
        let mut body = json!({
            "model": self.cfg.model(),
            "max_tokens": MAX_TOKENS,
            "system": [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}],
            "messages": messages,
            "output_config": {"effort": EFFORT},
        });
        // Server-side fallback on a safety decline, for the models that support it.
        let fallback = matches!(
            self.cfg.model(),
            "claude-opus-5-5"
                | "claude-opus-5"
                | "claude-sonnet-5-5"
                | "claude-fable-5-2"
                | "claude-fable-5-1"
        );
        if fallback {
            body["fallbacks"] = json!("default");
        }
        let mut req = Self::agent()
            .post(&endpoint(
                "ANTHROPIC_BASE_URL",
                "https://api.anthropic.com",
                "v1/messages",
            ))
            .header("x-api-key", &key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");
        if fallback {
            req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        let (status, v) = send(req, &body)?;
        if status != 200 {
            bail!("Anthropic API {status}: {}", api_error(&v));
        }
        if v.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
            bail!("the model declined this request");
        }
        let text: String = v
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            bail!("the Anthropic API returned no text");
        }
        Ok(text)
    }

    fn openai(&self, system: &str, turns: &[Turn]) -> Result<String> {
        let key = self.cfg.key()?;
        let mut messages = vec![json!({"role": "system", "content": system})];
        for t in turns {
            let content: Vec<Value> = t
                .parts
                .iter()
                .map(|p| match p {
                    Part::Text(s) => Ok(json!({"type": "text", "text": s})),
                    Part::Jpeg(path) => Ok(json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:image/jpeg;base64,{}", b64(path)?)}
                    })),
                })
                .collect::<Result<_>>()?;
            messages.push(json!({"role": role(t.role), "content": content}));
        }
        let body = json!({"model": self.cfg.model(), "messages": messages});
        let req = Self::agent()
            .post(&endpoint(
                "OPENAI_BASE_URL",
                "https://api.openai.com/v1",
                "chat/completions",
            ))
            .header("authorization", &format!("Bearer {key}"))
            .header("content-type", "application/json");
        let (status, v) = send(req, &body)?;
        if status != 200 {
            bail!("OpenAI API {status}: {}", api_error(&v));
        }
        v.pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("the OpenAI API returned no text"))
    }

    fn cli_path(&self) -> Result<PathBuf> {
        if let Some(p) = &self.cfg.cli {
            return Ok(p.clone());
        }
        let names: &[&str] = match self.cfg.provider {
            Provider::ClaudeCode => &["claude.exe", "claude.cmd", "claude"],
            _ => &["codex.exe", "codex.cmd", "codex"],
        };
        find_on_path(names).ok_or_else(|| {
            anyhow!(
                "{} not found on PATH; install it or set [ai] cli in kimera.toml",
                names.last().copied().unwrap_or("")
            )
        })
    }

    /// The CLIs take one prompt: the system text, then the conversation, then image paths.
    fn flatten(system: &str, turns: &[Turn]) -> (String, Vec<PathBuf>) {
        let mut text = format!("{system}\n\n---\n\n");
        let mut images = Vec::new();
        for t in turns {
            text.push_str(match t.role {
                Role::User => "## Request\n\n",
                Role::Assistant => "## Your previous answer\n\n",
            });
            for p in &t.parts {
                match p {
                    Part::Text(s) => {
                        text.push_str(s);
                        text.push_str("\n\n");
                    }
                    Part::Jpeg(path) => {
                        let _ = write!(text, "[image: {}]\n\n", path.display());
                        images.push(path.clone());
                    }
                }
            }
        }
        (text, images)
    }

    fn claude_cli(&self, system: &str, turns: &[Turn], cancel: &AtomicBool) -> Result<String> {
        let (mut prompt, images) = Self::flatten(system, turns);
        let mut cmd = Command::new(self.cli_path()?);
        cmd.args(["-p", "--output-format", "text"]);
        if !self.cfg.model.is_empty() {
            cmd.args(["--model", &self.cfg.model]);
        }
        if !images.is_empty() {
            prompt.push_str("Read the image files named above before answering.\n");
            cmd.args(["--allowedTools", "Read"]);
            for d in images.iter().filter_map(|p| p.parent()) {
                cmd.arg("--add-dir").arg(d);
            }
        }
        std::fs::create_dir_all(&self.scratch)?;
        cmd.current_dir(&self.scratch);
        let o = procrun::run(cmd, Some(prompt), cancel)?;
        procrun::check(&o, "claude -p")?;
        nonempty(o.stdout, "claude")
    }

    fn codex_cli(&self, system: &str, turns: &[Turn], cancel: &AtomicBool) -> Result<String> {
        let (prompt, images) = Self::flatten(system, turns);
        std::fs::create_dir_all(&self.scratch)?;
        let out = self
            .scratch
            .join(format!("codex-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let mut cmd = Command::new(self.cli_path()?);
        cmd.args([
            "exec",
            "--skip-git-repo-check",
            "--ephemeral",
            "-s",
            "read-only",
            "-o",
        ])
        .arg(&out);
        if !self.cfg.model.is_empty() {
            cmd.args(["-m", &self.cfg.model]);
        }
        for i in &images {
            cmd.arg("-i").arg(i);
        }
        cmd.arg("-");
        cmd.current_dir(&self.scratch);
        let o = procrun::run(cmd, Some(prompt), cancel)?;
        procrun::check(&o, "codex exec")?;
        let text = std::fs::read_to_string(&out).unwrap_or(o.stdout);
        let _ = std::fs::remove_file(&out);
        nonempty(text, "codex")
    }
}

fn nonempty(s: String, who: &str) -> Result<String> {
    if s.trim().is_empty() {
        bail!("{who} returned no answer");
    }
    Ok(s)
}

fn role(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn b64(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn send(
    req: ureq::RequestBuilder<ureq::typestate::WithBody>,
    body: &Value,
) -> Result<(u16, Value)> {
    let mut resp = req
        .send_json(body)
        .context("could not reach the AI service")?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(MAX_REPLY_BYTES)
        .read_to_string()
        .context("reading the AI service's reply")?;
    let v = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Ok((status, v))
}

fn api_error(v: &Value) -> String {
    v.pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string().chars().take(500).collect())
}

/// First of `names` found in a PATH folder.
pub fn find_on_path(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .find_map(|dir| names.iter().map(|n| dir.join(n)).find(|p| p.is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_prompt_keeps_order_and_lists_images() {
        let (text, images) = Client::flatten(
            "SYSTEM",
            &[
                Turn::user(vec![Part::Text("do it".into()), Part::Jpeg("a.jpg".into())]),
                Turn::assistant("draft".into()),
                Turn::user(vec![Part::Text("fix".into())]),
            ],
        );
        let s = text.find("SYSTEM").unwrap_or(usize::MAX);
        let d = text.find("do it").unwrap_or(0);
        let f = text.find("fix").unwrap_or(0);
        assert!(s < d && d < f);
        assert!(text.contains("[image: a.jpg]"));
        assert_eq!(images, [PathBuf::from("a.jpg")]);
    }
}
