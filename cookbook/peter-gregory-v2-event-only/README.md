# Peter Gregory v2 — Event-Only Ablation

An ablation arm of [`peter-gregory-v2`](../peter-gregory-v2). The agent receives
**only** the company baseline, the product catalog, and the external event
stream. There is **no company data API and no record access** in this condition.

It must reason from the baseline and catalog alone about which event, if any,
plausibly creates a latent exposure, and lay out the hypothesized causal path —
without claiming specific orders, revenue, inventory, or named internal records,
which it cannot observe here.

## Why this arm exists

The full experiment fuses an event stream with a sparse, per-case enterprise
ontology. The worry is that the state is *so* tailored per case that its mere
contents leak which event matters. This arm isolates the other half: **how far
can the agent get from the event + baseline alone?** If it fingers the right
event here at a high rate, the event itself is doing the discrimination and the
state isn't adding much. Read alongside:

- `peter-gregory-v2` — full condition (event + state).
- `peter-gregory-v2-state-only` — state without the event (the leak detector).

The full condition only earns its keep if it beats *both* ablations.

## What differs from the full arm

- **No data API.** `Dockerfile.data-api`, `pg_data_api.py`, `pg_mcp_server.py`,
  `mcp.json`, and the `workspaces/` + `runtime-data/` record corpus are absent.
- **No MCP wiring.** `experiment.yaml` drops the `pg-data-api` ephemeral, the
  agent `sidecars`, and the `--mcp-config` argument. The runtime image carries
  only the Nova config.
- **Rewritten task + schema.** The task is hypothesis-only. `scan_schema.json`
  drops every state-grounded field (`affected_*`, `revenue_at_risk`,
  `supporting_records`, `constrained_inventory`) and adds `hypothesis_confidence`
  and `key_assumption`. Alerts keep `causal_event_id` because events are present.
  The schema is the canonical contract and `scripts/build_cases.py` inlines it
  verbatim into every case prompt — the agent has no file-read tool, so a path
  reference would be a dead pointer.

## Cases

`cases.jsonl` is derived from the full arm's cases by `scripts/build_cases.py`,
which is the single source of truth for the scenario content. Each row carries
`metadata.arm = "event_only"` and `metadata.scenario` so analysis can pivot
arm-vs-arm on the same underlying scenario. Regenerate after the full arm's
cases change:

```bash
./scripts/build_cases.py
```

## How to run

```bash
# Build the agent runtime image (no company records, no MCP).
docker build -t peter-gregory-v2-event-only-nova:local \
  -f cookbook/peter-gregory-v2-event-only/Dockerfile.nova-runtime \
  cookbook/peter-gregory-v2-event-only

# Smoke-test one variant + one case
target/debug/bucephalus build-run cookbook/peter-gregory-v2-event-only/experiment.yaml --json --smoke-test \
  --secret-file graphd_db="$HOME/.graphd/graphd.db" \
  --secret-file codex_oauth="$HOME/.config/nova/codex-auth.json"

# Full run across all variants × all cases
target/debug/bucephalus build-run cookbook/peter-gregory-v2-event-only/experiment.yaml --json \
  --secret-file graphd_db="$HOME/.graphd/graphd.db" \
  --secret-file codex_oauth="$HOME/.config/nova/codex-auth.json"
```

Like the full arm this is graderless; metrics come from Nova's response envelope
(`model_calls`, `tool_calls`, `tokens_in/out`, `latency_ms`) and the structured
scan lands in `out/result.json`.
