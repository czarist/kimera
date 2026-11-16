# Roadmap

Kimera chains the Craft apps through their own command-line tools and control channels, one
program at a time, without changing the apps. This page lists what is done and what comes next.
Order can change; dates are not promised.

Status: ✅ done · 🟡 in progress · ⬜ planned

## 0.1 — Foundation ✅

- ✅ `kimera doctor`: finds LightCraft, PhotoCraft and VectorCraft, prints their versions, checks
  LightCraft's control channel.
- ✅ `kimera pipeline`: exports the photos selected in LightCraft (or `--ids`) and runs a PhotoCraft
  recipe on each file.
- ✅ `kimera photocraft`: runs only the PhotoCraft part of a recipe on existing files.
- ✅ TOML config (`kimera.toml`) and recipes (`recipes/*.toml`).
- ✅ Safety: no panics outside tests, no `unsafe`, loopback-only control channel, inputs never
  overwritten, one new folder per run.

## 0.2 — The photographer's conveyor 🟡

- ✅ `kimera run`: a folder (or some files) through the recipe's stages in any order, e.g.
  PhotoCraft clean-up → LightCraft masks; originals read only, one folder per stage.
- ✅ LightCraft headless (`lightcraft-cli run --import`): no open app needed.
- ✅ Progress as events (the engine is shared by the CLI and the coming window).
- ⬜ AI analysis per photo (vision): blemish positions → `paint.spotHealing` / `paint.remove` on a
  new layer; closed eyes → swap from a sibling frame (`loadFilesIntoStack` + align + eye mask),
  or flag for review when there is none.
- ⬜ Prompt → recipe through the user's AI (Anthropic / OpenAI API, or Claude Code / Codex),
  checked against the apps' real command lists.
- ⬜ Window (egui): setup with app paths and AI, folder thumbnails with pick one / some / all,
  prompt, test on one photo with before/after, then the conveyor board with live log and cancel.
- ⬜ Cancel a running stage (today cancel is checked between stages).
- ⬜ Turn PhotoCraft's subject selection into a layer mask in the default recipe.
- ⬜ Show each PhotoCraft command's result (for example the selection bounds), not only "done".
- ⬜ End-to-end test of `pipeline` against a running LightCraft.
- ⬜ Bring the result back into LightCraft, stacked with the original.
- ⬜ More recipes: skin clean-up, background replacement, flatten to TIFF for delivery.

## 0.3 — More Craft apps ⬜

- ⬜ VectorCraft stage: place the photo in a poster layout (title, credits, event logo) and export.
- ⬜ PdfCraft stage: contact sheet or client proof PDF from a run's results.
- ⬜ `doctor` checks every configured app and its control port (LightCraft 7980, VectorCraft 7979).

## 0.4 — Agent mode ⬜

- ⬜ `kimera ask "<prompt>"`: starts the user's own agent (Claude Code or Codex) with only the MCP
  server of the app the current stage needs, one app at a time.
- ⬜ Target picker: the active photo, the selection, a stack or an album in LightCraft.
- ⬜ Batch flow: edit the first photo, approve it, then apply to the rest.

## 0.5 — Undo ⬜

- ⬜ Undo a whole run: record each photo's LightCraft history position before a run and restore it
  (`history.list` / `history.restore`).

## Later

- ⬜ Progress reporting and cancel for long runs.
- ⬜ Release binaries for Windows, macOS and Linux.
- ⬜ A shared recipe library.

## Related

- LightCraft proposal for a built-in prompt bar: storytold/lightcraft#4410. If it lands, Kimera's
  agent mode can stay as the external option.
