# What You Must Provide

AgentLab is not a magic wrapper around arbitrary apps. A successful experiment needs explicit task data, an agent runtime contract, and, for benchmark scoring, a grader contract.

For the full field-level YAML surface, use [Experiment YAML Reference](experiment-yaml-reference.md).

## Required Pieces

| Piece | Required | Example |
| --- | --- | --- |
| Experiment YAML | Yes | `experiment.yaml` |
| Task rows | Yes | `tasks.jsonl` with `task_row_v2` rows |
| Trial runtime | Yes | `trial_runtime.task`, `trial_runtime.agent`, `trial_runtime.agent.outputs`, `trial_runtime.execution`, `trial_runtime.grader` |
| Agent command | Yes | `trial_runtime.agent.command` |
| Agent image | When `agent_site: agent_container` | `ghcr.io/my-org/my-agent-runtime:latest` |
| Agent mount | Optional; declare only when the agent needs mounted files | `trial_runtime.agent.mount.source: ./agent`, `trial_runtime.agent.mount.mount.path: /opt/agent` |
| Sidecars | Optional; declare only when a stage needs a per-trial service | `sidecars.mcp-bash`, `trial_runtime.agent.sidecars: [mcp-bash]` |
| Task sandbox image | When workspace source is `container_image` | `task_row_v2.runtime.container_image.image` |
| Grader declaration | Yes, use `strategy: none` if no grader runs | `trial_runtime.grader.strategy` |
| Metric declarations | If you want queryable custom metrics | `metrics[].id` plus `metrics[].source` |
| Event captures | If you want live runtime traces/progress | `trial_runtime.agent.events[]` |
| Runtime env/secrets | If your agent needs them | `--env OPENAI_API_KEY=...` |
| Grader inputs/outputs | If benchmark scoring needs a grader | `trial_runtime.grader.inputs`, `trial_runtime.grader.outputs` |

Schema files live in `schemas/`. Current task rows use `schemas/task_row_v2.jsonschema` and must set `schema_version: task_row_v2`. Agent responses are arbitrary JSON written to `AGENTLAB_RESULT_PATH`. Benchmark graders declare native outputs and metrics read from those outputs; graders do not need to emit AgentLab-specific conclusions.

## Minimal Experiment Shape

```yaml
experiment:
  id: my_eval
  name: My Agent Evaluation

runtime:
  compute: { backend: local-docker }
  storage: { backend: local-fs, config: { root: .lab/runs/ } }
  traces: { backend: local-stdout }
  secrets:
    - { name: OPENAI_API_KEY, from: env }
  network:
    task_sandbox: full
    agent: full

matrix:
  variants:
    - id: control
      baseline: true
      config:
        model: gpt-5.3-codex
  tasks:
    source: file
    path: tasks.jsonl
  repeats: 1
  seeds: [1]

scheduling:
  max_concurrency: 1
  comparison: none
  random_seed: 1

trial_runtime:
  task:
    interface: writable_workspace
    workspace:
      source: container_image
      image:
        from: task_row
      workdir:
        from: task_row
  agent:
    mount:
      source: ./agent
      mount:
        path: /opt/agent
        read_only: true
    image: ghcr.io/my-org/my-agent-runtime:latest
    command: ["my-agent", "run", "--model", "$model"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
    integration_level: cli_basic
    outputs:
      result:
        capture:
          type: file
          path: /agentlab/out/result.json
          format: json
  execution:
    agent_site: agent_container
  grader:
    strategy: in_task_runtime
    command: ["bash", "/testbed/eval.sh"]
    inputs: {}
    outputs:
      report:
        capture:
          type: file
          path: /agentlab/out/grader_report.json
          format: json
          required: true

metrics:
  - id: resolved
    source:
      type: grader_output
      output: report
      pointer: /resolved
    direction: maximize
    primary: true

policy:
  timeout_ms: 600000
  sanitization_profile: perf_benchmark
  task_sandbox: {}
```

`matrix.tasks.source` is currently local file backed. Use `source: file` with `path: tasks.jsonl`.

`policy.sanitization_profile` is optional. Declare `hermetic_functional` only for experiments where both `runtime.network.task_sandbox` and `runtime.network.agent` are `none`; build and preflight reject hermetic configs that request network access.

Metric declarations are the canonical analytics contract. The runner does not persist arbitrary fields from the agent response as metric rows; declare each custom metric you want to query. See [Metrics](metrics.md).

## Trial Runtime Responsibilities

`trial_runtime` is the public stage surface. Do not use the removed top-level `baseline`, `variant_plan`, `dataset`, `design`, `runtime.agent_runtime`, `runtime.agent`, `runtime.dependencies`, `task_runtime`, `known_agent_ref`, or `custom_image` shapes.

Your agent app must:

1. Start from `trial_runtime.agent.command`.
2. Read trial input from `AGENTLAB_TRIAL_INPUT_PATH`.
3. Do the task according to `trial_runtime.task.interface`.
4. Write any valid JSON response to `AGENTLAB_RESULT_PATH`.
5. Exit when finished.

If your grader needs a patch, declare it as an agent output and bind it into the grader input surface:

```yaml
trial_runtime:
  agent:
    outputs:
      candidate:
        capture:
          type: workspace_diff
  grader:
    inputs:
      candidate_file:
        source:
          output: agent.candidate
          field: patch
        materialize:
          as: file
          path: /patch.diff
        required: true
```

`workspace_diff` captures a git patch from writable task workspaces. The grader receives the materialized input declared under `trial_runtime.grader.inputs`; it should not know about the runner's internal envelope or trial layout.

Optional but recommended:

- declare `trial_runtime.agent.events` and write JSONL events there if using `integration_level: cli_events`
- write runtime evidence under declared `trial_runtime.agent.output_mounts`
- attach `sidecars` only for services the stage actually calls
- for artifact tasks, write `artifact_envelope_v1` JSON
- produce clear stdout/stderr for debugging

## Sidecar Responsibilities

Use `sidecars` for per-trial service containers, not for task data, mounted artifacts, or long-lived external dependencies. Each sidecar has a top-level declaration and each stage opts in explicitly:

```yaml
sidecars:
  mcp-bash:
    image: ghcr.io/acme/mcp-bash-server:v0.4
    lifecycle: per-trial
    expose:
      MCP_URL: http://mcp-bash:8080

trial_runtime:
  agent:
    sidecars: [mcp-bash]
```

Local Docker attaches sidecars on a per-trial network and tracks them for cleanup. Host stages cannot attach container sidecars. Modal rejects sidecars until backend-native support exists. See [Sidecars](sidecars.md).

## Grader Responsibilities

Your grader declaration must:

1. Start from `trial_runtime.grader.command`.
2. Declare any inputs it needs from task fields or upstream runtime outputs.
3. Declare outputs the runner should capture after the grader runs.
4. Declare metrics that read from those outputs.

The runner synthesizes its internal trial conclusion from declared grader outputs and metrics. The grader itself should emit its native report.

Pick one grader runtime strategy and declare only the fields for that strategy:

| Strategy | Required declaration | File/path rule |
| --- | --- | --- |
| `none` | no command | Use only when metrics come from agent output and no grader result metrics are declared. |
| `in_task_runtime` | `command` | Requires `task.interface: writable_workspace` and `workspace.source: container_image`. Relative command file paths are sealed into the package and mounted under the task workdir support directory before grading. |
| `injected` | `command`, `injected.bundle`, `injected.copy_dest` | The bundle is sealed into the package, copied into the task sandbox after the agent step, then executed there. |
| `separate` | `command`, `separate.image`, `separate.workdir` | The command runs in a separate grader image. |
| `host` | `command`, `host.capability` | The command must reference a runner-owned host capability, not package-local files or arbitrary host paths. |

Host graders are intentionally narrow. This is valid:

```yaml
trial_runtime:
  grader:
    strategy: host
    host:
      capability: swebench_official
    command:
      - official-grader
      - --grader-input
      - --grader-input
    outputs:
      report:
        capture:
          type: file
          path: /agentlab/out/swebench_report.json
          format: json
          required: true
```

This is not valid for `strategy: host`:

```yaml
trial_runtime:
  grader:
    strategy: host
    command: ["/Users/me/project/grader", "--out", "/agentlab/out/report.json"]
```

If your grader produces a native report, declare it under `trial_runtime.grader.outputs` and point metrics at that output. See [Grader Runtime](graders-and-mappers.md) and [Grader Transport](grader-transport.md).
