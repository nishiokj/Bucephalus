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

Inspect or remove the local installation:

```bash
bucephalus setup status
bucephalus setup uninstall
```

`setup status` reports daemon service state, daemon readiness, MCP registration,
and Cloud auth readiness. Local Core smoke fixtures do not require Cloud auth;
Cloud benchmark resolution and upload require `BUCEPHALUS_CLOUD_USER_TOKEN` or
a token file at `<BUCEPHALUS_HOME>/auth/cloud_user_token`. Use
`bucephalus login` to cache Cloud tokens, and `bucephalus logout` to remove the
cached token files. If `BUCEPHALUS_CLOUD_USER_TOKEN` is set, unset that
environment variable too.

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

Run the local daemon for development/debugging:

```bash
bucephalus daemon
```

The MCP adapter presents the host workflow as a dispatch surface:

| Tool | Purpose |
| --- | --- |
| `status` | Check install/auth/runtime readiness. |
| `dispatch_benchmark` | Resolve the requested benchmark into local cases and start a dispatch. |
| `dispatch_status` | Refresh dispatch state and the live viewing surface. |

`dispatch_benchmark` is where the caller supplies the agent process:

```json
{
  "benchmark": "local:file-edit-smoke",
  "cases": 1,
  "label": "local smoke",
  "headless_command": {
    "argv": ["codex", "exec", "solve the task"]
  }
}
```

The MCP response returns a `dispatch_id`, `dispatch_ref`, and refs such as
`paths.live_view_ref`. It does not return daemon job ids, socket paths, manifest
paths, run roots, or host filesystem paths. Those are internal local runtime
details.

For non-local benchmarks, `dispatch_benchmark` calls the Cloud latch resolver at
`POST /v1/latch/resolve`. The resolver returns a `latch_manifest_v1` plus
optional materials. The host downloads or writes those materials under the
dispatch resolution directory, rewrites `material://...`, `artifact://...`,
`cloud://...`, and matching `package://...` manifest refs to local package
paths, injects the supplied headless command into `defaults.launch`, then starts
the managed local runtime.

The public dispatch lifecycle reports:

| Field | Meaning |
| --- | --- |
| `resolution` | Cloud or local fixture resolution completed. |
| `materials` | Case material fetch/cache status and digests. |
| `local_runtime` | Local execution status, including `local_completed`. |
| `submission` | Cloud submission status for the completed local latch result archive, including upload and semantic latch submission ids. |
| `grading` | Host latch grading outcome from `latch_result.json`, including pass/fail/error/declined counts when graders are declared. |

The uploaded latch result archive is curated. It includes dispatch metadata,
resolved manifests, run/case result summaries, declared JSON result artifacts,
and candidate patch evidence, with local paths, local commands, environment
fields, and secret-looking keys redacted. It does not upload raw stdout/stderr
logs, full workspaces, runtime state, temp files, event streams, materialization
logs, or agent-visible trial input directories.

Low-level latch MCP calls are disabled by default. For local debugging only,
start the MCP server with `BUCEPHALUS_MCP_DEBUG_LATCH_TOOLS=1` to allow the
legacy manifest/job controls.

## Recoverable outcomes

A recoverable condition is never returned as a JSON-RPC protocol error. Instead
the tool result is marked `isError: true` and its `structuredContent` carries a
guided body so the caller can self-correct without leaving the tool:

```json
{
  "status": "blocked",
  "because": "cloud_auth_required",
  "summary": "Cloud benchmark 'X' requires Bucephalus Cloud sign-in; local fixtures do not.",
  "next_actions": [
    { "type": "cli_command", "command": "bucephalus login", "description": "..." },
    { "type": "mcp_tool", "tool": "dispatch_benchmark", "arguments": {}, "description": "..." }
  ],
  "details": {},
  "docs": "docs/user/latch.md"
}
```

Every guided body names exactly one `because` code and at least one next action,
so a response is always either a result or a way forward — never a dead end.

| `because` | Meaning | Primary next action |
| --- | --- | --- |
| `headless_command_required` | `dispatch_benchmark` was called without `headless_command.argv`. | Resubmit with the agent's headless command. |
| `cloud_auth_required` | A non-local benchmark was requested while signed out. | `bucephalus login`, or rehearse with `local:file-edit-smoke`. |
| `daemon_unavailable` | The local runtime could not be reached to start the dispatch. | `bucephalus setup`, then `bucephalus setup status`. |
| `dispatch_id_required` | `dispatch_status` was called without a `dispatch_id`. | Call `status` to list recent dispatches. |
| `dispatch_not_found` | The supplied `dispatch_id` does not exist on this host. | Call `status` to list recent dispatches. |

`status` returns `recent_dispatches` (newest first, with `dispatch_id`, `status`,
`label`, `benchmark`, and timestamps). It is the re-orientation surface: call it
to recover a lost `dispatch_id` or to see what can be polled.

For local UX rehearsal, `dispatch_benchmark` still supports the explicit
`local:file-edit-smoke` fixture. It resolves into a normal `latch_manifest_v1`,
then starts that manifest through the managed local runtime. The resolver artifact is
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
