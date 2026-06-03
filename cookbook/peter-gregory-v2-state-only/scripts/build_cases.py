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
    "pg_001": "sesame_canonical",
    "pg_002": "customer_of_customer",
    "pg_003": "regulatory_cascade",
    "pg_004": "brand_exposure_tweet",
    "pg_005": "noise_only_day",
    "pg_006": "near_miss_material",
    "pg_007": "unrelated_industry_earnings",
    "pg_008": "out_of_scope_regulation",
    "pg_009": "gpu_capacity_reallocation",
}

EVENT_HEADER = "\n\nExternal event stream:\n"


def task_block(case_id: str, schema_text: str) -> str:
    return f"""Task:
You are given ONLY the company baseline, the product catalog above, and the read-only company data API for case_id="{case_id}". There is no external event feed in this condition. Inspect the company's own operating data and identify any material latent exposure that is evident from the data itself — for example single-source dependencies, supplier or geographic concentration, inventory shortfalls against open orders or contracting windows, qualification or certification gaps, or capacity bottlenecks. There may be zero such exposures. Do not force a connection. Treat "no alert" as the correct answer when the data does not support a concrete exposure.

The detailed company data is exposed through Peter Gregory MCP tools, not as flat files. Every Peter Gregory tool call must include case_id="{case_id}". Use pg_overview, pg_search, pg_get_entity, pg_neighbors, pg_trace_exposure, and pg_orders_for_product to traverse the ontology and operating data.

Return exactly one JSON object that conforms to the following JSON Schema. Output only the JSON object — no Markdown, no prose, no code fences. latent_edge is a single string; use only the listed exposure_kind enum values.

{schema_text}

Traversal standard:
- Use pg_overview with case_id="{case_id}" once to inspect the available record collections.
- Use pg_search with case_id="{case_id}" for products, customers, suppliers, materials, commodities, lanes, or programs.
- Use pg_get_entity and pg_neighbors with case_id="{case_id}" to follow dependencies before claiming an exposure.
- Use pg_trace_exposure with case_id="{case_id}" to verify downstream products, orders, revenue, inventory, or program allocation.

Evidence standard:
- Raise an alert only when you can identify a supported latent_edge through MCP tool results to affected orders, programs, or other affected entities.
- Include latent_edge as the causal path you found, with nodes and relationship specific enough to audit.
- Include supporting_records for the record collections or entities that substantiate the path.
- If a path is plausible but unsupported by the data, put it in no_alert_paths_considered with the reason.

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
            },
        }
        rows_out.append(json.dumps(out, ensure_ascii=False))

    DEST.write_text("\n".join(rows_out) + "\n", encoding="utf-8")
    print(f"wrote {len(rows_out)} state-only cases to {DEST}")


if __name__ == "__main__":
    build()
