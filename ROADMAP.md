# Roadmap

Kimera chains the Craft apps through their own command-line tools and control channels, one
program at a time, without changing the apps. This page lists what is done and what comes next.
Order can change; dates are not promised.

Status: ✅ done · 🟡 partly done · ⬜ planned

## 0.1 — Foundation ✅

- ✅ `kimera doctor`, `kimera pipeline`, TOML config and recipes.
- ✅ Safety: no panics outside tests, no `unsafe`, loopback-only control channel, inputs never
  overwritten, one new folder per run.

## 0.2 — The photographer's conveyor ✅

- ✅ `kimera run`: a folder (or some files) through the recipe's stages in any order, headless,
  one folder per stage, originals read only.
- ✅ Variants (several results per photo) and optional step groups.
- ✅ AI recipes from a request (Anthropic, OpenAI, Claude Code, Codex), checked against the apps'
  real command lists, with the [playbook](prompts/playbook.md).
- ✅ Per-photo AI look: red eyes fixed, closed eyes flagged with swap donors.
- ✅ The window: setup, photos, request, recipe, test with before / after, conveyor board, log,
  cancel, client proof.
- ✅ Cancel stops the running app at once; progress as events shared by the CLI and the window.
- ✅ Model checks and installs (SAM 3, YuNet, lens database); runs stop early when a required
  step needs a missing model.
- ✅ Bring results back into LightCraft, stacked on their originals (`pipeline --reimport`).
- ✅ More recipes: portrait retouch, social formats, marketplace, B&W, web gallery, print, real
  estate.
- 🟡 Closed eyes: detected and flagged with donor frames; the swap itself is still done by hand.
  An automatic swap (`loadFilesIntoStack` + align + eye mask) needs eye positions precise enough
  to mask, and a careful test set of real portraits before it can be trusted.
- ⬜ End-to-end test of `pipeline` and `--reimport` against a running LightCraft library (written,
  not yet run against a real catalog).

## 0.3 — More Craft apps 🟡

- ✅ VectorCraft stage: layouts and posters exported as PDF / PNG / SVG, with templates.
- ✅ Client proof PDF (contact sheets through PhotoCraft).
- ⬜ PdfCraft stage: waiting for PdfCraft's CLI on this setup; the proof covers the main need.
- ⬜ `doctor` checks VectorCraft's control port (7979) for a VectorCraft `pipeline`.

## 0.4 — Agent mode 🟡

- ✅ `kimera ask "<request>"` with the user's own AI (API or Claude Code / Codex CLI).
- ✅ Target picker in the window: one, some or all photos of a folder.
- ✅ Batch flow: test on one photo, approve, then run on the rest.
- ⬜ Interactive agent with the apps' MCP servers (`lightcraft-cli mcp`, `photocraft-cli mcp`) for
  edits a recipe cannot express, one app at a time.

## 0.5 — Undo ✅

- ✅ Undo a `--reimport` (`kimera unimport <run folder>`): the imported results go to LightCraft's
  Recently Deleted. Runs never edit LightCraft photos themselves, so nothing else needs undoing;
  a run's files are in its own folder.

## Later

- ✅ Progress reporting and cancel for long runs.
- 🟡 Release binaries: Windows zip and Inno Setup installer from `scripts/package.ps1` and
  `installer/kimera.iss`; the GitHub workflow builds Windows, macOS and Linux archives for `v*`
  tags (not yet run on a published tag).
- ⬜ A shared recipe library online.
- ⬜ Speed: keep one headless LightCraft session open per run (`serve`/MCP) so AI masks do not
  reload SAM 3 for every photo.
- ⬜ Window translations (Brazilian Portuguese first).

## Related

- LightCraft proposal for a built-in prompt bar: storytold/lightcraft#4410. If it lands, Kimera's
  agent mode can stay as the external option.
