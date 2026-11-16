# Kimera

**A photographer's conveyor for the Craft apps.**

English · [Português (Brasil)](README.pt-BR.md) · [Roadmap](ROADMAP.md) · [Changelog](CHANGELOG.md)

Works with LightCraft, PhotoCraft and VectorCraft by the ArtCraft team. Not affiliated with or
endorsed by the ArtCraft team.

Pick a folder of photos and say what you want ("remove the pimples, whiten the teeth, give it a
film look, and make a stories version"). Your AI turns that into a recipe. Kimera then runs the
Craft apps' own command-line tools one after the other on every photo, like a conveyor:

```
your folder (read only)
  └─► work/run-<time>/
        1-photocraft/            clean-up, pixel work     (photocraft-cli run)
        2-lightcraft/            masks, colour, retouch   (lightcraft-cli run --import … app.export)
        3-stories-lightcraft/    one folder per variant   (feed, stories, B&W…)
```

The apps run headless, so none has to be open. Originals are never written to, every run gets
its own folder, and a photo that fails stops there while the others go on.

## Install

- **Windows zip and installer:** `scripts\package.ps1` builds `dist\Kimera-<version>-windows-x64.zip`;
  compiling `installer\kimera.iss` with Inno Setup 6 turns it into a setup program that adds
  Kimera to the Start menu and, optionally, an "Open in Kimera" entry on folders. Tagged versions
  (`v*`) are built for Windows, macOS and Linux by the GitHub workflow.
- **From source:** `cargo build --release` gives `target/release/kimera.exe` (command line) and
  `target/release/kimera-gui.exe` (window).

Open **Kimera** (the window). The first time it shows **Setup**:

1. The paths of `lightcraft-cli`, `photocraft-cli` and, optionally, `vectorcraft-cli`.
2. A work folder for the runs.
3. The AI: **Anthropic** or **OpenAI** (API key in `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`, or
   pasted in), or **Claude Code** / **Codex** (the `claude` / `codex` CLIs signed in on this
   computer). `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` are honoured for gateways.
4. **Save**. The checks list what works and which optional LightCraft models are missing:
   SAM 3 for AI masks, YuNet for automatic blemish removal, the lens database. Each one can be
   installed from there, through a running `lightcraft --control 7980`.

The settings are kept in `kimera.toml` next to the program (see `kimera.toml.example`).

## The window

1. **Photos:** choose a folder (or right-click a folder → *Open in Kimera*), then pick one, some
   or all of the photos.
2. **What should happen?** Describe the edit and press *Write recipe with AI*. The AI reads
   the apps' real command lists and Kimera's [playbook](prompts/playbook.md), and answers with
   notes and a recipe. Kimera refuses any command the apps do not have. *Change current recipe*
   adjusts the one on screen instead.
3. **Recipe:** read or edit it, or open one from `recipes/`.
4. **Test on 1 photo** shows before and after. **Run on N photos** starts the conveyor board
   (queue → each stage → done / failed) with a live log and *Cancel*, which stops the running app
   at once. *AI looks at each photo first* finds red eyes (fixed in LightCraft) and closed eyes
   (flagged for review, with nearby frames of the same people to swap from).
5. **Client proof (PDF)** makes A4 contact sheets of the results.

## The command line

| Command | What it does |
|---|---|
| `kimera doctor` | Checks the apps, their models, the work folder and the AI |
| `kimera ask "<request>" <folder or files> [--run] [--analyse]` | The AI writes a recipe (saved, shown, run with `--run`) |
| `kimera run --recipe <file> <folder or files> [--analyse]` | Runs a recipe |
| `kimera check <recipes…>` | Checks recipes against the apps' real commands and installed models |
| `kimera analyse <folder or files>` | The AI looks at each photo: faces, closed eyes, red eyes |
| `kimera proof <folder or files>` | Client proof: A4 contact sheets as PDF |
| `kimera pipeline --recipe <file> [--ids 12,15] [--reimport]` | Exports photos from a running LightCraft, runs the recipe, and with `--reimport` stacks the results on their originals |
| `kimera unimport <run folder>` | Undoes a `--reimport`: the imported results go to LightCraft's Recently Deleted (files stay) |
| `kimera models install sam3\|yunet\|lensdb --accept` | Installs an optional LightCraft model through the running app |
| `kimera recipes` | Lists the bundled recipes |

## Recipes

A recipe is a TOML file: `[[stage]]` tables run in order, each one app and its engine commands.
`[[variant]]` tables add several results per photo. List an app's commands with
`lightcraft-cli commands`, `photocraft-cli commands` or `vectorcraft-cli commands`.

```toml
[[stage]]
app = "lightcraft"
steps = [
  { cmd = "spot.findBlemishes", params = { sensitivity = 45 }, optional = "blemishes" },
  { cmd = "preset.apply", params = { id = "lc.cinematic", amount = 80 } },
  { cmd = "mask.add", params = { kind = "prompt", text = "teeth" }, optional = "teeth" },
  { cmd = "mask.adjust", params = { values = { exposure = 0.15, saturation = -35 } }, optional = "teeth" },
]
export = { format = "tif", bit_depth = 16 }

[[variant]]
name = "stories"
[[variant.stage]]
app = "lightcraft"
steps = [{ cmd = "crop.aspect", params = { aspect = [9, 16] } }]
export = { format = "jpg", long_edge = 1920, options = { watermark = { text = "© Studio" } } }
```

- `optional = "<group>"`: when a step of the group fails on a photo (no teeth in the photo, a
  model not installed), the whole group is left out for that photo and the stage runs again.
- `export` takes `format`, `long_edge`, `quality`, `bit_depth`, `color_space`, and `options` for
  any other LightCraft export option (watermark, `limitKb`, `sharpen`, `metadata`,
  `removeLocation`…). Options that choose where files go are refused.
- PhotoCraft stages write `out_ext` (`tif` between stages, `pcraft` only as the last stage).
- VectorCraft stages open the photo, or a `template` whose steps place it with `{photo}`.
- Raw files must meet a LightCraft stage before any PhotoCraft stage.

Bundled recipes: cinematic clean-up, portrait retouch, social formats (feed, stories, square),
marketplace white background, black and white, web gallery with watermark, print delivery, real
estate, subject mask.

## Design rules

- **No crash:** no `unwrap`, `expect` or `panic!` outside tests, and no `unsafe`. Every failure
  is an error message.
- **Non-destructive:** a stage never writes over its input, originals are only read, and each
  run gets a new folder.
- **Checked recipes:** recipes may come from an AI, so command ids, extensions and export
  options are checked before anything runs, and only the apps' real commands are accepted.
- **Loopback only:** Kimera talks to LightCraft's control channel on `127.0.0.1` only.
- **Your AI, your keys:** prompts and previews go only to the AI provider you configure.

## License

MIT OR Apache-2.0.
