#!/usr/bin/env python3
"""Run Codex CLI headlessly against a SWE-bench workspace."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any


def env_first(*names: str) -> str | None:
    for name in names:
        value = os.environ.get(name)
        if value:
            return value
    return None


def read_json(path: str | Path) -> Any:
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write_json(path: str | Path, payload: Any) -> None:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def find_case_payload(trial: dict[str, Any]) -> dict[str, Any]:
    for key in ("case", "task"):
        value = trial.get(key)
        if isinstance(value, dict):
            return value
    return {}


def find_prompt(case: dict[str, Any]) -> str:
    candidates = [
        case.get("prompt"),
        case.get("input", {}).get("prompt") if isinstance(case.get("input"), dict) else None,
        case.get("inputs", {}).get("prompt") if isinstance(case.get("inputs"), dict) else None,
        case.get("task", {}).get("input", {}).get("prompt")
        if isinstance(case.get("task"), dict) and isinstance(case.get("task", {}).get("input"), dict)
        else None,
    ]
    for candidate in candidates:
        if isinstance(candidate, str) and candidate.strip():
            return candidate.strip()
    raise RuntimeError("trial input did not contain a SWE-bench prompt")


def find_instance_id(case: dict[str, Any]) -> str:
    candidates = [
        case.get("instance_id"),
        case.get("inputs", {}).get("instance_id") if isinstance(case.get("inputs"), dict) else None,
        case.get("input", {}).get("instance_id") if isinstance(case.get("input"), dict) else None,
        case.get("metadata", {}).get("swebench", {}).get("instance_id")
        if isinstance(case.get("metadata"), dict) and isinstance(case.get("metadata", {}).get("swebench"), dict)
        else None,
        case.get("task", {}).get("swebench", {}).get("input", {}).get("instance_id")
        if isinstance(case.get("task"), dict)
        and isinstance(case.get("task", {}).get("swebench"), dict)
        and isinstance(case.get("task", {}).get("swebench", {}).get("input"), dict)
        else None,
    ]
    for candidate in candidates:
        if isinstance(candidate, str) and candidate.strip():
            return candidate.strip()
    return "unknown"


def build_prompt(instance_id: str, issue_prompt: str) -> str:
    return f"""You are solving a real SWE-bench Lite task.

Instance: {instance_id}
Repository workspace: /testbed

Fix the bug described below by editing files in /testbed. Do not create a
separate patch file; make the actual source changes in the git workspace. After
editing, run the most relevant focused tests you can. Leave the workspace with
only the intended source/test changes.

Issue:
{issue_prompt}
"""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default=os.environ.get("CODEX_MODEL", "gpt-5-codex"))
    parser.add_argument("--codex-bin", default=os.environ.get("CODEX_BIN", "codex"))
    parser.add_argument("--workdir", default=os.environ.get("BUCEPHALUS_WORKSPACE", "/testbed"))
    args = parser.parse_args()

    input_path = env_first("BUCEPHALUS_TRIAL_INPUT_PATH", "AGENTLAB_TRIAL_INPUT_PATH")
    output_path = env_first("BUCEPHALUS_RESULT_PATH", "AGENTLAB_RESULT_PATH") or "/bucephalus/out/result.json"
    if not input_path:
        raise RuntimeError("BUCEPHALUS_TRIAL_INPUT_PATH is required")

    trial = read_json(input_path)
    case = find_case_payload(trial if isinstance(trial, dict) else {})
    instance_id = find_instance_id(case)
    prompt = build_prompt(instance_id, find_prompt(case))

    command = [
        args.codex_bin,
        "exec",
        "--full-auto",
        "-m",
        args.model,
        prompt,
    ]
    # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-tainted-env-args.dangerous-subprocess-use-tainted-env-args
    proc = subprocess.run(
        command,
        cwd=args.workdir,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=int(os.environ.get("CODEX_TIMEOUT_SECONDS", "900")),
    )

    write_json(
        output_path,
        {
            "agent": "codex_cli",
            "mode": "headless_exec",
            "model": args.model,
            "instance_id": instance_id,
            "exit_code": proc.returncode,
            "stdout_tail": proc.stdout[-12000:],
            "stderr_tail": proc.stderr[-12000:],
        },
    )
    return proc.returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        output_path = env_first("BUCEPHALUS_RESULT_PATH", "AGENTLAB_RESULT_PATH") or "/bucephalus/out/result.json"
        write_json(output_path, {"agent": "codex_cli", "error": str(exc)})
        print(f"run_codex.py error: {exc}", file=sys.stderr)
        raise SystemExit(1)
