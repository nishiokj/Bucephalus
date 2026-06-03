#!/usr/bin/env python3
"""Derive the state-only ablation cases from the full peter-gregory-v2 cases.

This arm gives the agent ONLY the company baseline, the product catalog, and the
read-only company data API. The external event stream is removed entirely, so the
agent must surface exposure from the company's own operating data. We rewrite the
task block and prune the alert schema of the event-anchored field (causal_event_id).

If the agent can reconstruct the planted exposure here, with no event to triage,
the per-case state is leaking relevance through its sparsity — which is the whole
point of this arm.

Source of truth is the sibling full experiment's cases.jsonl:

    ./scripts/build_cases.py
"""
from __future__ import annotations

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
SOURCE = HERE.parent / "peter-gregory-v2" / "cases.jsonl"
DEST = HERE / "cases.jsonl"
NOVA_IMAGE = "peter-gregory-v2-state-only-nova:local"

SCENARIO = {
    "pg_001": "castor_canonical",
    "pg_002": "customer_of_customer",
    "pg_003": "regulatory_cascade",
    "pg_004": "brand_exposure_tweet",
    "pg_005": "noise_only_day",
    "pg_006": "near_miss_material",
    "pg_007": "unrelated_industry_earnings",
    "pg_008": "out_of_scope_regulation",
    "pg_009": "gpu_capacity_reallocation",
}

# Three orthogonal axes per scenario:
#   exposure_class    - the kind of exposure being probed.
#   full_arm_verdict  - the correct answer WITH the event (event + state).
#   leak_invariant    - what the state-only arm (no event) should produce:
#       expect_no_alert -> a clean, unassuming-numerical state yields zero alerts; the action is
#                          event-conditioned, so any state-only alert is a relevance leak.
#       exempt          -> the action is derivable from static state alone and the event is not
#                          load-bearing (a degenerate case we could not fix). Currently only
#                          customer_of_customer: the event just raises demand on a supply-capped
#                          line with no available lever, so there is nothing the event changes.
EXPOSURE_CLASS = {
    "castor_canonical": "supply_disruption",
    "brand_exposure_tweet": "procurement_arbitrage",
    "gpu_capacity_reallocation": "capacity_reallocation",
    "customer_of_customer": "demand_pull",
    "regulatory_cascade": "no_alert",
    "noise_only_day": "no_alert",
    "near_miss_material": "no_alert",
    "unrelated_industry_earnings": "no_alert",
    "out_of_scope_regulation": "no_alert",
}
FULL_ARM_VERDICT = {
    "castor_canonical": "alert",
    "brand_exposure_tweet": "alert",
    "gpu_capacity_reallocation": "alert",
    "customer_of_customer": "alert",
    "regulatory_cascade": "no_alert",
    "noise_only_day": "no_alert",
    "near_miss_material": "no_alert",
    "unrelated_industry_earnings": "no_alert",
    "out_of_scope_regulation": "no_alert",
}
# What live state-only runs showed, after fixing the decoy populations:
#   supply_disruption, no_alert, procurement_arbitrage, capacity_reallocation -> expect_no_alert.
#     brand (arbitrage) leaked only via decoy tenders that were unfillable on TONS, GRADE, or
#     DELIVERY WINDOW; covering all three closed it. gpu (capacity) leaked only because every
#     program looked short on finished goods; giving each program a build pipeline (finished +
#     in-progress >= requested) closed it. Neither needed an event-aware API — the with-event
#     signal is a directional judgment (reserve-before-reprice / reallocate-to-pull-forward).
#   demand_pull (customer_of_customer): the event raises demand on a supply-capped line with no
#     lever (no reallocation/substitute/expedite) -> NO-LEVER-BAN -> genuinely exempt.
_EXEMPT_CLASSES = {"demand_pull"}
LEAK_INVARIANT = {s: ("exempt" if EXPOSURE_CLASS[s] in _EXEMPT_CLASSES else "expect_no_alert") for s in EXPOSURE_CLASS}

EVENT_HEADER = "\n\nExternal event stream:\n"


def task_block(case_id: str, schema_text: str) -> str:
    return f"""Task:
You are given ONLY the company baseline, the product catalog above, and the read-only company data API for case_id="{case_id}". There is no external event feed in this condition. Inspect the company's own operating data and identify any material exposure where the company cannot meet a committed order or tender from on-hand inventory plus qualified, currently-available supply. A dependency that is single-source, concentrated, foreign, or long-lead-time is latent structure, not an exposure, as long as the affected commitment is currently coverable — do not alert on structural fragility alone. There may be zero exposures. Do not force a connection. Treat "no alert" as the correct answer when every committed order and tender is coverable from current inventory and qualified available supply.

The detailed company data is exposed through the company data API tools, not as flat files. Every data API tool call must include case_id="{case_id}". Use cd_overview, cd_search, cd_get_entity, cd_neighbors, cd_trace_exposure, and cd_orders_for_product to traverse the ontology and operating data.

Return exactly one JSON object that conforms to the following JSON Schema. Output only the JSON object — no Markdown, no prose, no code fences. latent_edge is a single string; use only the listed exposure_kind enum values.

{schema_text}

Traversal standard:
- Use cd_overview with case_id="{case_id}" once to inspect the available record collections.
- Use cd_search with case_id="{case_id}" for products, customers, suppliers, materials, commodities, lanes, or programs.
- Use cd_get_entity and cd_neighbors with case_id="{case_id}" to follow dependencies before claiming an exposure.
- Use cd_trace_exposure with case_id="{case_id}" to verify downstream products, orders, revenue, inventory, or program allocation.

Evidence standard:
- Raise an alert only when MCP tool results show a committed order, tender, or program commitment that cannot be met from on-hand inventory plus qualified, currently-available (or transferable) supply within its window.
- Single-source, no-substitute, foreign, or long-lead-time facts may explain the severity of such a shortfall, but are not sufficient on their own to raise an alert.
- Include latent_edge as the causal path you found, with nodes and relationship specific enough to audit.
- Include supporting_records for the record collections or entities that substantiate the path.
- If a dependency is structurally fragile but the affected commitment is currently coverable, put it in no_alert_paths_considered with the reason.

The JSON object must include:
- alerts: only exposures found, each with latent_edge, affected_orders, revenue_at_risk, exposure_kind, business_action, and supporting_records.
- no_alert_paths_considered: paths you considered and dismissed, with reasons."""


def build() -> None:
    schema_text = json.dumps(json.loads((HERE / "agent" / "scan_schema.json").read_text(encoding="utf-8")), indent=2)
    rows_out = []
    for line in SOURCE.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        case_id = row["id"]
        src = row["inputs"]

        # Drop the event stream entirely: keep everything up to the event header.
        preamble = src["prompt"].split(EVENT_HEADER, 1)[0]
        new_prompt = preamble + "\n\n" + task_block(case_id, schema_text)

        out = {
            "schema_version": "case_v2",
            "id": case_id,
            "inputs": {
                "case_id": case_id,
                "company_profile": src["company_profile"],
                "prompt": new_prompt,
            },
            "resources": {
                "workspace": {"source": "container_image", "image": NOVA_IMAGE, "workdir": "/bucephalus/workspace"}
            },
            "metadata": {
                "arm": "state_only",
                "scenario": SCENARIO.get(case_id),
                "exposure_class": EXPOSURE_CLASS.get(SCENARIO.get(case_id)),
                "full_arm_verdict": FULL_ARM_VERDICT.get(SCENARIO.get(case_id)),
                "leak_invariant": LEAK_INVARIANT.get(SCENARIO.get(case_id)),
            },
        }
        rows_out.append(json.dumps(out, ensure_ascii=False))

    DEST.write_text("\n".join(rows_out) + "\n", encoding="utf-8")
    print(f"wrote {len(rows_out)} state-only cases to {DEST}")


if __name__ == "__main__":
    build()
