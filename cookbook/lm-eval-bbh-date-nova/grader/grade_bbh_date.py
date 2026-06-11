#!/usr/bin/env python3
"""Grade Nova output for BBH date_understanding answer labels."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any


PAREN_LABEL_RE = re.compile(r"\(([A-F])\)", re.IGNORECASE)
BARE_LABEL_RE = re.compile(r"(?<![A-Za-z])([A-F])(?![A-Za-z])", re.IGNORECASE)


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def normalize(text: str) -> str:
    return re.sub(r"\s+", " ", text.casefold()).strip()


def extract_candidate_text(result: dict[str, Any]) -> str:
    response = result.get("response", "")
    if not isinstance(response, str):
        return json.dumps(response, ensure_ascii=False)

    stripped = response.strip()
    if stripped.startswith("```"):
        stripped = re.sub(r"^```(?:json)?\s*", "", stripped)
        stripped = re.sub(r"\s*```$", "", stripped)
    try:
        parsed = json.loads(stripped)
    except json.JSONDecodeError:
        return stripped
    if isinstance(parsed, dict):
        for key in ("answer", "selected_answer", "choice", "final_answer"):
            value = parsed.get(key)
            if isinstance(value, str) and value.strip():
                return value.strip()
        return json.dumps(parsed, ensure_ascii=False)
    return stripped


def choice_texts(question: str) -> dict[str, str]:
    labels = {}
    matches = list(re.finditer(r"\(([A-F])\)\s*", question))
    for i, match in enumerate(matches):
        start = match.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(question)
        labels[f"({match.group(1).upper()})"] = question[start:end].strip()
    return labels


def extract_label(candidate_text: str, choices: dict[str, str]) -> str | None:
    parenthesized = PAREN_LABEL_RE.findall(candidate_text)
    if parenthesized:
        return f"({parenthesized[-1].upper()})"

    bare = BARE_LABEL_RE.findall(candidate_text)
    if bare:
        return f"({bare[-1].upper()})"

    candidate_norm = normalize(candidate_text)
    for label, text in choices.items():
        if text and normalize(text) in candidate_norm:
            return label
    return None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--trial-input", type=Path, required=True)
    parser.add_argument("--nova-result", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    trial_input = load_json(args.trial_input)
    nova_result = load_json(args.nova_result)
    case = trial_input.get("case", {}) if isinstance(trial_input, dict) else {}
    inputs = case.get("inputs", {}) if isinstance(case, dict) else {}
    question = inputs.get("question", "") if isinstance(inputs, dict) else ""
    target = inputs.get("target", "") if isinstance(inputs, dict) else ""
    choices = choice_texts(question if isinstance(question, str) else "")

    candidate_text = extract_candidate_text(nova_result if isinstance(nova_result, dict) else {})
    predicted = extract_label(candidate_text, choices)
    exact_match = 1 if predicted == target else 0
    usage = nova_result.get("usage", {}) if isinstance(nova_result, dict) else {}

    report = {
        "benchmark": "lm-evaluation-harness",
        "task": "leaderboard_bbh_date_understanding",
        "case_id": case.get("id") if isinstance(case, dict) else None,
        "smoke_ok": 1,
        "exact_match": exact_match,
        "answer_extracted": 1 if predicted else 0,
        "prediction": predicted,
        "target": target,
        "candidate_text": candidate_text,
        "target_choice_text": choices.get(target),
        "model_calls": usage.get("model_calls", 0) if isinstance(usage, dict) else 0,
        "tokens_in": usage.get("tokens_in", 0) if isinstance(usage, dict) else 0,
        "tokens_out": usage.get("tokens_out", 0) if isinstance(usage, dict) else 0,
        "latency_ms": usage.get("latency_ms", 0) if isinstance(usage, dict) else 0,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
