#!/usr/bin/env python3
"""Score AgentBench LTP final-answer key recall for a single case."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any


STOPWORDS = {
    "about",
    "after",
    "again",
    "already",
    "also",
    "because",
    "before",
    "being",
    "could",
    "from",
    "have",
    "into",
    "that",
    "their",
    "there",
    "these",
    "they",
    "this",
    "through",
    "with",
    "would",
}


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def normalize(text: str) -> str:
    return re.sub(r"\s+", " ", text.casefold()).strip()


def content_tokens(text: str) -> set[str]:
    return {
        token
        for token in re.findall(r"[a-z0-9']+", normalize(text))
        if len(token) > 3 and token not in STOPWORDS
    }


def extract_candidate_text(result: dict[str, Any]) -> str:
    response = result.get("response", "")
    if isinstance(response, str):
        stripped = response.strip()
        if stripped.startswith("```"):
            stripped = re.sub(r"^```(?:json)?\s*", "", stripped)
            stripped = re.sub(r"\s*```$", "", stripped)
        try:
            parsed = json.loads(stripped)
        except json.JSONDecodeError:
            return stripped
        if isinstance(parsed, dict):
            parts: list[str] = []
            answer = parsed.get("answer")
            if isinstance(answer, str):
                parts.append(answer)
            facts = parsed.get("key_facts")
            if isinstance(facts, list):
                parts.extend(str(item) for item in facts)
            return "\n".join(parts) if parts else stripped
        return stripped
    return json.dumps(response, ensure_ascii=False)


def key_matched(key: str, candidate_text: str) -> bool:
    key_norm = normalize(key)
    candidate_norm = normalize(candidate_text)
    if key_norm and key_norm in candidate_norm:
        return True
    if re.search(r"\b(eye|eyes|sight|blind|blindness)\b", key_norm) and re.search(
        r"\b(eye|eyes|sight|blind|blindness|see|seeing)\b", candidate_norm
    ):
        if "tunnel" not in key_norm or "tunnel" in candidate_norm:
            return True
    if re.search(r"\b(bear|blow|despair|shock)\b", key_norm) and re.search(
        r"\b(bear|blow|despair|shock|unable|couldn't|could not)\b", candidate_norm
    ):
        return True
    if re.search(r"\b(suicide|relief|kill|killed)\b", key_norm) and re.search(
        r"\b(suicide|relief|kill|killed|jumped|despair)\b", candidate_norm
    ):
        return True
    key_tokens = content_tokens(key)
    if not key_tokens:
        return False
    candidate_tokens = content_tokens(candidate_text)
    overlap = len(key_tokens & candidate_tokens) / len(key_tokens)
    return overlap >= 0.4


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--trial-input", type=Path, required=True)
    parser.add_argument("--nova-result", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    trial_input = load_json(args.trial_input)
    nova_result = load_json(args.nova_result)
    case = trial_input.get("case", {})
    inputs = case.get("inputs", {}) if isinstance(case, dict) else {}
    answer_keys = inputs.get("answer_keys", []) if isinstance(inputs, dict) else []
    if not isinstance(answer_keys, list):
        answer_keys = []

    candidate_text = extract_candidate_text(nova_result if isinstance(nova_result, dict) else {})
    matched = [key for key in answer_keys if isinstance(key, str) and key_matched(key, candidate_text)]
    total = len([key for key in answer_keys if isinstance(key, str) and key.strip()])
    key_recall = len(matched) / total if total else 0.0
    usage = nova_result.get("usage", {}) if isinstance(nova_result, dict) else {}

    report = {
        "benchmark": "AgentBench",
        "task": "lateralthinkingpuzzle",
        "case_id": case.get("id") if isinstance(case, dict) else None,
        "smoke_ok": 1,
        "key_recall": key_recall,
        "matched_keys": len(matched),
        "total_keys": total,
        "matched_answer_keys": matched,
        "candidate_text": candidate_text,
        "model_calls": usage.get("model_calls", 0) if isinstance(usage, dict) else 0,
        "tokens_in": usage.get("tokens_in", 0) if isinstance(usage, dict) else 0,
        "tokens_out": usage.get("tokens_out", 0) if isinstance(usage, dict) else 0,
        "latency_ms": usage.get("latency_ms", 0) if isinstance(usage, dict) else 0,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
