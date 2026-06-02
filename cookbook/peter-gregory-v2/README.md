# Peter Gregory v2 — Latent Exposure Traversal

A graderless 8-case observational benchmark for triaging noisy external events
against company context, then traversing a constrained enterprise data API to
find or dismiss latent exposure.

## What's in the box

- `experiment.yaml` — graderless Bucephalus experiment spec. Metrics are
  extracted from the agent response, not from an answer-key grader.
- `cases.jsonl` — one row per public case id. Each row injects a high-level
  company baseline plus the external event stream directly into the trial input.
  It does not expose record-file paths.
- `workspaces/<scenario_id>/` — source bundles used to build sanitized
  `runtime-data/`. `.oracle.json` files are private reference material and are
  not copied into the data API image.
- `agent/run_nova_pg.js` — Nova/Codex runner. It builds a clean per-trial
  workspace containing only `events/event-stream.md`, `tools/pg_query`, and
  `output/scan_schema.json`, uses the ephemeral read-only data API, sends the
  company baseline and event stream in the prompt, captures the full agent
  output, and derives observational metrics.
- `agent/pg_data_api.py` and `agent/pg_query.py` — constrained traversal API
  service and client.
- `scripts/sync-workspaces.sh` — recompose the source workspaces from the
  upstream `synth-data-pipeline-agents` repo.

## The cases

| public id | source scenario | event stream | intended phenomenon |
|---|---|---|---|
| `pg_001` | `sesame_canonical` | H03 castor drought | supply-chain exposure |
| `pg_002` | `customer_of_customer` | E03 Iron Horse Rail guidance cut | customer-of-customer demand pull-through |
| `pg_003` | `regulatory_cascade` | R04 EO-13881 amendment | regulatory supplier disqualification |
| `pg_004` | `brand_exposure_tweet` | T05 viral castor-deforestation post | reputational exposure |
| `pg_005` | `noise_only_day` | no anchor | pure-noise dismissal |
| `pg_006` | `near_miss_material` | H05 cobalt + rare earths | material near miss |
| `pg_007` | `unrelated_industry_earnings` | E04 grocery chain warning | industry/customer near miss |
| `pg_008` | `out_of_scope_regulation` | R06 vehicle emissions EO | regulatory scope near miss |

## How to run

```bash
# 1. Make sure Nova/Codex credentials are available
#    experiment.yaml currently mounts ~/.config/nova/codex-auth.json

# 2. Build the agent runtime image. This image does not contain company records.
docker build -t peter-gregory-v2-nova:local \
  -f cookbook/peter-gregory-v2/Dockerfile.nova-runtime \
  cookbook/peter-gregory-v2

# 3. Build the ephemeral data API image.
docker build -t peter-gregory-v2-data-api:local \
  -f cookbook/peter-gregory-v2/Dockerfile.data-api \
  cookbook/peter-gregory-v2

# 4. Smoke-test against one variant + one case to validate plumbing
target/debug/bucephalus build-run cookbook/peter-gregory-v2/experiment.yaml --json --smoke-test

# 5. Full run across all variants × all cases
target/debug/bucephalus build-run cookbook/peter-gregory-v2/experiment.yaml --json
```

Reports land under `<run_dir>/`. Each trial produces:

- `out/result.json` — full event stream, submitted scan, normalized
  observations, scalar metrics, raw Nova response content, and Nova response
  envelope.
- optional Nova daemon logs under `out/nova/`

`RESPONSE_TIMEOUT_MS` is set in `experiment.yaml` to `540000`, leaving
headroom under the 600000 ms trial policy timeout.

The prompt is deliberately neutral: it gives the agent baseline company context
for fast triage, says most events may be noise, says there may be zero
exposures, and requires API-supported causal paths for alerts.

## Adding a model

Open `experiment.yaml` and add a row under `matrix.variants`:

```yaml
- id: <short-id>
  config: { provider: codex, model: gpt-5.5 }
```

`provider` and `model` are passed to Nova's model selector. The runner does
not need provider-specific workspace tools.

## Data API

The active workspace does not contain `records/`, and the agent image does not
contain the private record corpus. `experiment.yaml` attaches a per-trial
`pg-data-api` ephemeral container; the agent receives `PG_DATA_API_URL` and
`PG_CASE_ID` and must use:

```bash
tools/pg_query overview
tools/pg_query search castor
tools/pg_query get_entity castor_derived_polyol
tools/pg_query neighbors castor_derived_polyol
tools/pg_query trace_exposure castor_derived_polyol
tools/pg_query orders_for_product CMC-410
```

This is intentionally not a full database shell. It exposes bounded search,
entity lookup, neighbor traversal, and downstream exposure tracing.

## Updating the workspaces

The 8 workspaces are pre-composed from the `synth-data-pipeline-agents`
preset library. After editing any preset module or scenario YAML upstream:

```bash
bash cookbook/peter-gregory-v2/scripts/sync-workspaces.sh
```

This regenerates `data/peter_gregory/` upstream and copies the result here.

## Observability metrics

This experiment intentionally does not run a semantic answer-key grader. The
queryable metrics are observational:

- `schema_valid`, `submitted_present`, `json_parse_error`
- `event_count`, `alert_count`, `dismissal_count`
- `latent_edge_count`, `latent_node_count_total`, `latent_node_count_max`
- `affected_order_count`, `affected_product_count`
- `supporting_record_count`, `revenue_at_risk_total`
- `response_text_bytes`, `elapsed_seconds`

The complete structured scan remains in `out/result.json` under `submitted`.
Per-alert latent-edge nodes are also normalized under `observations.alerts`.
