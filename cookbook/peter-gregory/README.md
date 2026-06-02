# Peter Gregory — Latent Exposure Detection

A hand-curated 8-case benchmark for "find the latent edge between today's news
and your open sales orders." Inspired by the Silicon Valley sesame-seed scene.

## What's in the box

- `experiment.yaml` — Bucephalus experiment spec; model matrix lives in
  `matrix.variants`.
- `cases.jsonl` — one row per scenario. Four true positives (the right answer
  is a single alert at hop-4), four true negatives (the right answer is zero
  alerts plus an audited dismissal trail).
- `workspaces/<scenario_id>/` — the 8 read-only workspace bundles, each with
  `README.md` (agent brief), `records/` (ontology), `events/` (the day's feed),
  `tools/pg_tools.py` (the workspace CLI the model drives), `output/scan_schema.json`
  (required answer shape), and `.oracle.json` (grader-only).
- `agent/run_pg.py` — model-agnostic tool-call loop. Picks Anthropic or OpenAI
  from `variant.config.provider`. Materializes the per-trial workspace, strips
  the oracle, drives the model until `submit_scan` is called.
- `grader/pg_grader.py` — three-axis grader: precision (no false alerts),
  recall (no missed exposures), revenue accuracy (±5%), and audit completeness
  (did the agent log dismissed headlines on negative cases).
- `scripts/sync-workspaces.sh` — recompose the workspaces from the upstream
  `synth-data-pipeline-agents` repo. Run after editing any preset or scenario.

## The cases

| id | label | causal anchor | correct answer |
|---|---|---|---|
| `sesame_canonical` | TP | H03 castor drought | 1 alert: SO-8312 / $1.29M |
| `customer_of_customer` | TP | E03 Iron Horse Rail guidance cut | 1 alert: SO-7991 / $3.33M |
| `regulatory_cascade` | TP | R04 EO-13881 amendment | 1 alert: SO-8207 / $3.36M |
| `brand_exposure_tweet` | TP | T05 viral castor-deforestation post | 1 alert: SO-8312 reputational |
| `noise_only_day` | TN | (none — 14 distractors) | 0 alerts, ≥2 dismissals |
| `near_miss_material` | TN | H05 cobalt + rare earths | 0 alerts (no BOM match) |
| `unrelated_industry_earnings` | TN | E04 grocery chain warning | 0 alerts (no customer overlap) |
| `out_of_scope_regulation` | TN | R06 vehicle emissions EO | 0 alerts (no product family) |

## How to run

```bash
# 1. Make sure your model keys are exported
export ANTHROPIC_API_KEY=...
export OPENAI_API_KEY=...      # only if you enable an openai variant

# 2. Smoke-test against one variant + one case to validate plumbing
bucephalus build-run cookbook/peter-gregory/experiment.yaml \
  --materialize full --json --smoke-test

# 3. Full run across all variants × all cases
bucephalus build-run cookbook/peter-gregory/experiment.yaml \
  --materialize full --json
```

Reports land under `.lab/runs/<run-id>/`. Each trial produces:

- `out/result.json` — the agent's submitted scan + tool-call trace + token usage
- `out/pg_report.json` — the grader scores

## Adding a model

Open `experiment.yaml` and add a row under `matrix.variants`:

```yaml
- id: <short-id>
  config: { provider: anthropic, model: claude-sonnet-4-7 }
```

`provider` must be `anthropic` or `openai`. The agent runner picks the SDK
based on this value. No other changes needed.

## Updating the workspaces

The 8 workspaces are pre-composed from the `synth-data-pipeline-agents`
preset library. After editing any preset module or scenario YAML upstream:

```bash
bash cookbook/peter-gregory/scripts/sync-workspaces.sh
```

This regenerates `data/peter_gregory/` upstream and copies the result here.

## Grading rubric

`pg_report.json` carries five axes per trial. The `resolved` boolean is `True`
iff:

- zero false positives (no alert with the wrong causal event id or unmatched order)
- zero false negatives (every required alert was caught)
- 100% revenue accuracy (every matched alert's `revenue_at_risk` is within 5%)
- 100% audit completeness (every expected dismissed-event id is present on
  negative cases; ≥`minimum_dismissals` items present on pure-noise cases)

The five continuous metrics (`precision`, `recall`, `revenue_accuracy`,
`audit_completeness`, plus `false_positive_count` / `false_negative_count`)
let you slice partial credit and compare models that miss in different ways.
