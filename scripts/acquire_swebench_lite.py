#!/usr/bin/env python3
"""Acquire SWE-bench Lite tasks and official grading metadata for AgentLab."""

from __future__ import annotations

import argparse
import json
import sys
import urllib.parse
import urllib.request
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


DEFAULT_DATASET = "princeton-nlp/SWE-bench_Lite"
DEFAULT_SPLIT = "test"
DEFAULT_COUNT = 30
DEFAULT_MAX_PER_REPO = 6
PAGE_SIZE = 100
REQUIRED_GRADER_FIELDS = ("test_patch", "FAIL_TO_PASS", "PASS_TO_PASS")


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        stripped = line.strip()
        if not stripped:
            continue
        payload = json.loads(stripped)
        if not isinstance(payload, dict):
            raise ValueError(f"{path}:{line_no}: expected a JSON object")
        rows.append(payload)
    return rows


def write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def write_jsonl(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "".join(json.dumps(row, separators=(",", ":"), sort_keys=True) + "\n" for row in rows),
        encoding="utf-8",
    )


def compact(text: Any) -> str:
    return " ".join(str(text or "").split())


def slug_task_id(instance_id: str) -> str:
    chars = [ch.lower() if ch.isalnum() else "_" for ch in instance_id.replace("__", "_")]
    slug = "".join(chars).strip("_")
    while "__" in slug:
        slug = slug.replace("__", "_")
    return f"swebench_{slug}"


def require_string(row: dict[str, Any], key: str) -> str:
    value = row.get(key)
    if isinstance(value, str) and value.strip():
        return value.strip()
    instance_id = row.get("instance_id", "<unknown>")
    raise ValueError(f"{instance_id}: missing non-empty string field {key!r}")


def has_grader_field(row: dict[str, Any], key: str) -> bool:
    value = row.get(key)
    if isinstance(value, str):
        return bool(value.strip())
    if isinstance(value, list):
        return bool(value)
    return value is not None


def validate_official_row(row: dict[str, Any]) -> None:
    for key in ("repo", "instance_id", "base_commit", "problem_statement"):
        require_string(row, key)
    missing = [key for key in REQUIRED_GRADER_FIELDS if not has_grader_field(row, key)]
    if missing:
        instance_id = row.get("instance_id", "<unknown>")
        raise ValueError(f"{instance_id}: missing official grader field(s): {', '.join(missing)}")


def task_image(instance_id: str) -> str:
    return f"ghcr.io/epoch-research/swe-bench.eval.x86_64.{instance_id}:latest"


def to_task_row(row: dict[str, Any], *, benchmark_name: str, split: str, adapter_id: str) -> dict[str, Any]:
    validate_official_row(row)
    repo = require_string(row, "repo")
    instance_id = require_string(row, "instance_id")
    base_commit = require_string(row, "base_commit")
    prompt = require_string(row, "problem_statement")
    task_id = slug_task_id(instance_id)

    swebench_input: dict[str, Any] = {
        "repo": repo,
        "instance_id": instance_id,
        "base_commit": base_commit,
    }
    hints = row.get("hints_text")
    if isinstance(hints, str) and hints.strip():
        swebench_input["hints_text"] = compact(hints)

    return {
        "schema_version": "task_row_v1",
        "id": task_id,
        "image": task_image(instance_id),
        "workdir": "/testbed",
        "task": {
            "id": task_id,
            "benchmark": {
                "adapter_id": adapter_id,
                "name": benchmark_name,
                "split": split,
            },
            "input": {
                "prompt": compact(prompt),
            },
            "swebench": {
                "input": swebench_input,
            },
            "metadata": {
                "created_at": row.get("created_at"),
                "version": row.get("version"),
                "environment_setup_commit": row.get("environment_setup_commit"),
            },
        },
        "materialization": {
            "kind": "task_image",
            "platform": "linux/amd64",
        },
    }


def fetch_page(dataset: str, split: str, offset: int, length: int) -> list[dict[str, Any]]:
    query = urllib.parse.urlencode(
        {
            "dataset": dataset,
            "config": "default",
            "split": split,
            "offset": str(offset),
            "length": str(length),
        }
    )
    url = f"https://datasets-server.huggingface.co/rows?{query}"
    with urllib.request.urlopen(url, timeout=60) as response:
        payload = json.loads(response.read().decode("utf-8"))
    rows = payload.get("rows")
    if not isinstance(rows, list):
        return []
    normalized: list[dict[str, Any]] = []
    for entry in rows:
        row = entry.get("row") if isinstance(entry, dict) else entry
        if isinstance(row, dict):
            normalized.append(row)
    return normalized


def fetch_all_rows(dataset: str, split: str) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    offset = 0
    while True:
        page = fetch_page(dataset, split, offset, PAGE_SIZE)
        if not page:
            break
        rows.extend(page)
        offset += len(page)
        if len(page) < PAGE_SIZE:
            break
    return rows


def load_ids(path: Path) -> list[str]:
    if not path.exists():
        return []
    return [
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


def write_ids(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "# SWE-bench Lite instance IDs selected for this AgentLab acquisition",
        "# One instance_id per line.",
        *[require_string(row, "instance_id") for row in rows],
        "",
    ]
    path.write_text("\n".join(lines), encoding="utf-8")


def deterministic_curate(rows: list[dict[str, Any]], count: int, max_per_repo: int) -> list[dict[str, Any]]:
    sorted_rows = sorted(rows, key=lambda row: (str(row.get("repo", "")), str(row.get("instance_id", ""))))
    selected: list[dict[str, Any]] = []
    per_repo: Counter[str] = Counter()
    for row in sorted_rows:
        repo = str(row.get("repo", ""))
        if per_repo[repo] >= max_per_repo:
            continue
        selected.append(row)
        per_repo[repo] += 1
        if len(selected) >= count:
            return selected

    selected_ids = {row.get("instance_id") for row in selected}
    for row in sorted_rows:
        if row.get("instance_id") in selected_ids:
            continue
        selected.append(row)
        if len(selected) >= count:
            return selected
    return selected


def select_by_ids(rows: list[dict[str, Any]], ids: list[str]) -> list[dict[str, Any]]:
    by_id = {row.get("instance_id"): row for row in rows}
    selected: list[dict[str, Any]] = []
    missing: list[str] = []
    for instance_id in ids:
        row = by_id.get(instance_id)
        if row is None:
            missing.append(instance_id)
        else:
            selected.append(row)
    if missing:
        raise ValueError(f"ID file references unknown instance(s): {', '.join(missing[:10])}")
    return selected


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dataset", default=DEFAULT_DATASET)
    parser.add_argument("--split", default=DEFAULT_SPLIT)
    parser.add_argument("--count", type=int, default=DEFAULT_COUNT)
    parser.add_argument("--max-per-repo", type=int, default=DEFAULT_MAX_PER_REPO)
    parser.add_argument("--source-jsonl", type=Path, help="Read already-fetched SWE-bench rows from JSONL")
    parser.add_argument("--ids", type=Path, default=Path(".lab/experiments/data/swebench_lite_30.ids.txt"))
    parser.add_argument("--refresh-ids", action="store_true")
    parser.add_argument("--output", type=Path, default=Path(".lab/experiments/data/swebench_lite_30.task_rows.jsonl"))
    parser.add_argument("--raw-output", type=Path, default=Path(".lab/experiments/data/swebench_lite_30.raw.jsonl"))
    parser.add_argument("--metadata-dir", type=Path, default=Path(".lab/experiments/data/swebench_lite_30.official_metadata"))
    parser.add_argument("--meta-output", type=Path, default=Path(".lab/experiments/data/swebench_lite_30.meta.json"))
    parser.add_argument("--benchmark-name", default="swebench_lite")
    parser.add_argument("--adapter-id", default="swebench_official_harness")
    args = parser.parse_args()

    if args.count <= 0:
        raise SystemExit("--count must be positive")
    if args.max_per_repo <= 0:
        raise SystemExit("--max-per-repo must be positive")

    rows = read_jsonl(args.source_jsonl) if args.source_jsonl else fetch_all_rows(args.dataset, args.split)
    if not rows:
        raise SystemExit("no SWE-bench rows available")

    existing_ids = [] if args.refresh_ids else load_ids(args.ids)
    selected = select_by_ids(rows, existing_ids)[: args.count] if existing_ids else deterministic_curate(rows, args.count, args.max_per_repo)
    selected = selected[: args.count]
    if len(selected) < args.count:
        raise SystemExit(f"requested {args.count} tasks but only selected {len(selected)}")

    task_rows = [
        to_task_row(row, benchmark_name=args.benchmark_name, split=args.split, adapter_id=args.adapter_id)
        for row in selected
    ]

    write_jsonl(args.output, task_rows)
    write_jsonl(args.raw_output, selected)
    write_ids(args.ids, selected)
    args.metadata_dir.mkdir(parents=True, exist_ok=True)
    for row in selected:
        write_json(args.metadata_dir / f"{require_string(row, 'instance_id')}.json", row)

    repo_distribution = Counter(require_string(row, "repo") for row in selected)
    manifest = {
        "schema_version": "swebench_agentlab_acquisition_v1",
        "dataset": args.dataset,
        "split": args.split,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "selected_rows": len(selected),
        "task_rows_path": str(args.output),
        "raw_rows_path": str(args.raw_output),
        "ids_path": str(args.ids),
        "metadata_dir": str(args.metadata_dir),
        "benchmark_name": args.benchmark_name,
        "adapter_id": args.adapter_id,
        "repo_distribution": dict(sorted(repo_distribution.items())),
        "images": sorted({row["image"] for row in task_rows}),
    }
    write_json(args.meta_output, manifest)
    print(json.dumps(manifest, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"acquire_swebench_lite.py error: {exc}", file=sys.stderr)
        raise SystemExit(1)
