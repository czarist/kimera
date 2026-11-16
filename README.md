# Kimera

**A CLI tool for Craft apps.**

English · [Português (Brasil)](README.pt-BR.md) · [Roadmap](ROADMAP.md) · [Changelog](CHANGELOG.md)

Works with LightCraft, PhotoCraft and VectorCraft by the ArtCraft team. Not affiliated with or
endorsed by the ArtCraft team.

Kimera chains the Craft apps through their own command-line tools, one program at a time, handing
files from one stage to the next. It never changes the apps. Each run writes to its own folder,
and originals are never overwritten.

```
your folder (read only)
  └─► work/run-<time>/
        1-photocraft/   clean-up, pixel work   (photocraft-cli run)
        2-lightcraft/   masks, colour grading  (lightcraft-cli run --import … app.export)
```

Stages run in the order the recipe gives, one photo at a time; each stage's output is the next
one's input. Both apps run headless, so neither has to be open. A photo that fails stops there
and the others go on.

## Setup

1. Build: `cargo build --release` (binary: `target/release/kimera.exe`).
2. Copy `kimera.toml.example` to `kimera.toml` and set the paths of `lightcraft-cli`,
   `photocraft-cli` and the work folder.
3. Check everything: `kimera doctor`.

## Commands

| Command | What it does |
|---|---|
| `kimera doctor` | Finds the apps, prints their versions, says whether LightCraft's control port answers |
| `kimera run --recipe recipes/cleanup-cinematic.toml D:\shoot` | Runs the recipe's stages on every image of a folder |
| `kimera run --recipe … a.jpg b.jpg` | The same on some files only |
| `kimera pipeline --recipe … [--ids 12,15]` | Exports photos from a running LightCraft (`lightcraft --control 7980`; its selection, or the ids), then runs the stages on them |

## Recipes

A recipe is a TOML file: a list of `[[stage]]`, each one app and its engine commands, in order.
List an app's commands with `photocraft-cli commands` or `lightcraft-cli commands`.

```toml
[[stage]]
app = "photocraft"
out_ext = "tif"        # what the next stage reads; "pcraft" keeps layers (last stage only)
steps = [{ cmd = "select.subject" }]

[[stage]]
app = "lightcraft"
steps = [
  { cmd = "mask.add", params = { kind = "background" } },
  { cmd = "mask.adjust", params = { values = { exposure = -0.35 } } },
]
export = { format = "jpg", quality = 92 }   # also long_edge, bit_depth, color_space, options
```

For `kimera pipeline`, an `[export]` table (`format`, `bit_depth`, `long_edge`, `color_space`)
sets how photos leave the LightCraft catalog. Command ids and extensions are checked before
anything runs. [`prompts/playbook.md`](prompts/playbook.md) maps everyday requests to commands.

## Design rules

- **No crash:** no `unwrap`, `expect` or `panic!` outside tests, and no `unsafe`. Every failure
  is an error message.
- **Loopback only:** Kimera talks to LightCraft's control channel on `127.0.0.1` and refuses
  other addresses.
- **Non-destructive:** a PhotoCraft output never replaces its input, and each run gets a new
  `run-<time>` folder.

## License

MIT OR Apache-2.0.
