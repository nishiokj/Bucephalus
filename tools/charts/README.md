# lab-charts

Editorial chart renderer for Bucephalus experiments. Reads from the account
SQLite (`~/.bucephalus/bucephalus.sqlite` by default), renders every applicable
chart per experiment in a consistent editorial brand, and writes a browsable
HTML gallery.

Personal tooling — not part of the runner's distribution. The runner has no
dependency on this; this depends on the runner's SQLite schema.

## Setup

```bash
pip install matplotlib pandas seaborn numpy
```

Optionally, a shell alias for ergonomics:

```bash
# ~/.zshrc
alias lab-charts='python3 /path/to/Bucephalus/tools/charts/gallery.py'
```

## Usage

```bash
# Render every experiment with completed runs (skips up-to-date ones)
lab-charts --sweep

# Render specific experiments
lab-charts <experiment_id> [<experiment_id> ...]

# Force rerender even when charts are up to date
lab-charts --sweep --force

# Open the gallery
open /path/to/Bucephalus/tools/charts/gallery/index.html

# Render/open the newest completed experiment
lab-charts --open-latest

# Pick a recent completed run, optionally rename labels, then open it
lab-charts ls
```

A tighter run-then-view shell function:

```bash
labrun() {
  lab run "$@" && \
  lab-charts --sweep && \
  open /path/to/Bucephalus/tools/charts/gallery/index.html
}
```

## Architecture

```
brand/           Editorial design tokens (sizes, colors, fonts) + primitives.
                 Locked: 4 type tiers, 5 color roles, palette pool of 8.
charts/          One module per chart type. Each declares NAME, REQUIRES,
                 applicable(ctx), render(ctx, out_dir). Listed in REGISTRY.
data.py          Loads the per-experiment render context from SQLite, with
                 auto-derivation (titles, palettes, tick formats) and
                 override support.
gallery.py       Orchestrator. Discovers experiments, runs every applicable
                 chart, generates gallery/index.html.

gallery/         Generated output — gitignored.
gallery/runs/    Run-scoped chart output and per-run labels.json overrides.
```

Adding a new chart type: create `charts/<name>.py` with `NAME`, `REQUIRES`,
`applicable(ctx)`, and `render(ctx, out_dir)`. Add to `charts/__init__.py`'s
`REGISTRY`. That's it — the gallery picks it up automatically.
