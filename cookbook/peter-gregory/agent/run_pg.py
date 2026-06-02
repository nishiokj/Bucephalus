#!/usr/bin/env python3
"""Peter Gregory agent runner.

Drives a single model through one Peter Gregory case using its native
tool-calling API. The model is given the workspace README + five tools
(read_feed, search_records, read_record, calculate_exposure, submit_scan)
that wrap the workspace `tools/pg_tools.py` CLI. The loop runs until the
model calls submit_scan or hits a step limit.

Bucephalus contract:
  - reads BUCEPHALUS_TRIAL_INPUT_PATH for the trial spec (case + variant config)
  - case.inputs.scenario_id selects which workspace to materialize
  - variant.config.model + .provider pick the API
  - writes the agent's submitted scan + run trace to BUCEPHALUS_RESULT_PATH

Supports two providers:
  - anthropic (Claude *)
  - openai    (gpt-* and any model exposed via the OpenAI Responses API)
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


# ── tool surface ────────────────────────────────────────────────────

TOOL_SPECS: list[dict[str, Any]] = [
    {
        "name": "read_feed",
        "description": "Print today's external event feed. Read this first.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "search_records",
        "description": "Substring search across records/ and events/. Returns matching paths and short excerpts.",
        "input_schema": {
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
        },
    },
    {
        "name": "read_record",
        "description": "Print one workspace-relative file (e.g. records/orders.yaml).",
        "input_schema": {
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        },
    },
    {
        "name": "calculate_exposure",
        "description": (
            "Deterministic exposure helper. Given one or more entity ids (a material id, "
            "component id, or product id), returns the set of affected products / orders / "
            "customers and the total revenue at risk. Use this instead of computing exposure "
            "by hand from YAML."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "entity_ids": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                }
            },
            "required": ["entity_ids"],
        },
    },
    {
        "name": "submit_scan",
        "description": (
            "Submit your final daily scan. Pass the scan inline as `scan` (object matching "
            "output/scan_schema.json). Calling this ends your turn."
        ),
        "input_schema": {
            "type": "object",
            "properties": {"scan": {"type": "object"}},
            "required": ["scan"],
        },
    },
]


# ── pg_tools.py invocation ───────────────────────────────────────────

def _run_pg(workspace: Path, args: list[str]) -> str:
    """Invoke the workspace's pg_tools CLI; return stdout."""
    cmd = ["python3", str(workspace / "tools" / "pg_tools.py"), *args]
    proc = subprocess.run(cmd, cwd=workspace, capture_output=True, text=True, timeout=60)
    if proc.returncode != 0:
        return json.dumps({"ok": False, "error": proc.stderr.strip() or "tool failed", "stdout": proc.stdout})
    return proc.stdout


def execute_tool(name: str, args: dict[str, Any], workspace: Path) -> str:
    """Dispatch one tool call to the workspace; return text the model sees."""
    if name == "read_feed":
        return _run_pg(workspace, ["read_feed"])
    if name == "search_records":
        return _run_pg(workspace, ["search_records", str(args.get("query", ""))])
    if name == "read_record":
        return _run_pg(workspace, ["read_record", str(args.get("path", ""))])
    if name == "calculate_exposure":
        entity_ids = args.get("entity_ids") or []
        if not isinstance(entity_ids, list) or not entity_ids:
            return json.dumps({"ok": False, "error": "entity_ids must be a non-empty array"})
        flags: list[str] = ["calculate_exposure"]
        for eid in entity_ids:
            flags.extend(["--entity-id", str(eid)])
        return _run_pg(workspace, flags)
    if name == "submit_scan":
        # Persist the model's scan and signal end-of-turn through a sentinel.
        scan = args.get("scan")
        if not isinstance(scan, dict):
            return json.dumps({"ok": False, "error": "scan must be an object"})
        scan_path = workspace / "submitted_scan.json"
        scan_path.write_text(json.dumps(scan, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        submit_out = _run_pg(workspace, ["submit_scan", str(scan_path)])
        return submit_out
    return json.dumps({"ok": False, "error": f"unknown tool: {name}"})


# ── workspace materialization ────────────────────────────────────────

def materialize_workspace(scenario_id: str, source_root: Path, dest: Path) -> Path:
    """Copy the per-scenario workspace into a writable trial dir; strip the oracle."""
    src = source_root / scenario_id
    if not src.is_dir():
        raise SystemExit(f"scenario workspace not found: {src}")
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(src, dest)
    oracle = dest / ".oracle.json"
    if oracle.exists():
        oracle.unlink()  # the agent must not see the answer key
    return dest


# ── provider drivers ────────────────────────────────────────────────

def _system_prompt(readme: str) -> str:
    return (
        "You are the daily latent-exposure agent for Coastal Allied Industries. "
        "Your task brief and tool documentation are below. Walk every credible "
        "causal path explicitly. Use calculate_exposure for arithmetic. Log every "
        "headline you considered (alerted or dismissed) in your final scan. End "
        "your turn by calling submit_scan exactly once.\n\n"
        f"--- TASK BRIEF ---\n{readme}\n--- END BRIEF ---"
    )


def _user_kickoff() -> str:
    return (
        "Begin. Call read_feed first to see today's external events, then walk "
        "the latent edges, then call submit_scan with your final daily scan. "
        "Aim to dismiss noise explicitly via no_alert_paths_considered."
    )


def drive_anthropic(*, model: str, workspace: Path, max_steps: int, max_output_tokens: int) -> dict[str, Any]:
    import anthropic

    client = anthropic.Anthropic()
    readme = (workspace / "README.md").read_text(encoding="utf-8")
    messages: list[dict[str, Any]] = [{"role": "user", "content": _user_kickoff()}]
    trace: list[dict[str, Any]] = []
    submitted: dict[str, Any] | None = None
    input_tokens = output_tokens = 0

    for step in range(1, max_steps + 1):
        resp = client.messages.create(
            model=model,
            max_tokens=max_output_tokens,
            system=_system_prompt(readme),
            tools=TOOL_SPECS,
            messages=messages,
        )
        input_tokens += resp.usage.input_tokens
        output_tokens += resp.usage.output_tokens
        # Append the assistant turn for the next loop iteration.
        messages.append({"role": "assistant", "content": [block.model_dump() for block in resp.content]})
        tool_results: list[dict[str, Any]] = []
        for block in resp.content:
            if block.type == "tool_use":
                args = dict(block.input) if isinstance(block.input, dict) else {}
                tool_output = execute_tool(block.name, args, workspace)
                trace.append({"step": step, "tool": block.name, "args_keys": sorted(args.keys()), "output_preview": tool_output[:400]})
                if block.name == "submit_scan":
                    try:
                        submitted = json.loads((workspace / "submitted_scan.json").read_text(encoding="utf-8"))
                    except Exception as exc:
                        submitted = {"_parse_error": str(exc)}
                tool_results.append(
                    {"type": "tool_result", "tool_use_id": block.id, "content": tool_output}
                )
        if submitted is not None:
            break
        if not tool_results:
            # Model returned without calling any tool — treat as malformed end of turn.
            trace.append({"step": step, "warning": "no tool calls in assistant turn"})
            break
        messages.append({"role": "user", "content": tool_results})

    return {
        "provider": "anthropic",
        "model": model,
        "steps_taken": min(step, max_steps),
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "submitted": submitted,
        "trace": trace,
    }


def drive_openai(*, model: str, workspace: Path, max_steps: int, max_output_tokens: int) -> dict[str, Any]:
    from openai import OpenAI

    client = OpenAI()
    readme = (workspace / "README.md").read_text(encoding="utf-8")
    # OpenAI Responses API tool shape.
    tools = [
        {
            "type": "function",
            "name": t["name"],
            "description": t["description"],
            "parameters": t["input_schema"],
            "strict": False,
        }
        for t in TOOL_SPECS
    ]
    instructions = _system_prompt(readme)
    input_items: list[dict[str, Any]] = [
        {"role": "user", "content": [{"type": "input_text", "text": _user_kickoff()}]}
    ]
    trace: list[dict[str, Any]] = []
    submitted: dict[str, Any] | None = None
    input_tokens = output_tokens = 0

    for step in range(1, max_steps + 1):
        resp = client.responses.create(
            model=model,
            instructions=instructions,
            tools=tools,
            input=input_items,
            max_output_tokens=max_output_tokens,
        )
        input_tokens += int(getattr(resp.usage, "input_tokens", 0) or 0)
        output_tokens += int(getattr(resp.usage, "output_tokens", 0) or 0)
        any_tool_call = False
        for item in resp.output:
            d = item.model_dump() if hasattr(item, "model_dump") else dict(item)
            input_items.append(d)
            if d.get("type") in {"function_call", "tool_call"}:
                any_tool_call = True
                name = d.get("name") or ""
                try:
                    args = json.loads(d.get("arguments") or "{}")
                except Exception:
                    args = {}
                tool_output = execute_tool(name, args, workspace)
                trace.append({"step": step, "tool": name, "args_keys": sorted(args.keys()), "output_preview": tool_output[:400]})
                if name == "submit_scan":
                    try:
                        submitted = json.loads((workspace / "submitted_scan.json").read_text(encoding="utf-8"))
                    except Exception as exc:
                        submitted = {"_parse_error": str(exc)}
                input_items.append(
                    {"type": "function_call_output", "call_id": d.get("call_id") or d.get("id") or "", "output": tool_output}
                )
        if submitted is not None or not any_tool_call:
            break

    return {
        "provider": "openai",
        "model": model,
        "steps_taken": min(step, max_steps),
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "submitted": submitted,
        "trace": trace,
    }


# ── entrypoint ──────────────────────────────────────────────────────


def _read_trial() -> dict[str, Any]:
    path = os.environ.get("BUCEPHALUS_TRIAL_INPUT_PATH")
    if not path:
        raise SystemExit("BUCEPHALUS_TRIAL_INPUT_PATH not set")
    return json.loads(Path(path).read_text(encoding="utf-8"))


def _write_result(payload: dict[str, Any]) -> None:
    out = os.environ.get("BUCEPHALUS_RESULT_PATH", "/bucephalus/out/result.json")
    out_path = Path(out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--provider", default=None, help="anthropic | openai (overrides variant config)")
    parser.add_argument("--model", default=None, help="model id (overrides variant config)")
    parser.add_argument("--max-steps", type=int, default=20)
    parser.add_argument("--max-output-tokens", type=int, default=4096)
    parser.add_argument("--source-workspaces", default="/opt/peter-gregory/workspaces")
    args = parser.parse_args()

    trial = _read_trial()
    case = trial.get("case", {}) or {}
    inputs = case.get("inputs", {}) or {}
    scenario_id = inputs.get("scenario_id") or case.get("id")
    if not scenario_id:
        raise SystemExit("trial case missing inputs.scenario_id")

    variant_cfg = (trial.get("variant", {}) or {}).get("config", {}) or {}
    provider = args.provider or variant_cfg.get("provider") or "anthropic"
    model = args.model or variant_cfg.get("model") or "claude-sonnet-4-6"

    workspace_root = Path(args.source_workspaces)
    workspace = materialize_workspace(scenario_id, workspace_root, Path("/bucephalus/workspace") / scenario_id)

    started = time.perf_counter()
    if provider == "anthropic":
        run = drive_anthropic(model=model, workspace=workspace, max_steps=args.max_steps, max_output_tokens=args.max_output_tokens)
    elif provider == "openai":
        run = drive_openai(model=model, workspace=workspace, max_steps=args.max_steps, max_output_tokens=args.max_output_tokens)
    else:
        raise SystemExit(f"unknown provider: {provider}")
    run["elapsed_seconds"] = round(time.perf_counter() - started, 3)
    run["scenario_id"] = scenario_id

    _write_result(run)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
