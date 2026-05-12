#!/usr/bin/env python3
"""Convert AgentLab patches to official SWE-bench evals and AgentLab scores."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_HARNESS_PYTHON = REPO_ROOT / ".lab/swebench-harness-py312-venv/bin/python"
DEFAULT_HARNESS_SOURCE = REPO_ROOT / ".lab/upstream/SWE-bench"
SWEBENCH_CANDIDATE_SCOPE_POLICY = "swebench_candidate_source_patch_v1"

EXCLUDED_CANDIDATE_PATHS = {
    "Pipfile",
    "Pipfile.lock",
    "poetry.lock",
    "pyproject.toml",
    "requirements.txt",
    "requirements-dev.txt",
    "setup.cfg",
    "tox.ini",
}
EXCLUDED_CANDIDATE_PREFIXES = (
    ".agentlab/",
    ".haiku/",
    ".lab/",
    ".pytest_cache/",
    "logs/",
    "out/",
)


@dataclass(frozen=True)
class PredictionContext:
    trial_dir: Path
    ids: dict[str, Any]
    benchmark: dict[str, Any]
    instance_id: str
    patch: str
    patch_source: str
    patch_scope: dict[str, Any]
    schedule_idx: int
    row_seq: int
    slot_commit_id: str
    attempt: int

    @property
    def variant_id(self) -> str:
        return str(self.ids.get("variant_id") or "variant_unknown")


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def write_jsonl(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "".join(json.dumps(row, separators=(",", ":"), sort_keys=True) + "\n" for row in rows),
        encoding="utf-8",
    )


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def docker_safe_token(value: Any) -> str:
    token = re.sub(r"[^A-Za-z0-9_.-]+", "_", str(value or "").strip())
    token = token.strip("._-")
    return token or "unknown"


def swebench_run_id(contexts: list[PredictionContext], *, variant_id: str, output_dir: Path) -> str:
    if len(contexts) == 1:
        context = contexts[0]
        return "_".join(
            docker_safe_token(part)
            for part in (
                context.ids.get("run_id"),
                context.ids.get("trial_id"),
                context.ids.get("variant_id") or variant_id,
                context.instance_id,
            )
        )
    run_id = contexts[0].ids.get("run_id") if contexts else output_dir.parent.name
    return "_".join(
        docker_safe_token(part)
        for part in (
            run_id,
            output_dir.name,
            variant_id,
        )
    )


def load_optional_json(path: Path) -> Any:
    if not path.exists():
        return {}
    return read_json(path)


def extract_patch(result: dict[str, Any]) -> str | None:
    if result.get("schema_version") == "artifact_envelope_v1":
        if result.get("artifact_type") != "patch_submission":
            return None
        artifact = result.get("artifact")
        if isinstance(artifact, dict) and isinstance(artifact.get("patch"), str):
            return artifact["patch"]
        return None

    artifact = result.get("artifact")
    if isinstance(artifact, dict) and isinstance(artifact.get("patch"), str):
        return artifact["patch"]

    answer = result.get("answer")
    if isinstance(answer, dict) and isinstance(answer.get("patch"), str):
        return answer["patch"]
    if isinstance(answer, str) and answer.lstrip().startswith("diff --git"):
        return answer
    return None


def strip_git_prefix(path: str) -> str:
    path = path.strip()
    if path in {"", "/dev/null"}:
        return ""
    if len(path) > 2 and path[1] == "/" and path[0] in {"a", "b"}:
        return path[2:]
    return path


def diff_header_paths(header: str) -> list[str]:
    parts = header.split()
    if len(parts) < 4 or parts[0] != "diff" or parts[1] != "--git":
        return []
    paths = [strip_git_prefix(parts[2]), strip_git_prefix(parts[3])]
    return [path for path in paths if path]


def candidate_path_reason(path: str) -> str | None:
    normalized = path
    while normalized.startswith("./"):
        normalized = normalized[2:]
    parts = [part for part in normalized.split("/") if part]
    basename = parts[-1] if parts else normalized

    if not normalized:
        return "empty_path"
    if normalized in EXCLUDED_CANDIDATE_PATHS:
        return "dependency_or_tooling_metadata"
    if any(normalized.startswith(prefix) for prefix in EXCLUDED_CANDIDATE_PREFIXES):
        return "agentlab_or_generated_output"
    if any(part in {"test", "tests"} for part in parts):
        return "test_file"
    if basename.startswith("test_") and basename.endswith(".py"):
        return "test_file"
    if basename.endswith("_test.py"):
        return "test_file"
    return None


def scope_swebench_candidate_patch(patch_text: str) -> tuple[str, dict[str, Any]]:
    diagnostics: dict[str, Any] = {
        "policy": SWEBENCH_CANDIDATE_SCOPE_POLICY,
        "included_files": [],
        "excluded_files": [],
        "raw_bytes": len(patch_text.encode("utf-8")),
        "scoped_bytes": 0,
    }
    if not patch_text.strip():
        return "", diagnostics

    blocks: list[list[str]] = []
    current: list[str] = []
    for line in patch_text.splitlines(keepends=True):
        if line.startswith("diff --git "):
            if current:
                blocks.append(current)
            current = [line]
        elif current:
            current.append(line)
    if current:
        blocks.append(current)

    if not blocks:
        diagnostics["excluded_files"].append({"path": "<unknown>", "reason": "not_git_diff"})
        return "", diagnostics

    included_blocks: list[str] = []
    included_files: list[str] = []
    excluded_files: list[dict[str, str]] = []
    for block in blocks:
        paths = diff_header_paths(block[0])
        reason = None
        for path in paths:
            reason = candidate_path_reason(path)
            if reason is not None:
                break
        display_path = paths[-1] if paths else "<unknown>"
        if reason is None:
            included_blocks.append("".join(block))
            included_files.extend(paths)
        else:
            excluded_files.append({"path": display_path, "reason": reason})

    scoped_patch = "".join(included_blocks)
    diagnostics["included_files"] = sorted(set(included_files))
    diagnostics["excluded_files"] = excluded_files
    diagnostics["scoped_bytes"] = len(scoped_patch.encode("utf-8"))
    return scoped_patch, diagnostics


def run_capture(argv: list[str], *, cwd: Path) -> str:
    proc = subprocess.run(
        argv,
        cwd=str(cwd),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return proc.stdout


def extract_workspace_patch(trial_dir: Path) -> str | None:
    workspace = trial_dir / "workspace"
    if not (workspace / ".git").is_dir():
        return None
    pathspec = [".", ":(exclude).agentlab", ":(exclude).haiku", ":(exclude).lab", ":(exclude)logs", ":(exclude)out"]
    subprocess.run(
        ["git", "add", "-N", "--", *pathspec],
        cwd=str(workspace),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return run_capture(["git", "diff", "--binary", "--", *pathspec], cwd=workspace)


def task_from_trial_input(trial_input: dict[str, Any]) -> dict[str, Any]:
    task = trial_input.get("task")
    return task if isinstance(task, dict) else {}


def extract_instance_id(task: dict[str, Any], result: dict[str, Any]) -> str | None:
    candidates = [
        task.get("swebench", {}).get("input", {}).get("instance_id"),
        result.get("metadata", {}).get("instance_id"),
    ]
    for candidate in candidates:
        if isinstance(candidate, str) and candidate.strip():
            return candidate.strip()
    return None


def extract_benchmark(task: dict[str, Any]) -> dict[str, Any]:
    candidate = task.get("benchmark")
    if isinstance(candidate, dict):
        return {
            "adapter_id": str(candidate.get("adapter_id") or "swebench_official_harness"),
            "name": str(candidate.get("name") or "swebench_lite"),
            "split": str(candidate.get("split") or "test"),
        }
    return {
        "adapter_id": "swebench_official_harness",
        "name": "swebench_lite",
        "split": "test",
    }


def extract_ids(trial_dir: Path, trial_input: dict[str, Any], metadata: dict[str, Any]) -> dict[str, Any]:
    ids = trial_input.get("ids")
    if not isinstance(ids, dict):
        ids = metadata.get("ids") if isinstance(metadata.get("ids"), dict) else {}
    return {
        "run_id": str(ids.get("run_id") or trial_dir.parents[1].name),
        "trial_id": str(ids.get("trial_id") or trial_dir.name),
        "variant_id": str(ids.get("variant_id") or "variant_unknown"),
        "task_id": str(ids.get("task_id") or "task_unknown"),
        "repl_idx": int(ids.get("repl_idx") or 0),
    }


def extract_schedule_idx(metadata: dict[str, Any], fallback: int) -> int:
    ids = metadata.get("ids")
    if isinstance(ids, dict) and isinstance(ids.get("task_index"), int):
        return int(ids["task_index"])
    return fallback


def load_slot_commit_id(trial_dir: Path, fallback: str) -> str:
    candidates = [
        trial_dir / "trial_state.json",
        trial_dir / "state/lab_control.json",
    ]
    for path in candidates:
        payload = load_optional_json(path)
        if isinstance(payload, dict):
            for key in ("slot_commit_id", "commit_id"):
                value = payload.get(key)
                if isinstance(value, str) and value.strip():
                    return value.strip()
    return fallback


def result_path_for_trial(trial_dir: Path) -> Path | None:
    for candidate in (trial_dir / "out/result.json", trial_dir / "result.json"):
        if candidate.exists():
            return candidate
    return None


def collect_predictions(run_dir: Path, *, require_all_trials: bool = True) -> list[PredictionContext]:
    trial_dirs = sorted((run_dir / "trials").glob("trial_*"))
    if not trial_dirs:
        raise RuntimeError(f"no trial directories found under {run_dir / 'trials'}")

    contexts: list[PredictionContext] = []
    missing: list[str] = []
    for row_seq, trial_dir in enumerate(trial_dirs):
        result_path = result_path_for_trial(trial_dir)
        trial_input_path = trial_dir / "in/trial_input.json"
        if result_path is None or not trial_input_path.exists():
            missing.append(f"{trial_dir.name}: missing result or trial input")
            continue

        result = read_json(result_path)
        trial_input = read_json(trial_input_path)
        metadata = load_optional_json(trial_dir / "trial_metadata.json")
        if not isinstance(result, dict) or not isinstance(trial_input, dict):
            missing.append(f"{trial_dir.name}: result/trial input must be objects")
            continue
        if not isinstance(metadata, dict):
            metadata = {}

        task = task_from_trial_input(trial_input)
        patch = extract_patch(result)
        patch_source = "result_artifact"
        if patch is None:
            patch = extract_workspace_patch(trial_dir)
            patch_source = "retained_workspace_diff"
        instance_id = extract_instance_id(task, result)
        if not instance_id:
            missing.append(f"{trial_dir.name}: missing SWE-bench instance_id")
            continue
        if patch is None:
            missing.append(f"{trial_dir.name}: missing patch_submission and retained git workspace")
            continue
        patch, patch_scope = scope_swebench_candidate_patch(patch)

        contexts.append(
            PredictionContext(
                trial_dir=trial_dir,
                ids=extract_ids(trial_dir, trial_input, metadata),
                benchmark=extract_benchmark(task),
                instance_id=instance_id,
                patch=patch,
                patch_source=patch_source,
                patch_scope=patch_scope,
                schedule_idx=extract_schedule_idx(metadata, row_seq),
                row_seq=row_seq,
                slot_commit_id=load_slot_commit_id(trial_dir, "post_run_official_swebench_eval"),
                attempt=1,
            )
        )

    if missing and require_all_trials:
        raise RuntimeError("cannot build official SWE-bench predictions:\n  " + "\n  ".join(missing))
    if not contexts:
        raise RuntimeError("no patch predictions found in run")
    return contexts


def swebench_prediction_row(context: PredictionContext) -> dict[str, Any]:
    return {
        "instance_id": context.instance_id,
        "model_name_or_path": context.variant_id,
        "model_patch": context.patch,
    }


def agentlab_prediction_record(context: PredictionContext) -> dict[str, Any]:
    return {
        "schema_version": "benchmark_prediction_record_v1",
        "schedule_idx": context.schedule_idx,
        "slot_commit_id": context.slot_commit_id,
        "attempt": context.attempt,
        "row_seq": context.row_seq,
        "ts": now_iso(),
        "ids": context.ids,
        "benchmark": context.benchmark,
        "prediction": {
            "kind": "patch",
            "value": context.patch,
            "metadata": {
                "swebench_instance_id": context.instance_id,
                "candidate_patch_source": context.patch_source,
                "candidate_patch_scope": context.patch_scope,
            },
        },
        "ext": {
            "swebench": {
                "instance_id": context.instance_id,
                "prediction_format": "official_swebench",
            }
        },
    }


def resolve_docker_host(environ: dict[str, str]) -> str | None:
    explicit = environ.get("DOCKER_HOST")
    if explicit:
        return explicit
    try:
        completed = subprocess.run(
            [
                "docker",
                "context",
                "inspect",
                "--format",
                "{{json .Endpoints.docker.Host}}",
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    raw = completed.stdout.strip()
    if not raw:
        return None
    try:
        host = json.loads(raw)
    except json.JSONDecodeError:
        host = raw.strip('"')
    return host if isinstance(host, str) and host else None


def run_harness(
    *,
    harness_python: Path,
    harness_source: Path,
    predictions_path: Path,
    instance_ids: list[str],
    variant_id: str,
    harness_run_id: str,
    output_dir: Path,
    dataset_name: str,
    split: str,
    namespace: str,
    timeout: int,
    max_workers: int,
) -> list[str]:
    env = os.environ.copy()
    env["PYTHONPATH"] = str(harness_source)
    if docker_host := resolve_docker_host(env):
        env["DOCKER_HOST"] = docker_host
    cmd = [
        str(harness_python),
        "-m",
        "swebench.harness.run_evaluation",
        "--dataset_name",
        dataset_name,
        "--split",
        split,
        "--predictions_path",
        str(predictions_path),
        "--instance_ids",
        *instance_ids,
        "--max_workers",
        str(max_workers),
        "--timeout",
        str(timeout),
        "--run_id",
        harness_run_id,
        "--namespace",
        namespace,
        "--cache_level",
        "instance",
        "--clean",
        "false",
        "--report_dir",
        str(output_dir),
    ]
    subprocess.run(cmd, cwd=str(output_dir), env=env, check=True)
    return cmd


def as_string_set(value: Any) -> set[str]:
    if isinstance(value, list):
        return {item for item in value if isinstance(item, str)}
    if isinstance(value, dict):
        return {key for key, enabled in value.items() if enabled and isinstance(key, str)}
    return set()


def load_report(path: Path) -> dict[str, Any]:
    payload = read_json(path)
    if not isinstance(payload, dict):
        raise ValueError(f"official report is not a JSON object: {path}")
    return payload


def report_candidates(variant_dir: Path) -> list[Path]:
    ignored = {
        "predictions.jsonl",
        "agentlab_predictions.jsonl",
        "agentlab_scores.jsonl",
        "manifest.json",
    }
    return [
        path
        for path in sorted(variant_dir.rglob("*.json"))
        if path.name not in ignored and not path.name.startswith("agentlab_")
    ]


def find_official_report(variant_dir: Path, instance_ids: set[str]) -> tuple[Path, dict[str, Any]]:
    candidates = report_candidates(variant_dir)
    best: tuple[Path, dict[str, Any]] | None = None
    best_score = -1
    for path in candidates:
        try:
            payload = load_report(path)
        except Exception:
            continue
        mentioned = 0
        for value in payload.values():
            if isinstance(value, list):
                mentioned += len(instance_ids.intersection(item for item in value if isinstance(item, str)))
            elif isinstance(value, dict):
                mentioned += len(instance_ids.intersection(key for key in value.keys() if isinstance(key, str)))
        if mentioned > best_score:
            best = (path, payload)
            best_score = mentioned
    if best is None:
        raise RuntimeError(f"no official SWE-bench JSON report found under {variant_dir}")
    return best


def verdict_from_report(report: dict[str, Any], instance_id: str) -> tuple[str, float, dict[str, Any]]:
    resolved = as_string_set(report.get("resolved_instances")) | as_string_set(report.get("resolved_ids"))
    unresolved = as_string_set(report.get("unresolved_instances")) | as_string_set(report.get("unresolved_ids"))
    empty = as_string_set(report.get("empty_patch_instances")) | as_string_set(
        report.get("empty_patch_ids")
    )
    errored = as_string_set(report.get("error_instances")) | as_string_set(report.get("errored_instances"))
    completed = as_string_set(report.get("completed_instances"))

    ext = {
        "resolved_instances_key_present": bool(resolved),
        "official_status": None,
    }
    if instance_id in resolved:
        ext["official_status"] = "resolved"
        return "pass", 1.0, ext
    if instance_id in empty:
        ext["official_status"] = "empty_patch"
        return "missing", 0.0, ext
    if instance_id in errored:
        ext["official_status"] = "error"
        return "error", 0.0, ext
    if instance_id in unresolved or instance_id in completed:
        ext["official_status"] = "unresolved"
        return "fail", 0.0, ext
    ext["official_status"] = "absent_from_report"
    return "error", 0.0, ext


def agentlab_score_record(
    context: PredictionContext,
    *,
    report_path: Path | None,
    report: dict[str, Any] | None,
    evaluator_command: list[str],
    error: str | None = None,
) -> dict[str, Any]:
    if error is not None:
        verdict = "error"
        value = 0.0
        status_ext: dict[str, Any] = {"official_status": "adapter_error"}
    else:
        assert report is not None
        verdict, value, status_ext = verdict_from_report(report, context.instance_id)

    record: dict[str, Any] = {
        "schema_version": "benchmark_score_record_v1",
        "schedule_idx": context.schedule_idx,
        "slot_commit_id": context.slot_commit_id,
        "attempt": context.attempt,
        "row_seq": context.row_seq,
        "ts": now_iso(),
        "ids": context.ids,
        "benchmark": context.benchmark,
        "verdict": verdict,
        "primary_metric_name": "resolved",
        "primary_metric_value": value,
        "metrics": {
            "resolved": value,
        },
        "evaluator": {
            "name": "swebench.harness.run_evaluation",
            "mode": "official",
            "command": evaluator_command,
        },
        "ext": {
            "swebench": {
                "instance_id": context.instance_id,
                **status_ext,
            }
        },
    }
    if report_path is not None:
        record["ext"]["swebench"]["report_path"] = str(report_path)
    if error is not None:
        record["error"] = {
            "error_type": "OfficialSwebenchAdapterError",
            "message": error,
        }
    return record


def write_variant_predictions(variant_dir: Path, contexts: list[PredictionContext]) -> tuple[Path, Path]:
    swebench_path = variant_dir / "predictions.jsonl"
    agentlab_path = variant_dir / "agentlab_predictions.jsonl"
    write_jsonl(swebench_path, [swebench_prediction_row(context) for context in contexts])
    write_jsonl(agentlab_path, [agentlab_prediction_record(context) for context in contexts])
    return swebench_path, agentlab_path


def required_env_path(name: str) -> Path:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"missing required env var: {name}")
    return Path(value)


def map_grader_visible_path(path: str) -> Path:
    raw = path.strip()
    if raw.startswith("/agentlab/in/"):
        return required_env_path("AGENTLAB_CONTRACT_IN_HOST") / raw.removeprefix("/agentlab/in/")
    if raw.startswith("/agentlab/out/"):
        return required_env_path("AGENTLAB_CONTRACT_OUT_HOST") / raw.removeprefix("/agentlab/out/")
    return Path(raw)


def extract_patch_from_grader_input(grader_input: dict[str, Any]) -> tuple[str, str]:
    candidate = grader_input.get("candidate_artifact")
    if isinstance(candidate, dict) and candidate.get("state") == "valid":
        payload = candidate.get("payload")
        if isinstance(payload, dict):
            patch = payload.get("patch")
            if isinstance(patch, str) and patch.strip():
                return patch, "candidate_artifact"

    workspace_delta = grader_input.get("workspace_delta")
    if isinstance(workspace_delta, dict):
        patch_path = workspace_delta.get("patch_path")
        if isinstance(patch_path, str) and patch_path.strip():
            host_path = map_grader_visible_path(patch_path)
            if host_path.exists():
                return host_path.read_text(encoding="utf-8"), "workspace_delta.patch_path"
    return "", "missing"


def context_from_grader_input(grader_input: dict[str, Any]) -> PredictionContext:
    task = grader_input.get("task")
    if not isinstance(task, dict):
        raise RuntimeError("grader input task must be an object")
    result_payload = {}
    result_path = os.environ.get("AGENTLAB_RESULT_PATH", "").strip()
    if result_path and Path(result_path).exists():
        loaded = read_json(Path(result_path))
        if isinstance(loaded, dict):
            result_payload = loaded
    instance_id = extract_instance_id(task, result_payload)
    if not instance_id:
        raise RuntimeError("grader input is missing SWE-bench instance_id")
    patch, patch_source = extract_patch_from_grader_input(grader_input)
    patch, patch_scope = scope_swebench_candidate_patch(patch)
    ids = grader_input.get("ids") if isinstance(grader_input.get("ids"), dict) else {}
    return PredictionContext(
        trial_dir=Path(os.environ.get("AGENTLAB_TRIAL_DIR", ".")),
        ids={
            "run_id": str(ids.get("run_id") or "run_unknown"),
            "trial_id": str(ids.get("trial_id") or "trial_unknown"),
            "variant_id": str(ids.get("variant_id") or "variant_unknown"),
            "task_id": str(ids.get("task_id") or task.get("id") or "task_unknown"),
            "repl_idx": int(ids.get("repl_idx") or 0),
        },
        benchmark=extract_benchmark(task),
        instance_id=instance_id,
        patch=patch,
        patch_source=patch_source,
        patch_scope=patch_scope,
        schedule_idx=int(ids.get("schedule_idx") or 0),
        row_seq=0,
        slot_commit_id="runner_grading_phase",
        attempt=1,
    )


def trial_conclusion_from_score(
    context: PredictionContext,
    *,
    report_path: Path | None,
    report: dict[str, Any],
    evaluator_command: list[str],
) -> dict[str, Any]:
    verdict, value, status_ext = verdict_from_report(report, context.instance_id)
    return {
        "schema_version": "trial_conclusion_v1",
        "payload": {
            "benchmark": context.benchmark,
            "ids": context.ids,
            "task_id": context.ids.get("task_id") or "task_unknown",
            "verdict": verdict,
            "resolved": value,
            "metrics": {
                "success": value,
                "resolved": value,
            },
            "candidate_patch_source": context.patch_source,
            "candidate_patch_scope": context.patch_scope,
            "evaluator_command": evaluator_command,
            "swebench": {
                "instance_id": context.instance_id,
                **status_ext,
                "report_path": str(report_path) if report_path else None,
            },
        },
        "reported_outcome": "success" if verdict == "pass" else "failure",
        "primary_metric": {
            "name": "success",
            "value": value,
        },
        "grader": {
            "name": "swebench.harness.run_evaluation",
            "strategy": "host",
            "version": "official",
        },
    }


def grade_from_grader_input(args: argparse.Namespace) -> int:
    grader_input_path = required_env_path("AGENTLAB_GRADER_INPUT_PATH")
    raw_output_path = required_env_path("AGENTLAB_RAW_GRADER_OUTPUT_PATH")
    mapped_output_path = required_env_path("AGENTLAB_MAPPED_GRADER_OUTPUT_PATH")
    if not args.skip_harness:
        if not args.harness_python.exists():
            raise SystemExit(f"official SWE-bench harness python not found: {args.harness_python}")
        if not args.harness_source.exists():
            raise SystemExit(f"official SWE-bench source checkout not found: {args.harness_source}")

    grader_input = read_json(grader_input_path)
    if not isinstance(grader_input, dict):
        raise RuntimeError("grader input must be a JSON object")
    context = context_from_grader_input(grader_input)
    output_dir = (args.output_dir or raw_output_path.parent / "official_swebench_eval").resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    predictions_path, agentlab_predictions_path = write_variant_predictions(output_dir, [context])

    evaluator_command: list[str] = []
    report_path: Path | None = None
    report: dict[str, Any] = {}
    if args.skip_harness:
        report = {"unresolved_instances": [context.instance_id]}
    else:
        evaluator_command = run_harness(
            harness_python=args.harness_python,
            harness_source=args.harness_source,
            predictions_path=predictions_path,
            instance_ids=[context.instance_id],
            variant_id=context.variant_id,
            harness_run_id=swebench_run_id(
                [context],
                variant_id=context.variant_id,
                output_dir=output_dir,
            ),
            output_dir=output_dir,
            dataset_name=args.dataset_name,
            split=args.split,
            namespace=args.namespace,
            timeout=args.timeout,
            max_workers=args.max_workers,
        )
        report_path, report = find_official_report(output_dir, {context.instance_id})

    raw = {
        "schema_version": "official_swebench_grader_raw_v1",
        "prediction_path": str(predictions_path),
        "agentlab_prediction_path": str(agentlab_predictions_path),
        "report_path": str(report_path) if report_path else None,
        "report": report,
        "candidate_patch_source": context.patch_source,
        "candidate_patch_scope": context.patch_scope,
        "evaluator_command": evaluator_command,
    }
    write_json(raw_output_path, raw)
    write_json(
        mapped_output_path,
        trial_conclusion_from_score(
            context,
            report_path=report_path,
            report=report,
            evaluator_command=evaluator_command,
        ),
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path, nargs="?")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--dataset-name", default="princeton-nlp/SWE-bench_Lite")
    parser.add_argument("--split", default="test")
    parser.add_argument("--namespace", default="swebench")
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--max-workers", type=int, default=1)
    parser.add_argument("--harness-python", type=Path, default=DEFAULT_HARNESS_PYTHON)
    parser.add_argument("--harness-source", type=Path, default=DEFAULT_HARNESS_SOURCE)
    parser.add_argument("--skip-harness", action="store_true", help="Only write prediction files")
    parser.add_argument("--allow-missing-trials", action="store_true")
    parser.add_argument("--grader-input", action="store_true", help="Run as an AgentLab trial grader")
    args = parser.parse_args()

    if args.grader_input:
        return grade_from_grader_input(args)
    if args.run_dir is None:
        raise SystemExit("run_dir is required unless --grader-input is set")
    run_dir = args.run_dir.resolve()
    if not (run_dir / "trials").is_dir():
        raise SystemExit(f"not an AgentLab run directory: {run_dir}")
    if not args.skip_harness:
        if not args.harness_python.exists():
            raise SystemExit(f"official SWE-bench harness python not found: {args.harness_python}")
        if not args.harness_source.exists():
            raise SystemExit(f"official SWE-bench source checkout not found: {args.harness_source}")

    output_dir = (args.output_dir or run_dir / "official_swebench_eval").resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    contexts = collect_predictions(run_dir, require_all_trials=not args.allow_missing_trials)

    by_variant: dict[str, list[PredictionContext]] = defaultdict(list)
    for context in contexts:
        by_variant[context.variant_id].append(context)

    manifest: dict[str, Any] = {
        "schema_version": "official_swebench_eval_manifest_v1",
        "run_dir": str(run_dir),
        "output_dir": str(output_dir),
        "dataset_name": args.dataset_name,
        "split": args.split,
        "variants": {},
    }
    aggregate_predictions: list[dict[str, Any]] = []
    aggregate_scores: list[dict[str, Any]] = []

    for variant_id, variant_contexts in sorted(by_variant.items()):
        variant_dir = output_dir / variant_id
        variant_dir.mkdir(parents=True, exist_ok=True)
        predictions_path, agentlab_predictions_path = write_variant_predictions(variant_dir, variant_contexts)
        aggregate_predictions.extend(agentlab_prediction_record(context) for context in variant_contexts)

        instance_ids = [context.instance_id for context in variant_contexts]
        evaluator_command: list[str] = []
        scores_path = variant_dir / "agentlab_scores.jsonl"
        report_path: Path | None = None
        if args.skip_harness:
            scores: list[dict[str, Any]] = []
        else:
            evaluator_command = run_harness(
                harness_python=args.harness_python,
                harness_source=args.harness_source,
                predictions_path=predictions_path,
                instance_ids=instance_ids,
                variant_id=variant_id,
                harness_run_id=swebench_run_id(
                    variant_contexts,
                    variant_id=variant_id,
                    output_dir=variant_dir,
                ),
                output_dir=variant_dir,
                dataset_name=args.dataset_name,
                split=args.split,
                namespace=args.namespace,
                timeout=args.timeout,
                max_workers=args.max_workers,
            )
            report_path, report = find_official_report(variant_dir, set(instance_ids))
            scores = [
                agentlab_score_record(
                    context,
                    report_path=report_path,
                    report=report,
                    evaluator_command=evaluator_command,
                )
                for context in variant_contexts
            ]
            write_jsonl(scores_path, scores)
            aggregate_scores.extend(scores)

        manifest["variants"][variant_id] = {
            "prediction_count": len(variant_contexts),
            "instance_ids": instance_ids,
            "swebench_predictions_path": str(predictions_path),
            "agentlab_predictions_path": str(agentlab_predictions_path),
            "agentlab_scores_path": str(scores_path) if not args.skip_harness else None,
            "official_report_path": str(report_path) if report_path else None,
        }

    write_json(output_dir / "manifest.json", manifest)
    write_jsonl(output_dir / "agentlab_predictions.jsonl", aggregate_predictions)
    if aggregate_scores:
        write_jsonl(output_dir / "agentlab_scores.jsonl", aggregate_scores)
    print(json.dumps(manifest, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"run_official_swebench_eval_from_agentlab.py error: {exc}", file=sys.stderr)
        raise SystemExit(1)
