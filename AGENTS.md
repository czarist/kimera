# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, and others) working on Kimera. Read this
before changing code. [README.md](README.md) is for users; this file is for whoever edits the
code.

## What Kimera is

A Rust application with two programs over one library:

- `kimera` (`src/main.rs`): the command line.
- `kimera-gui` (`src/bin/kimera-gui.rs` → `src/gui/`): the egui window.

Both drive the Craft apps' own CLIs (`lightcraft-cli`, `photocraft-cli`, `vectorcraft-cli`)
headless, one photo and one app at a time, following a TOML recipe. An AI (Anthropic, OpenAI,
or the `claude` / `codex` CLIs) writes recipes from a request and turns per-photo notes into
steps. Kimera never changes the apps and never writes over a user's photo.

| Module | Job |
|---|---|
| `engine.rs` | The conveyor: plan (slots), shared stages then variants, optional groups, per-photo extras, `ensure_apps`, events. |
| `recipe.rs` | Recipe format (`[[stage]]`, `[[variant]]`, `optional`), validation, `check_step`. |
| `apps/` | `App` enum and one adapter per CLI; each reports *which step* failed (`StageFailed`). |
| `procrun.rs` | Runs a child process, kills it on cancel, hides consoles on Windows. |
| `catalog.rs` | The apps' command registries, read from the CLIs and cached per version. |
| `writer.rs` | Request → recipe: system prompt (contract + playbook + catalogs), checks, retries. |
| `notes.rs` | Circled regions on one photo → steps for that photo (`NotesJob`). |
| `analysis.rs` | The AI's look at each photo (closed eyes, red eyes) and LightCraft previews. |
| `ai.rs` | One `ask()` over the four providers. |
| `status.rs` | Health checks, optional LightCraft models (`Model`), installs. |
| `config.rs` | `kimera.toml`. |
| `prompts/playbook.md` | The AI's knowledge of what users ask, mapped to real commands. |

## Commands

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings   # CI fails on any warning
cargo test                                   # no Craft apps needed
cargo run -- doctor                          # CLI against ./kimera.toml
cargo run --bin kimera-gui -- <photo folder> # the window, opened on a folder
```

`kimera.toml` is git-ignored; copy `kimera.toml.example`. `scripts/package.ps1` builds the
Windows zip.

The build folder may not be `target/`: a git-ignored `.cargo/config.toml` can move it to
another drive (`[build] target-dir = "F:/cargo-target/kimera"`). Scripts ask Cargo for it
(`cargo metadata --format-version 1 --no-deps` → `target_directory`); never hard-code
`target/`.

## Rules

- **No `unwrap`, `expect`, `panic!`, `todo!`, `unreachable!` or `unsafe`** outside tests. The lints
  live in `Cargo.toml` (`[lints]`) only; don't add crate-level `#![deny]` / `#![forbid]`. This is
  stricter than the `rust-best-practices` skill, which allows `expect("why")` and treats lock
  poisoning as fatal: here a poisoned lock or an impossible state becomes an error or a skipped
  item, never a panic.
- **Fix lints, don't silence them.** No `#[allow]`; if a lint is truly wrong for one item, use
  `#[expect(clippy::name, reason = "…")]` on that item.
- **Application error style:** `anyhow` with `.context(..)`; messages name the file, step or
  value that failed. Typed errors only where code branches on them (`StageFailed`, `Cancelled`).
- **Types over strings:** apps are `apps::App`, models `status::Model`. The lowercase ids
  (`lightcraft`, `sam3`) appear only in recipes, folders, logs and CLI arguments.
- **More than four arguments → a struct** (`StageJob`, `NotesJob`, `AskArgs`).
- **Named constants** for sizes, limits and timeouts, with a doc comment saying why.
- **Deterministic output:** sort before printing or saving anything that came from a `HashMap`.
- **Non-destructive, always.** A stage's output never equals its input; originals are only read;
  each run gets a new folder; notes are stored in the work folder, never beside the photos.
- **Recipes from an AI are untrusted:** keep `recipe::validate`, `check_step`, the catalog check
  in `writer::check`, and the reserved export options in place. Never pass an AI string to a
  shell.
- **Prompts and previews go only to the configured AI provider.** Don't add telemetry or other
  network calls.

## How the Craft CLIs behave

Verified against lightcraft-cli 0.21, photocraft-cli 0.23, vectorcraft-cli 0.24. Keep the
adapters in line with these facts:

- `lightcraft-cli run --import <file> <cmd> <json> … app.export <json>` works with no app open,
  in a throwaway session. One JSON line per command (`{"command","ok","result"|"error"}`); it
  stops at the first failure, exits non-zero, and exports nothing then. `run --demo` gives a
  procedural library, used for queries and test photos.
- `photocraft-cli run <file> --cmd <id> --params <json> … --out <file>` prints one JSON line per
  successful command and the error on stderr. It cannot open raw files. `--out` equal to the
  input is refused.
- `vectorcraft-cli run --in <file> --cmd … --export <file>`: errors read `step N (id): message`,
  counted from 0.
- LightCraft coordinates are normalized `[x, y]` from the top-left; PhotoCraft's are pixels.
- `mask.add` with an AI kind (`subject`, `background`, `sky`, `object`, `prompt`) needs SAM 3 and
  fails when it finds nothing; each run reloads SAM 3 (about a minute per photo for prompt masks).
  `spot.findBlemishes` needs the YuNet face detector. Steps like these belong in `optional`
  groups.
- `lightcraft-cli render <in> -o <out> --size N` makes a preview of any format, raw included.

## Verifying a change

1. `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must be clean.
2. If the change touches the conveyor, an adapter or a recipe, run it against the real apps:
   - test photos: `lightcraft-cli run --demo app.export '{"ids":[1,2,3,4,5,6],"dir":"<dir>","format":"jpeg","longEdge":1600}'`;
   - `kimera check recipes/*.toml`, then `kimera run --recipe … <dir>`, and look at the output files
     (render a small preview and open it).
3. If it touches the window, open `kimera-gui <dir>` and check it on screen (a screenshot is
   enough). Say what was not verified.
4. AI paths cost money or quota: one real call to prove a change is fine; don't loop on them.

## Skills

Installed under `.agents/skills/` (git-ignored, pinned in `skills-lock.json`):

| Skill | Use it for |
|---|---|
| `rust-best-practices` | Writing or refactoring Rust here (with the stricter panic rules above). |
| `rust-review` | Reviewing a diff or module; report before rewriting. |
| `github-actions-hardening` | Any change to `.github/workflows/`: permissions, SHA-pinned actions, injection. |
| `github-actions-efficiency` | CI cost and speed: caching, concurrency, path filters. |
| `rust-port` | Porting code from another language into Rust. |
| `ui-ux-pro-max` | General UX judgement for the window (contrast, states, feedback). Its web stacks don't apply. |
| `ui-styling`, `design`, `design-system`, `brand`, `banner-design`, `slides` | Web and marketing assets only; not for the egui code. |

## Conventions

- **Language:** code, comments, docs, prompts and commit messages in English. Brazilian
  Portuguese only in `README.pt-BR.md` and the bilingual vocabulary table of the playbook.
- **Commits:** short imperative subject, a body that says why. No AI co-author or "generated
  with" lines.
- **Never commit** `kimera.toml`, `dist/`, `target/` or `.agents/skills/`.
- **Changelog:** add user-visible changes to `CHANGELOG.md`; keep `ROADMAP.md` honest about what
  is verified.
- **Playbook:** every command id in backticks must exist in the apps' catalogs (the AI gets full
  parameters for those ids). Mark features Kimera lacks with **[kimera: …]**.
