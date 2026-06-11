#!/usr/bin/env python3
"""Generate a small BBH date_understanding slice from lm-evaluation-harness."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from datasets import load_dataset


FEWSHOT_EXAMPLES = [
    {
        "input": (
            "Today is Christmas Eve of 1937. What is the date 10 days ago in MM/DD/YYYY?\n"
            "Options:\n"
            "(A) 12/14/2026\n"
            "(B) 12/14/1950\n"
            "(C) 12/14/2007\n"
            "(D) 12/14/1937\n"
            "(E) 07/14/1938\n"
            "(F) 12/14/1988"
        ),
        "target": "(D)",
    },
    {
        "input": (
            "Tomorrow is 11/12/2019. What is the date one year ago from today in MM/DD/YYYY?\n"
            "Options:\n"
            "(A) 09/04/2018\n"
            "(B) 11/11/2018\n"
            "(C) 08/25/2018\n"
            "(D) 11/02/2018\n"
            "(E) 11/04/2018"
        ),
        "target": "(B)",
    },
    {
        "input": (
            "Jane and John married on Jan 2, 1958. It is their 5-year anniversary today. "
            "What is the date tomorrow in MM/DD/YYYY?\n"
            "Options:\n"
            "(A) 01/11/1961\n"
            "(B) 01/03/1963\n"
            "(C) 01/18/1961\n"
            "(D) 10/14/1960\n"
            "(E) 01/03/1982\n"
            "(F) 12/03/1960"
        ),
        "target": "(B)",
    },
]


def build_prompt(question: str) -> str:
    shots = [f"Q: {example['input']}\nA: {example['target']}" for example in FEWSHOT_EXAMPLES]
    shots.append(f"Q: {question}\nA:")
    return "\n\n".join(shots)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, default=Path("cases.jsonl"))
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--image", default="lm-eval-bbh-date-nova:local")
    parser.add_argument("--source-commit", required=True)
    args = parser.parse_args()

    dataset = load_dataset("SaylorTwift/bbh", "date_understanding", split="test")
    rows = []
    for index, doc in enumerate(dataset.select(range(args.limit))):
        prompt = build_prompt(doc["input"])
        rows.append(
            {
                "schema_version": "case_v2",
                "id": f"lm_eval_bbh_date_{index + 1:03d}",
                "inputs": {
                    "prompt": prompt,
                    "question": doc["input"],
                    "target": doc["target"],
                    "fewshot_examples": FEWSHOT_EXAMPLES,
                    "choices": ["(A)", "(B)", "(C)", "(D)", "(E)", "(F)"],
                },
                "resources": {
                    "workspace": {
                        "source": "container_image",
                        "image": args.image,
                        "workdir": "/bucephalus/workspace",
                    }
                },
                "metadata": {
                    "benchmark": "lm-evaluation-harness",
                    "benchmark_task_group": "leaderboard_bbh",
                    "task": "leaderboard_bbh_date_understanding",
                    "dataset_path": "SaylorTwift/bbh",
                    "dataset_name": "date_understanding",
                    "split": "test",
                    "source_commit": args.source_commit,
                    "source_task_yaml": (
                        "lm_eval/tasks/leaderboard/bbh_mc/date_understanding.yaml"
                    ),
                    "source_template_yaml": (
                        "lm_eval/tasks/leaderboard/bbh_mc/_fewshot_template_yaml"
                    ),
                    "source_index": index,
                    "metric_note": (
                        "Official leaderboard config uses multiple_choice acc_norm; "
                        "this recipe uses generated answer extraction against the same target label."
                    ),
                },
            }
        )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
