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
- The agent stage invokes Nova's native headless CLI directly with the
  benchmark MCP config:
  `nova run --mcp-config /opt/peter-gregory/agent/mcp.json ...`.
  There is no experiment-local Nova wrapper or RPC bridge.
- `agent/pg_data_api.py` — constrained traversal API service.
- `agent/pg_mcp_server.py` and `agent/mcp.json` — official Python MCP SDK
  stdio server and server/policy config for the Peter Gregory tools.
- `scripts/sync-workspaces.sh` — recompose the source workspaces from the
  upstream `synth-data-pipeline-agents` repo.

## The cases

| public id | source scenario | event stream | intended phenomenon |
|---|---|---|---|
| `pg_001` | `castor_canonical` | H03 Bahia wet-season drought | supply-chain exposure |
| `pg_002` | `customer_of_customer` | E03 Mississippi low-water barge restrictions | customer demand pull-through |
| `pg_003` | `regulatory_cascade` | R04 administrative grant-attestation update | regulatory false-positive trap |
| `pg_004` | `brand_exposure_tweet` | T05 periodical cicada cycle overlap | Peter Gregory-style procurement arbitrage |
| `pg_005` | `noise_only_day` | no anchor | pure-noise dismissal |
| `pg_006` | `near_miss_material` | H05 cobalt + rare earths | material near miss |
| `pg_007` | `unrelated_industry_earnings` | E04 grocery chain warning | industry/customer near miss |
| `pg_008` | `out_of_scope_regulation` | R06 vehicle emissions EO | regulatory scope near miss |
| `pg_009` | `gpu_capacity_reallocation` | I02 undersea fiber repair windows | GPU supply-chain capacity reallocation |

## How to run

```bash
# 1. Make sure Nova GraphD state and Codex OAuth are mounted
#    --secret-file graphd_db="$HOME/.graphd/graphd.db"
#    --secret-file codex_oauth="$HOME/.config/nova/codex-auth.json"

# 2. Build the agent runtime image. This image does not contain company records.
docker build -t peter-gregory-v2-nova:local \
  -f cookbook/peter-gregory-v2/Dockerfile.nova-runtime \
  cookbook/peter-gregory-v2

# 3. Build the ephemeral data API image.
docker build -t peter-gregory-v2-data-api:local \
  -f cookbook/peter-gregory-v2/Dockerfile.data-api \
  cookbook/peter-gregory-v2

# 4. Smoke-test against one variant + one case to validate plumbing
target/debug/bucephalus build-run cookbook/peter-gregory-v2/experiment.yaml --json --smoke-test \
  --secret-file graphd_db="$HOME/.graphd/graphd.db" \
  --secret-file codex_oauth="$HOME/.config/nova/codex-auth.json"

# 5. Full run across all variants × all cases
target/debug/bucephalus build-run cookbook/peter-gregory-v2/experiment.yaml --json \
  --secret-file graphd_db="$HOME/.graphd/graphd.db" \
  --secret-file codex_oauth="$HOME/.config/nova/codex-auth.json"
```

Reports land under `<run_dir>/`. Each trial produces:

- `out/result.json` — Nova headless CLI response envelope, including the raw
  final response string and usage counters.
- optional Nova daemon logs under `out/nova/`

`experiment.yaml` passes `--timeout-ms 540000`, leaving headroom under the
600000 ms trial policy timeout.

The prompt is deliberately neutral: it gives the agent baseline company context
and a short business-facing product catalog for fast triage, says most events
may be noise, says there may be zero exposures, and requires causal paths
supported by company API results for alerts.

## Adding a model

Open `experiment.yaml` and add a row under `matrix.variants`:

```yaml
- id: <short-id>
  config: { provider: codex, model: gpt-5.5 }
```

`provider` and `model` are passed to Nova's model selector. The runner does
not need provider-specific workspace tools.

## MCP data tools

The active workspace does not contain `records/`, and the agent image does not
contain the private record corpus. `experiment.yaml` attaches a per-trial
`pg-data-api` ephemeral container and passes `agent/mcp.json` to Nova. Nova is
expected to connect to the stdio MCP server and expose these read-only tools:

| tool | purpose |
|---|---|
| `pg_overview(case_id)` | List record collections and product families for one case. |
| `pg_search(case_id, query, limit)` | Search indexed entity records. |
| `pg_get_entity(case_id, entity_id)` | Return records for one entity id. |
| `pg_neighbors(case_id, entity_id)` | Return graph neighbors for one entity id. |
| `pg_trace_exposure(case_id, entity_ids)` | Trace entity ids to the reachable downstream records — products/orders (BOM cases), tenders + lane facts (procurement cases), or program commitments + transferable allocations (capacity cases). Returns neutral quantities, not a verdict; the agent composes actionability with the event. |
| `pg_orders_for_product(case_id, product_id)` | Return open orders for one product id. |

Every tool call carries `case_id` explicitly. The MCP server calls the
independent `pg-data-api` sidecar at `PG_DATA_API_URL` (default:
`http://pg-data-api:9757`). The model should never start the API server or
shell out to a benchmark CLI shim.

## Updating the workspaces

The 8 workspaces are pre-composed from the `synth-data-pipeline-agents`
preset library. After editing any preset module or scenario YAML upstream:

```bash
bash cookbook/peter-gregory-v2/scripts/sync-workspaces.sh
```

This regenerates `data/peter_gregory/` upstream and copies the result here.

## Observability metrics

This experiment intentionally does not run a semantic answer-key grader. The
queryable metrics come from Nova's native headless response envelope:

- `model_calls`, `tool_calls`
- `tokens_in`, `tokens_out`
- `latency_ms`

The model's final structured scan is in `out/result.json` under `response` as
the raw Nova final response string.
