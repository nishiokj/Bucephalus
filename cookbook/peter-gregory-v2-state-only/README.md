# Peter Gregory v2 — State-Only Ablation

An ablation arm of [`peter-gregory-v2`](../peter-gregory-v2). The agent receives
**only** the company baseline, the product catalog, and the read-only company
data API. There is **no external event stream** in this condition.

It must surface latent exposure from the company's *own operating data* —
single-source dependencies, supplier or geographic concentration, inventory
shortfalls against open orders or contracting windows, qualification gaps,
capacity bottlenecks — or conclude that none is evident.

## Why this arm exists — the leak detector

The full experiment hands the agent a sparse, per-case enterprise ontology and
asks it to triage events against that state. If the per-case state is so tailored
that its mere contents identify the answer, the agent can shortcut the triage.
This arm tests exactly that: **with no event to triage, can the agent still
reconstruct the planted exposure from the state alone?** A high recovery rate is
strong evidence the state leaks relevance through its sparsity.

Recovery is judged against each scenario's intended phenomenon (the
`metadata.scenario` label maps to the same table as the full arm — e.g.
`castor_canonical` plants a supply exposure, `noise_only_day` plants nothing).
Read alongside:

- `peter-gregory-v2` — full condition (event + state).
- `peter-gregory-v2-event-only` — event without the state.

## Exposure classes and the leak invariant

Each case carries three orthogonal tags: `exposure_class` (the kind of exposure),
`full_arm_verdict` (the correct answer *with* the event), and `leak_invariant`
(what the state-only arm, *without* the event, should produce).

The key move is that **state is unassuming and numerical** — it carries facts
(inventory, lead times, committed quantities, transferable capacity, reservable
lanes), never verdicts. Actionability is reasoned out by composing those facts
with the event, not read off a label. So whether the event is load-bearing is a
property of the data design, and the trace API reports neutral facts, not a
`constrained_inventory` / `actionable_*` / `do_not_prioritize` adjudication.

- **`expect_no_alert`** (8 of 9: `castor_canonical`, `brand_exposure_tweet`,
  `gpu_capacity_reallocation`, and the five `no_alert` traps): the correct
  state-only output is **zero alerts**, because the action is event-conditioned or
  absent. Any state-only alert is a relevance leak. This holds by construction:
  - every committed order/tender is **coverable from on-hand inventory + qualified
    available supply in the baseline** (the shared-fixture distractor shortfalls on
    RSG-700/EVB-330 were covered; `brand`'s baseline sesame was raised so the
    committed bakery-grade tenders are met, leaving the Indonesia lane as *spare*
    optionality the event activates; `gpu`'s builds are on-track for their base
    window with no pre-stated accelerated option or bonus);
  - the state carries no baked opportunity — `brand`'s non-sesame reservable decoy
    lanes (`LANE-ARG-SUN-Q3`, `LANE-PRY-CHI-Q3`) are `contracted`, and `gpu`'s
    `constraint_status` / `accelerated_ship_window` / `acceleration_bonus` /
    `do_not_prioritize` verdicts were stripped to raw numbers;
  - the state-only task prompt enforces the bar: single-source / foreign /
    long-lead facts explain severity but are not alertable on their own.
- **`exempt`** (1 of 9: `customer_of_customer`, `demand_pull`): **degenerate** and
  left as-is. The event raises demand on RSG-700 — a supply-capped line with no
  available lever (no reallocation, substitute, or expedite) — so the event
  changes nothing actionable. It fails REACH ∧ LEVER ∧ FLIP at the LEVER step and
  cannot be made event-conditioned without inventing a lever; it stays flagged
  rather than faked.

All corrections were applied canonically (the full `peter-gregory-v2` arm serves
the same corpus) and verified to preserve every oracle target (sesame $1.29M,
brand $2.83M, gpu $19.8M).

## What differs from the full arm

- **No event stream.** It is stripped from every prompt, and the
  `event_stream` / `event_source` / `feed_type` input fields are dropped so
  nothing leaks the event back in.
- **Rewritten task.** The agent is told there is no event feed and to reason from
  the data itself. The data API, MCP tools, and the full record corpus are kept
  intact.
- **Schema.** `scan_schema.json` drops `causal_event_id` (no events to cite) and
  re-keys `no_alert_paths_considered` on `candidate_path_examined` instead of
  `event_id`. The state-grounded alert fields (`latent_edge`, `affected_orders`,
  `revenue_at_risk`, `supporting_records`, …) are exactly what we want produced.
  The schema is the canonical contract and `scripts/build_cases.py` inlines it
  verbatim into every case prompt — the agent has no file-read tool, so a path
  reference would be a dead pointer.
- **`pg_009` fix.** `pg_mcp_server.py`'s `CaseId` now includes `pg_009`, which the
  upstream full arm omitted.

## Cases

`cases.jsonl` is derived from the full arm's cases by `scripts/build_cases.py`,
the single source of truth for scenario content. Each row carries
`metadata.arm = "state_only"` and `metadata.scenario`. Regenerate after the full
arm's cases change:

```bash
./scripts/build_cases.py
```

`scripts/sync-workspaces.sh` still re-exports the record corpus from upstream
`synth-data-pipeline-agents`, same as the full arm.

## How to run

```bash
# Build the agent runtime image (carries the MCP server, not the records).
docker build -t peter-gregory-v2-state-only-nova:local \
  -f cookbook/peter-gregory-v2-state-only/Dockerfile.nova-runtime \
  cookbook/peter-gregory-v2-state-only

# Build the ephemeral data API image (carries the records).
docker build -t peter-gregory-v2-state-only-data-api:local \
  -f cookbook/peter-gregory-v2-state-only/Dockerfile.data-api \
  cookbook/peter-gregory-v2-state-only

# Smoke-test one variant + one case
target/debug/bucephalus build-run cookbook/peter-gregory-v2-state-only/experiment.yaml --json --smoke-test \
  --secret-file graphd_db="$HOME/.graphd/graphd.db" \
  --secret-file codex_oauth="$HOME/.config/nova/codex-auth.json"

# Full run across all variants × all cases
target/debug/bucephalus build-run cookbook/peter-gregory-v2-state-only/experiment.yaml --json \
  --secret-file graphd_db="$HOME/.graphd/graphd.db" \
  --secret-file codex_oauth="$HOME/.config/nova/codex-auth.json"
```

Like the full arm this is graderless; metrics come from Nova's response envelope
and the structured scan lands in `out/result.json`.

## MCP data tools

Unchanged from the full arm — `pg_overview`, `pg_search`, `pg_get_entity`,
`pg_neighbors`, `pg_trace_exposure`, `pg_orders_for_product`, each carrying
`case_id`. The MCP server calls the `pg-data-api` sidecar at `PG_DATA_API_URL`
(default `http://pg-data-api:9757`).
