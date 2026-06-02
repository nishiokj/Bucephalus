#!/usr/bin/env python3
"""Peter Gregory grader.

Reads the agent's submitted scan and the per-case oracle, and produces a
report with three independent dimensions so the experiment can distinguish
false negatives from false positives from sloppy reading.

Inputs (via Bucephalus runtime contract):
  --result-path        agent stage result JSON (contains `submitted` scan + trace)
  --oracle-path        path to the case's .oracle.json (mounted from the
                       read-only workspace bundle; see experiment.yaml)
  --report-output      where to write the grader report JSON

Score axes:
  - precision:        of alerts the agent raised, fraction that matched a true exposure
  - recall:           of true exposures, fraction the agent caught
  - revenue_accuracy: for matched alerts, whether revenue_at_risk is within 5% of oracle
  - audit_completeness: did the agent log the causal-looking trap headline in
                       no_alert_paths_considered on negative cases? (1.0 if yes, 0.0 if no)
  - resolved:         1.0 iff all of: zero false positives, zero false negatives,
                       all alerts revenue-accurate, audit log present where expected.
                       This is the headline "did the model pass this case" metric.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def _load(path: str | Path) -> Any:
    return json.loads(Path(path).read_text(encoding="utf-8"))


def _required_alerts(oracle_expected: dict[str, Any]) -> list[dict[str, Any]]:
    """An oracle either has explicit `alerts: []` (negative case) or carries
    the single canonical alert at its top level (positive case)."""
    if "alerts" in oracle_expected:
        return list(oracle_expected.get("alerts") or [])
    if not oracle_expected.get("affected_orders"):
        return []
    return [
        {
            "causal_headline": oracle_expected.get("causal_headline"),
            "affected_orders": list(oracle_expected.get("affected_orders") or []),
            "affected_customers": list(oracle_expected.get("affected_customers") or []),
            "revenue_at_risk": oracle_expected.get("revenue_at_risk"),
        }
    ]


def _alert_matches(submitted: dict[str, Any], required: dict[str, Any]) -> bool:
    """An alert matches if it cites the right causal event and at least one
    of the required affected orders."""
    if submitted.get("causal_event_id") != required.get("causal_headline"):
        return False
    submitted_orders = set(submitted.get("affected_orders") or [])
    required_orders = set(required.get("affected_orders") or [])
    return bool(required_orders & submitted_orders)


def _revenue_within_tolerance(submitted_rev: Any, required_rev: Any, tolerance: float = 0.05) -> bool:
    try:
        s = float(submitted_rev)
        r = float(required_rev)
    except (TypeError, ValueError):
        return False
    if r == 0:
        return s == 0
    return abs(s - r) / r <= tolerance


def grade(submitted: dict[str, Any] | None, oracle_expected: dict[str, Any]) -> dict[str, Any]:
    submitted = submitted or {}
    submitted_alerts: list[dict[str, Any]] = list(submitted.get("alerts") or [])
    dismissed: list[dict[str, Any]] = list(submitted.get("no_alert_paths_considered") or [])
    required = _required_alerts(oracle_expected)

    # Precision / recall: match each submitted alert to one or zero requireds.
    matched_required: set[int] = set()
    true_positives: list[dict[str, Any]] = []
    false_positives: list[dict[str, Any]] = []
    for sa in submitted_alerts:
        matched = False
        for idx, ra in enumerate(required):
            if idx in matched_required:
                continue
            if _alert_matches(sa, ra):
                true_positives.append({"submitted": sa, "required": ra})
                matched_required.add(idx)
                matched = True
                break
        if not matched:
            false_positives.append(sa)
    false_negatives = [ra for idx, ra in enumerate(required) if idx not in matched_required]

    precision = len(true_positives) / len(submitted_alerts) if submitted_alerts else (1.0 if not required else 0.0)
    recall = len(true_positives) / len(required) if required else (1.0 if not submitted_alerts else 0.0)

    # Revenue accuracy across matched pairs (1.0 if no matches and no requireds)
    if true_positives:
        within = sum(
            1
            for pair in true_positives
            if _revenue_within_tolerance(pair["submitted"].get("revenue_at_risk"), pair["required"].get("revenue_at_risk"))
        )
        revenue_accuracy = within / len(true_positives)
    else:
        revenue_accuracy = 1.0 if not required else 0.0

    # Audit completeness — on negative cases we expect the causal-looking
    # trap headline id to appear in no_alert_paths_considered.
    audit_completeness = 1.0
    audit_evidence: list[str] = []
    expected_dismissals = oracle_expected.get("expected_dismissal_event_ids") or []
    dismissed_ids = {d.get("event_id") for d in dismissed}
    missing_dismissals = [eid for eid in expected_dismissals if eid not in dismissed_ids]
    if expected_dismissals:
        if missing_dismissals:
            audit_completeness = 1.0 - (len(missing_dismissals) / len(expected_dismissals))
            audit_evidence.append(f"missing dismissals: {missing_dismissals}")
    # Also: if the day is pure noise, expect SOME dismissals to prove the
    # model read the feed instead of returning an empty scan reflexively.
    minimum_dismissals = int(oracle_expected.get("no_alert_paths_considered_minimum") or 0)
    if not required and minimum_dismissals and len(dismissed) < minimum_dismissals:
        deficit = (minimum_dismissals - len(dismissed)) / minimum_dismissals
        audit_completeness = min(audit_completeness, 1.0 - deficit)
        audit_evidence.append(f"dismissed only {len(dismissed)} of {minimum_dismissals} expected items")

    resolved = (
        not false_positives
        and not false_negatives
        and revenue_accuracy == 1.0
        and audit_completeness == 1.0
    )

    return {
        "resolved": 1.0 if resolved else 0.0,
        "precision": round(precision, 4),
        "recall": round(recall, 4),
        "revenue_accuracy": round(revenue_accuracy, 4),
        "audit_completeness": round(audit_completeness, 4),
        "true_positive_count": len(true_positives),
        "false_positive_count": len(false_positives),
        "false_negative_count": len(false_negatives),
        "submitted_alert_count": len(submitted_alerts),
        "submitted_dismissal_count": len(dismissed),
        "audit_evidence": audit_evidence,
        "false_positives": false_positives,
        "false_negatives": false_negatives,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--result-path", required=True)
    parser.add_argument("--oracle-path", required=True)
    parser.add_argument("--report-output", required=True)
    args = parser.parse_args()

    result = _load(args.result_path)
    oracle = _load(args.oracle_path)
    submitted = result.get("submitted") if isinstance(result, dict) else None

    report = grade(submitted, oracle.get("expected") or {})
    report["case_id"] = oracle.get("case_id")
    report["scenario"] = oracle.get("scenario")

    out = Path(args.report_output)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
