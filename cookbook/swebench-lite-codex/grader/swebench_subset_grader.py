#!/usr/bin/env python3
"""Real SWE-bench Lite subset grader.

The grader applies the captured candidate patch, applies the official
SWE-bench test patch, and runs the official FAIL_TO_PASS and PASS_TO_PASS test
selections for the instance.
"""

from __future__ import annotations

import argparse
import ast
import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any


def read_json(path: str | Path) -> Any:
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write_json(path: str | Path, payload: Any) -> None:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def list_field(value: Any) -> list[str]:
    if isinstance(value, list):
        return [str(item) for item in value]
    if isinstance(value, str) and value.strip():
        parsed = ast.literal_eval(value)
        if isinstance(parsed, list):
            return [str(item) for item in parsed]
    return []


def run(argv: list[str], *, cwd: Path, timeout: int) -> dict[str, Any]:
    proc = subprocess.run(
        argv,
        cwd=str(cwd),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    return {
        "argv": argv,
        "returncode": proc.returncode,
        "stdout_tail": proc.stdout[-16000:],
        "stderr_tail": proc.stderr[-16000:],
    }


def apply_patch(repo: Path, patch_text: str, label: str, timeout: int) -> dict[str, Any]:
    if not patch_text.strip():
        return {"label": label, "returncode": 2, "stdout_tail": "", "stderr_tail": "empty patch"}
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", suffix=f".{label}.patch") as patch_file:
        patch_file.write(patch_text)
        patch_file.flush()
        result = run(["git", "apply", "--whitespace=nowarn", patch_file.name], cwd=repo, timeout=timeout)
    result["label"] = label
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--instance-id", required=True)
    parser.add_argument("--metadata-dir", required=True)
    parser.add_argument("--patch-file", required=True)
    parser.add_argument("--repo", default="/testbed")
    parser.add_argument("--report-output", default="/bucephalus/out/swebench_report.json")
    parser.add_argument("--timeout-seconds", type=int, default=900)
    args = parser.parse_args()

    repo = Path(args.repo)
    official = read_json(Path(args.metadata_dir) / f"{args.instance_id}.json")
    candidate_patch = Path(args.patch_file).read_text(encoding="utf-8")
    fail_to_pass = list_field(official.get("FAIL_TO_PASS"))
    pass_to_pass = list_field(official.get("PASS_TO_PASS"))
    base_commit = str(official["base_commit"])

    report: dict[str, Any] = {
        "schema_version": "swebench_subset_grader_report_v1",
        "benchmark": "SWE-bench Lite",
        "instance_id": args.instance_id,
        "repo": official.get("repo"),
        "base_commit": base_commit,
        "candidate_patch_bytes": len(candidate_patch.encode("utf-8")),
        "fail_to_pass": fail_to_pass,
        "pass_to_pass_count": len(pass_to_pass),
        "steps": [],
    }

    if not candidate_patch.strip():
        report.update({"resolved": 0.0, "failure_label": "NO_PATCH"})
        write_json(args.report_output, report)
        return 0

    for step in (
        run(["git", "reset", "--hard", base_commit], cwd=repo, timeout=args.timeout_seconds),
        run(["git", "clean", "-fdx"], cwd=repo, timeout=args.timeout_seconds),
    ):
        report["steps"].append(step)
        if step["returncode"] != 0:
            report.update({"resolved": 0.0, "failure_label": "WORKSPACE_RESET_FAILED"})
            write_json(args.report_output, report)
            return 0

    candidate_apply = apply_patch(repo, candidate_patch, "candidate", args.timeout_seconds)
    report["steps"].append(candidate_apply)
    if candidate_apply["returncode"] != 0:
        report.update({"resolved": 0.0, "failure_label": "CANDIDATE_PATCH_APPLY_FAILED"})
        write_json(args.report_output, report)
        return 0

    test_apply = apply_patch(repo, str(official.get("test_patch") or ""), "official_test", args.timeout_seconds)
    report["steps"].append(test_apply)
    if test_apply["returncode"] != 0:
        report.update({"resolved": 0.0, "failure_label": "OFFICIAL_TEST_PATCH_APPLY_FAILED"})
        write_json(args.report_output, report)
        return 0

    fail_run = run(["python", "-m", "pytest", "-q", *fail_to_pass], cwd=repo, timeout=args.timeout_seconds)
    pass_run = run(["python", "-m", "pytest", "-q", *pass_to_pass], cwd=repo, timeout=args.timeout_seconds)
    report["steps"].append({"label": "fail_to_pass", **fail_run})
    report["steps"].append({"label": "pass_to_pass", **pass_run})

    fail_to_pass_passed = fail_run["returncode"] == 0
    pass_to_pass_passed = pass_run["returncode"] == 0
    resolved = fail_to_pass_passed and pass_to_pass_passed
    report.update(
        {
            "fail_to_pass_passed": fail_to_pass_passed,
            "pass_to_pass_passed": pass_to_pass_passed,
            "resolved": 1.0 if resolved else 0.0,
            "failure_label": None if resolved else "TESTS_FAILED",
        }
    )
    write_json(args.report_output, report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

