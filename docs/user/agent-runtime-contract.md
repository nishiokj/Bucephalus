# Agent Runtime Contract

The agent runtime is your application. AgentLab launches it once per trial from `stages.agent.command`.

## Config Fields

```yaml
ephemerals:
  mcp-bash:
    image: ghcr.io/acme/mcp-bash-server:v0.4
    lifecycle: per-trial
    expose:
      MCP_URL: http://mcp-bash:8080

runtime:
  network:
    task_sandbox: none
    agent: full

stages:
  case:
    interface: writable_workspace
    workspace:
      source: container_image
      image:
        from: case_row
      workdir:
        from: case_row
  agent:
    ephemerals: [mcp-bash]
    mount:
      source: ./agent
      mount:
        path: /opt/agent
        read_only: true
    image: ghcr.io/my-org/my-agent-runtime:latest
    command: ["my-agent", "run"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
    output_mounts:
      - id: session_context
        kind: directory
        path: session-context
        env: AGENTLAB_SESSION_CONTEXT_ROOT
    integration_level: cli_basic
  execution:
    agent_site: agent_container
```

| Field | Meaning |
| --- | --- |
| `stages.agent.command` | Process argv. `$NAME` resolves from variant config, `--env`, `--env-file`, then host env. Use this for non-secret variant configuration. |
| `stages.agent.image` | Container image for the agent process. Required for `agent_site: agent_container`; forbidden for `agent_site: task_runtime` and `agent_site: host`. |
| `stages.agent.ephemerals` | Optional list of top-level ephemeral ids attached to the agent stage. Local Docker injects each ephemeral's `expose` env into the agent process. Forbidden when `agent_site: host`. |
| `stages.agent.mount` | Optional explicit agent file mount object. Omit for image-native agents. |
| `stages.agent.mount.source` | Source path or agent build id to stage. |
| `stages.agent.mount.mount.path` | Absolute runtime mount path, such as `/opt/agent`. |
| `stages.agent.mount.mount.read_only` | Whether the mount is read-only. |
| `stages.agent.env` | Env vars injected into the agent process. `$NAME` resolves from variant config, `--env`, `--env-file`, then host env. Use this for secrets and ambient runtime affordances. |
| `stages.agent.output_mounts` | Runtime-owned output directories under `/agentlab/out`, optionally exposed through an env var and persisted with trial outputs. |
| `stages.agent.integration_level` | `cli_basic` or `cli_events` for current local runs. |
| `runtime.network.agent` | Agent network mode, usually `none` for hermetic evals or `full` for provider-backed agents. |
| `stages.execution.agent_site` | Where the agent runs: `agent_container`, `task_runtime`, or `host`. |

If `policy.sanitization_profile` is `hermetic_functional`, `runtime.network.task_sandbox` and `runtime.network.agent` must both be `none`.

Removed execution-shaping fields such as `workspace_patches`, `launch`, `env_from_host`, `binding_args`, `support_files`, `secret_env`, and `trial_runtime.agent.network` are rejected. Use `command`, `env`, `output_mounts`, `runtime.network`, and explicit case/grader surfaces instead.

## Agent Site

| Site | Meaning |
| --- | --- |
| `agent_container` | The agent runs in its own container image. This is the normal path for provider-backed or packaged agents. |
| `task_runtime` | The agent runs inside the case sandbox. Requires `case.interface: writable_workspace` and `workspace.source: container_image`; forbids `agent.image`. Declare `agent.mount` only when the command needs mounted agent files. |
| `host` | The agent runs on the runner host. For advanced local integrations only; forbids `agent.image`. |

## Ephemerals

An agent may attach per-trial service containers declared at top level:

```yaml
ephemerals:
  mcp-bash:
    image: ghcr.io/acme/mcp-bash-server:v0.4
    lifecycle: per-trial
    expose:
      MCP_URL: http://mcp-bash:8080

stages:
  agent:
    ephemerals: [mcp-bash]
```

The ephemeral id is the hostname alias on the per-trial network. `expose` values become agent env vars only for the stages that list the ephemeral. If the agent runs on the host, it cannot attach container ephemerals.

## Output Mounts

Use `stages.agent.output_mounts` for runtime evidence that should be written as files rather than embedded in the final result JSON.

```yaml
stages:
  agent:
    output_mounts:
      - id: session_context
        kind: directory
        path: session-context
        env: AGENTLAB_SESSION_CONTEXT_ROOT
        persist: true
```

AgentLab creates the directory before launch, maps it inside the container as `/agentlab/out/<path>`, and injects `env` when provided. The `path` value is relative to `/agentlab/out`; absolute paths and `..` segments are rejected.

## Runtime Environment Variables

AgentLab provides these to the agent process:

| Variable | Purpose |
| --- | --- |
| `AGENTLAB_TRIAL_INPUT_PATH` | JSON input for this trial. |
| `AGENTLAB_RESULT_PATH` | Where the agent must write any valid JSON response. |
| `AGENTLAB_RUN_ID` | Current run id. |
| `AGENTLAB_TRIAL_ID` | Current trial id. |
| `AGENTLAB_VARIANT_ID` | Current variant id. |
| `AGENTLAB_CASE_ID` | Current case id. |
| `AGENTLAB_TIMEOUT_MS` | Trial timeout in milliseconds. |
| `AGENTLAB_TRAJECTORY_PATH` | Event JSONL path when events are enabled. |
| `AGENTLAB_CASE_IMAGE` | Case sandbox image when one is resolved. |

## Trial Input

Your agent should treat `AGENTLAB_TRIAL_INPUT_PATH` as the source of truth. It includes ids, case payload, variant bindings, runtime control info, and policy context.

The case-specific payload is serialized under `case`. For example:

```json
{
  "ids": {
    "run_id": "run_...",
    "trial_id": "trial_1",
    "variant_id": "control",
    "case_id": "CASE001"
  },
  "bindings": {
    "model": "gpt-5.3-codex"
  },
  "case": {
    "id": "CASE001",
    "input": {
      "prompt": "Fix the failing test."
    }
  }
}
```

AgentLab passes the case payload through. It does not translate benchmark-specific case fields into a second runner-owned shape before invoking your agent.

## Trial Output

Write any valid JSON value to `AGENTLAB_RESULT_PATH`. AgentLab does not require runner-owned fields such as `schema_version`, ids, or `outcome`; process health comes from exit status, timeout, file presence, and JSON parse status.

```json
{
  "metrics": {
    "resolved": 1.0
  },
  "answer": {
    "summary": "What the agent did."
  }
}
```

The response JSON is your payload. It is not automatically promoted into durable analytics. If a value should be queryable in `metrics_long`, declare the metric in `experiment.yaml`:

```yaml
metrics:
  - id: resolved
    source:
      type: agent_response
      pointer: /metrics/resolved
    direction: maximize
    primary: true
```

The declaration's `id` is the stored metric name. The pointer is only the extraction path.

If your agent cannot solve the case, either exit nonzero or write diagnostics into the response payload. A missing or invalid JSON result is a contract failure, not a scientific verdict.

Agent outputs are declared under `stages.agent.outputs`. The canonical `result` output captures `/agentlab/out/result.json`; additional outputs can capture files or a `workspace_diff`. If a grader needs one of those values, bind it through `stages.grader.inputs` instead of having the grader inspect trial internals.

## Events

For `integration_level: cli_events`, declare an event capture and point the
agent at the injected path:

```yaml
stages:
  agent:
    integration_level: cli_events
    events:
      - id: rex_events
        path: /agentlab/out/rex-events.jsonl
        format: jsonl
        mode: jsonl
        ingest: true
        retain_raw: false
    command:
      - rex
      - run
      - --events
      - __AGENTLAB_EVENT_PATH_rex_events__
```

AgentLab replaces `__AGENTLAB_EVENT_PATH_<id>__` with the declared path before
launch. `AGENTLAB_TRAJECTORY_PATH` points at the first declared event capture
when one exists; otherwise it points at the default trajectory file.

The event file is newline-delimited JSON. Each line is the agent or tool's
native event payload:

```json
{"event_type":"model_call_end","ts":"2026-05-15T12:34:56Z","usage":{"tokens_in":1200,"tokens_out":230},"outcome":{"status":"ok"}}
```

The user does not declare a runner envelope for this stream. The runner owns
the internal transport metadata it needs for durable storage: run id, trial id,
variant id, case id, schedule slot, source id, row sequence, and ingest time.
The full JSON line is stored as opaque payload, with best-effort columns such as
`event_type`, `ts`, `tool_name`, `outcome_status`, and token counts exposed by
the `events` analysis view when those fields are present.

Events are optional for the first successful run, but they become important for
live progress, token counts, step counts, trace diagnostics, and control
acknowledgements. Declared event captures are ingested into the account SQLite
database during trial execution and exposed through `lab views-live` and the
`events` analysis view. The raw JSONL file is retained only when the capture
sets `retain_raw: true`.
