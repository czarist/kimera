# Changelog

All notable changes to Kimera are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Notes on a photo**: circle part of a photo in the window (*Notes…*, or double-click) and say
  what to do there; `--notes FILE` on `kimera run` and `kimera ask`. The AI sees the photo with the
  loops drawn and returns steps for that photo only, always in optional groups. Notes are kept
  per folder in the work folder. *Test on 1 photo* picks the photo with notes.
- A run turns LightCraft or PhotoCraft on by itself when per-photo steps need an app the recipe
  does not use.
- The window shows which apps a recipe turns on (LightCraft / PhotoCraft / VectorCraft on or off).
- `AGENTS.md`: guidance for AI coding agents (commands, rules, Craft CLI behaviour, verification,
  which installed skills to use).
- `.github/dependabot.yml` for actions and crates.
- Kimera starts LightCraft (with its control channel), PhotoCraft and VectorCraft when it
  starts, if they are not running: in the background by default, or minimized, or as normal
  windows (`[startup]` in `kimera.toml`, Setup in the window). `--no-open` skips it once; the
  window's Setup has an *Open apps* button.
- An icon (`assets/kimera.svg`): a K over a conveyor carrying three photos, one per Craft app.
  It is the window and taskbar icon and heads both READMEs.

### Fixed

- `scripts/package.ps1` finds the build wherever Cargo puts it (a local `.cargo/config.toml`
  may move the target folder to another drive).

### Changed

- `cargo run` opens the window; the command line is `cargo run --bin kimera -- …`.

- The recipe writer turns on only the apps a request needs, and refuses PhotoCraft stages without
  steps; `cleanup-cinematic` is LightCraft only.
- README (English and Brazilian Portuguese) in the project's showcase style.
- CI: read-only token by default, actions pinned to commit SHAs, no persisted credentials,
  `cargo fmt --check`, Rust caching, cancelled superseded runs, docs-only changes skipped.
- Code (no behaviour change): apps and LightCraft models are enums instead of strings;
  long argument lists became `StageJob`, `NotesJob` and `AskArgs`; sizes and limits are named
  constants; lints are configured only in `Cargo.toml`; the notes file is sorted.

## [0.2.0] - 2026-11-16

### Added

- **The window (`kimera-gui`)**: Setup (app paths, work folder, AI provider, checks, model
  installs), a folder of photos with thumbnails (raw files rendered by LightCraft) and
  one / some / all selection, a request box that asks the AI for a recipe, an editable recipe
  checked as you type, *Test on 1 photo* with before / after, the conveyor board (queue, each
  stage, done, failed) with a live log and *Cancel*, and a client proof button. It takes a folder
  as argument, for "Open with" and the installer's folder menu.
- **AI recipes** (`kimera ask`): Anthropic or OpenAI APIs (prompt caching on Anthropic, base URLs
  from `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL`), or the signed-in `claude` / `codex` CLIs. The AI
  gets `prompts/playbook.md`, the apps' real command lists (read from the CLIs and cached per
  version) and which LightCraft models are installed; Kimera refuses unknown command ids and
  sends errors back for up to two corrections.
- **Per-photo AI look** (`kimera analyse`, `--analyse`, the window's checkbox): faces, closed
  eyes, red eyes, subject box. Red eyes become `redeye.add` steps for that photo; closed eyes are
  flagged for review with nearby frames of the same people as swap donors.
- **Variants**: `[[variant]]` tables give several results per photo (feed / stories / square,
  colour / B&W) after shared stages.
- **Optional step groups**: `optional = "<group>"` leaves a group out for a photo when one of its
  steps fails there (a prompt mask that finds nothing, a missing model), and runs the stage again.
- **VectorCraft stages**: layouts and posters through `vectorcraft-cli run`, with a `template`
  and `{photo}` placeholders.
- `kimera check`: recipes against the apps' real commands and the installed models.
- `kimera proof`: client proof, A4 contact sheets as PDF through PhotoCraft.
- `kimera models install sam3|yunet|lensdb --accept`: installs LightCraft models through the
  running app; `kimera doctor` reports them, and runs stop early when a required step needs a
  missing model.
- `kimera pipeline --reimport`: imports the results into LightCraft, stacked on their originals,
  even when other photos of the run failed; `kimera unimport <run folder>` undoes it (the imported
  photos go to Recently Deleted, the files stay).
- `kimera recipes` and seven new recipes: portrait retouch, social formats, marketplace white
  background, black and white, web gallery, print delivery, real estate.
- `[ai]` settings in `kimera.toml`; the window writes `kimera.toml` next to the program.
- Packaging: `scripts/package.ps1` (portable zip), `installer/kimera.iss` (Inno Setup, with an
  "Open in Kimera" folder menu), and a GitHub workflow that tests on Windows, macOS and Linux and
  attaches builds to `v*` releases.
- `kimera run`: the photographer's conveyor. Every image of a folder (or the files given) goes
  through the recipe's stages in order, each stage writing into its own folder of the run.
- LightCraft stages run headless (`lightcraft-cli run --import <file> … app.export`).
- `export.options` on LightCraft stages: any other `app.export` option (watermark, `limitKb`,
  output sharpening, metadata, location removal…); options that choose where files go are refused.
- `prompts/playbook.md`: what photographers, designers and marketers ask, mapped to real
  commands, with the limits and lines not to cross.
- `ROADMAP.md`, this changelog, and a Brazilian Portuguese README.

### Changed

- Kimera is now a library with two programs: `kimera` (command line) and `kimera-gui` (window).
- Cancelling stops the running app at once instead of waiting for the stage to end; the Craft
  CLIs no longer flash console windows when the window starts them.
- Recipes are a list of `[[stage]]` tables (`app = "photocraft" | "lightcraft" | "vectorcraft"`)
  instead of one `[photocraft]` table; `[export]` is only for `kimera pipeline`. PhotoCraft stages
  default to `out_ext = "tif"`, and raw files are refused by PhotoCraft stages with a clear message.
- `kimera pipeline` exports into `0-export/` and then runs all the recipe's stages.
- `kimera doctor` checks the AI and the optional models, and no longer fails when LightCraft's
  control port is closed.

### Removed

- `kimera photocraft`: use `kimera run` with a PhotoCraft-only recipe.

## [0.1.0] - 2026-11-16

### Added

- `kimera doctor`: finds LightCraft, PhotoCraft and VectorCraft, prints their versions, and checks
  that LightCraft answers on its control port.
- `kimera pipeline --recipe <file> [--ids …]`: exports photos from a running LightCraft (`app.export`
  over the control channel) and runs the recipe's PhotoCraft commands on each exported file.
- `kimera photocraft --recipe <file> <files…>`: runs only the PhotoCraft part of a recipe.
- `kimera.toml.example` with the default install paths, and the `recipes/subject-mask.toml` recipe
  (16-bit TIFF export, then PhotoCraft's Select Subject).
- Each run writes to its own `run-<time>` folder; a PhotoCraft output never replaces its input.
- LightCraft's control channel is used on loopback addresses only; malformed replies are errors,
  never crashes.
