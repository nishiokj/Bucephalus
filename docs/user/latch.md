# Tier-1 Latch Host Protocol

Latch is the local host runner for Tier 1. It deliberately accepts a resolved
case manifest rather than benchmark authoring input:

```text
resolved latch manifest in -> local case workspaces -> latch result bundle out
```

The benchmark registry/API decides what a benchmark means. The host runner only
knows how to materialize file-staged cases, run one headless command per case,
and capture the outcome.

## Commands

Install the local daemon service and register the bundled MCP adapter:

```bash
bucephalus setup
```

The setup command installs a launchd user service on macOS or a systemd user
service on Linux. It also registers `bucephalus mcp` with detected MCP clients.
Use `--client claude-code`, `--client claude-desktop`, or
`--client cursor-project --project <dir>` to target a specific client.

Create a local demo manifest:

```bash
bucephalus latch demo --out /tmp/buc-latch-demo
```

Validate a resolved manifest:

```bash
bucephalus latch validate /tmp/buc-latch-demo/manifest.json
```

Run it directly:

```bash
bucephalus latch run /tmp/buc-latch-demo/manifest.json --json
```

Run the local resolver-shaped smoke fixture:

```bash
bucephalus latch smoke --out /tmp/buc-latch-smoke --json
```

Run the local daemon:

```bash
bucephalus daemon
```

The MCP adapter starts the daemon on demand and talks to it through the local
control protocol. The daemon methods are `status`, `start`, `progress`,
`cancel`, and `tail`.

For end-to-end UX rehearsal, MCP also exposes `latch_smoke_test`. It resolves
the local `local:file-edit-smoke` fixture into a normal `latch_manifest_v1`, then
starts that manifest through the daemon. The resolver artifact is
[latch_local_resolution_v1](../../schemas/latch_local_resolution_v1.jsonschema).
It is intentionally marked `resolver.kind: local_fixture`; it is not a hidden
cloud API shim and it does not alter runner behavior after manifest creation.

## Manifest

The manifest schema is [latch_manifest_v1](../../schemas/latch_manifest_v1.jsonschema).

Minimal shape:

```json
{
  "schema_version": "latch_manifest_v1",
  "defaults": {
    "launch": {
      "argv": ["codex", "exec", "{TASK_PROMPT}"],
      "task_injection": "argv",
      "cwd": "workspace"
    },
    "workspace_seed": { "kind": "files", "path": "seed" }
  },
  "cases": [
    {
      "case_id": "case-1",
      "task_prompt": "Edit the workspace to satisfy this case."
    }
  ]
}
```

Supported workspace seeds:

| Kind | Meaning |
| --- | --- |
| `empty` | Start from an empty workspace. |
| `files` | Copy a file or directory into the workspace. |
| `archive` | Unpack a tar or tar.gz archive into the workspace. |
| `git` | Clone a repository and optionally checkout `rev`. |

Supported task injection:

| Mode | Behavior |
| --- | --- |
| `argv` | Replaces `{TASK_PROMPT}` and `{TASK_FILE}` in argv entries. |
| `stdin` | Writes the task prompt to process stdin. |
| `file` | Writes the prompt to `task_prompt.txt`; the path is available as `LATCH_TASK_FILE`. |

The spawned process also receives:

| Variable | Meaning |
| --- | --- |
| `LATCH_TASK_PROMPT` | Raw case prompt. |
| `LATCH_TASK_FILE` | Host path to the prompt file. |
| `BUCEPHALUS_CASE_ID` | Case id. |
| `BUCEPHALUS_TASK_ID` | Task id, defaulting to case id. |
| `BUCEPHALUS_TRIAL_INPUT_PATH` | JSON input envelope path. |
| `BUCEPHALUS_RESULT_PATH` | Optional JSON result path. |
| `BUCEPHALUS_TRAJECTORY_PATH` | Optional JSONL trajectory path. |

## Result Bundle

The result schema is [latch_result_v1](../../schemas/latch_result_v1.jsonschema).

Each run directory contains:

```text
latch_manifest.json
latch_result.json
cases/<case_id>/
  case_manifest.json
  case_result.json
  task_prompt.txt
  in/trial_input.json
  workspace/
  out/stdout.log
  out/stderr.log
  out/result.json          # if the agent wrote it
  out/candidate.patch      # if the workspace changed
  events/trajectory.jsonl  # if the agent wrote it
```

Current host execution stamps `enforcement_level: guarded`. It does not claim
Linux containment until the command is actually launched through a containment
primitive.
