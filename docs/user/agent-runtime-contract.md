# Agent Runtime Contract

The agent runtime is your application. AgentLab launches it once per trial.

## Config Fields

```yaml
runtime:
  agent_runtime:
    artifact: ./agent
    image: ghcr.io/my-org/my-agent-runtime:latest
    command: ["python", "-m", "my_agent.run"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
    output_mounts:
      - id: session_context
        kind: directory
        path: session-context
        env: AGENTLAB_SESSION_CONTEXT_ROOT
    integration_level: cli_basic
    network: none
```

| Field | Meaning |
| --- | --- |
| `artifact` | Files copied into the agent runtime at `/opt/agent`. |
| `image` | Container image used for the agent process. |
| `command` | Process argv. Runs inside the task sandbox context. |
| `env` | Env vars injected into the agent process. `$NAME` resolves from variant bindings, `--env`, `--env-file`, then host env. |
| `output_mounts` | Runtime-owned output directories under `/agentlab/out`, optionally exposed through an env var and persisted with trial outputs. |
| `integration_level` | `cli_basic` or `cli_events` for current local runs. |
| `network` | Agent network mode, usually `none` for hermetic evals or `full` for provider-backed agents. |

## Output Mounts

Use `runtime.agent_runtime.output_mounts` for runtime evidence that should be written as files rather than embedded in the final result JSON.

```yaml
runtime:
  agent_runtime:
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
| `AGENTLAB_RESULT_PATH` | Where the agent must write `trial_output_v1`. |
| `AGENTLAB_RUN_ID` | Current run id. |
| `AGENTLAB_TRIAL_ID` | Current trial id. |
| `AGENTLAB_VARIANT_ID` | Current variant id. |
| `AGENTLAB_TASK_ID` | Current task id. |
| `AGENTLAB_TIMEOUT_MS` | Trial timeout in milliseconds. |
| `AGENTLAB_TRAJECTORY_PATH` | Event JSONL path when events are enabled. |

## Trial Input

Your agent should treat `AGENTLAB_TRIAL_INPUT_PATH` as the source of truth. It includes ids, task payload, variant bindings, runtime control info, and policy context.

The task-specific payload is under `task`. For example:

```json
{
  "ids": {
    "run_id": "run_...",
    "trial_id": "trial_1",
    "variant_id": "control",
    "task_id": "TASK001"
  },
  "bindings": {
    "model": "gpt-5.3-codex"
  },
  "task": {
    "id": "TASK001",
    "input": {
      "prompt": "Fix the failing test."
    }
  }
}
```

## Trial Output

At minimum, write:

```json
{
  "schema_version": "trial_output_v1",
  "outcome": "success",
  "objective": {
    "name": "resolved",
    "value": 1.0,
    "direction": "maximize"
  },
  "metrics": {
    "resolved": 1.0
  },
  "answer": {
    "summary": "What the agent did."
  }
}
```

If your agent cannot solve the task, still write a valid result with `outcome: "failure"` and useful diagnostics. A missing or invalid result is a contract failure, not a scientific verdict.

## Events

For `integration_level: cli_events`, write hook events to `AGENTLAB_TRAJECTORY_PATH`.

Events are optional for the first successful run, but they become important for token counts, step counts, trace diagnostics, and control acknowledgements.
