# Changelog

All notable changes to Kimera are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `ROADMAP.md`, this changelog, and a Brazilian Portuguese README (`README.pt-BR.md`).
- `kimera run --recipe <file> <folders/files…>`: the photographer's conveyor. Every image of a
  folder (or the files given) goes through the recipe's stages in order, each stage writing into
  its own `<n>-<app>/` folder of the run. Originals are only read; a photo that fails stops there
  and the others go on.
- LightCraft stages run headless (`lightcraft-cli run --import <file> … app.export`), so
  LightCraft no longer has to be open except for `kimera pipeline`.
- `recipes/cleanup-cinematic.toml`: PhotoCraft first, then a LightCraft cinematic grade (teal
  shadows, warm highlights, darker background mask, vignette), exported as JPEG.
- `prompts/playbook.md`: what photographers, designers and marketers ask, in their words, mapped
  to real LightCraft and PhotoCraft commands, with the limits (no generative fill, steps that
  fail when they find nothing, lines not to cross). The AI reads it when it writes a recipe.
- LightCraft stages take `export.options`: any other `app.export` option (watermark, `limitKb`,
  output sharpening, metadata, location removal…); options that choose where files go are refused.
- Recipes are checked before anything runs: command ids, bare output extensions, `.pcraft` only
  as the last stage.

### Changed

- Recipes are now a list of `[[stage]]` tables (`app = "photocraft"` or `"lightcraft"`) instead of
  one `[photocraft]` table; `[export]` is only for `kimera pipeline`. PhotoCraft stages default to
  `out_ext = "tif"`.
- `kimera pipeline` exports into `0-export/` and then runs all the recipe's stages.
- `kimera doctor` no longer fails when LightCraft's control port is closed; only `pipeline` needs it.

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
