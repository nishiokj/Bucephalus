#!/usr/bin/env python3
"""Derive the event-only ablation cases from the full peter-gregory-v2 cases.

This arm gives the agent ONLY the company baseline, the product catalog, and the
external event stream. The company data API is removed, so there is no record
access and no traversal. We rewrite the task block and prune the alert schema to
hypothesis-level fields the agent can actually justify without state.

Source of truth is the sibling full experiment's cases.jsonl. Run after the full
arm's cases change:

    ./scripts/build_cases.py
"""
from __future__ import annotations

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
SOURCE = HERE.parent / "peter-gregory-v2" / "cases.jsonl"
DEST = HERE / "cases.jsonl"
NOVA_IMAGE = "peter-gregory-v2-event-only-nova:local"

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
    "pg_010": "antimony_export_control",
}

# Scenario exposure class + full-arm correct verdict (shared taxonomy with the state-only arm).
EXPOSURE_CLASS = {
    "castor_canonical": "supply_disruption",
    "brand_exposure_tweet": "procurement_arbitrage",
    "gpu_capacity_reallocation": "capacity_reallocation",
    "antimony_export_control": "supply_disruption",
    "customer_of_customer": "demand_pull",
    "regulatory_cascade": "no_alert",
    "noise_only_day": "no_alert",
    "near_miss_material": "no_alert",
    "unrelated_industry_earnings": "no_alert",
    "out_of_scope_regulation": "no_alert",
}
# castor is a supply-themed near-miss (bridge then dismiss), so its with-event verdict is no_alert
# despite exposure_class supply_disruption; the real positives are brand/gpu/customer_of_customer.
FULL_ARM_VERDICT = {s: ("alert" if s in ("gpu_capacity_reallocation", "customer_of_customer", "antimony_export_control") else "no_alert") for s in EXPOSURE_CLASS}

EVENT_HEADER = "\n\nExternal event stream:\n"
TASK_HEADER = "\n\nTask:\n"


def task_block(case_id: str, schema_text: str) -> str:
    return f"""Task:
Triage the event stream for material latent exposure to the company described above. Most events may be noise. There may be zero exposures. Do not force a connection. Treat "no alert" as the correct answer when the company baseline does not support a concrete causal path.

You have ONLY the company baseline, the product catalog, and the external event stream above. There is no company data API and no record access in this condition. Reason from the baseline and catalog alone about which event, if any, plausibly creates a latent exposure, and lay out the hypothesized causal path. Do not assert specific order ids, revenue figures, inventory levels, or named internal records — you cannot observe them here.

Return exactly one JSON object that conforms to the following JSON Schema. Output only the JSON object — no Markdown, no prose, no code fences. Use only the listed exposure_kind enum values.

{schema_text}

Evidence standard:
- Raise an alert only when the company baseline and product catalog support a concrete, nameable causal path from a specific event to the company's products, inputs, customers, or obligations.
- Identify the causal_event_id and describe the latent_edge as the hypothesized path, with nodes and relationship specific enough to audit.
- State your hypothesis_confidence and the key_assumption the path depends on.
- If an event is plausible but unsupported, out of scope, or unrelated to the company baseline, put it in no_alert_paths_considered with the reason.

The JSON object must include:
- alerts: only hypothesized exposures, each with causal_event_id, latent_edge, exposure_kind, hypothesis_confidence, key_assumption, and business_action.
- no_alert_paths_considered: events you considered and dismissed, with reasons."""


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
        prompt = src["prompt"]

        preamble, rest = prompt.split(EVENT_HEADER, 1)
        preamble = preamble[preamble.index("Company baseline:"):]  # strip full-arm charter; this arm sets its own framing
        event_block = rest.split(TASK_HEADER, 1)[0].rstrip("\n")
        new_prompt = preamble + EVENT_HEADER + event_block + "\n\n" + task_block(case_id, schema_text)

        out = {
            "schema_version": "case_v2",
            "id": case_id,
            "inputs": {
                "case_id": case_id,
                "company_profile": src["company_profile"],
                "event_stream": src["event_stream"],
                "event_source": src.get("event_source"),
                "feed_type": src.get("feed_type"),
                "prompt": new_prompt,
            },
            "resources": {
                "workspace": {"source": "container_image", "image": NOVA_IMAGE, "workdir": "/bucephalus/workspace"}
            },
            "metadata": {
                "arm": "event_only",
                "scenario": SCENARIO.get(case_id),
                "exposure_class": EXPOSURE_CLASS.get(SCENARIO.get(case_id)),
                "full_arm_verdict": FULL_ARM_VERDICT.get(SCENARIO.get(case_id)),
                "feed_type": src.get("feed_type"),
                "event_count": row.get("metadata", {}).get("event_count"),
            },
        }
        rows_out.append(json.dumps(out, ensure_ascii=False))

    DEST.write_text("\n".join(rows_out) + "\n", encoding="utf-8")
    print(f"wrote {len(rows_out)} event-only cases to {DEST}")


if __name__ == "__main__":
    build()
