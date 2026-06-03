# What You Must Provide

Bucephalus is not a magic wrapper around arbitrary apps. A successful experiment needs explicit cases, a stage chain, and declared ephemerals and externals.

For the full field-level YAML surface, use [Experiment YAML Reference](experiment-yaml-reference.md).

## Required Pieces

| Piece | Required | Example |
| --- | --- | --- |
| Experiment YAML | Yes | `experiment.yaml` |
| Cases | Yes | `cases.jsonl` with `case_v2` rows |
| Stages | Yes | `stages.case`, `stages.agent`, `stages.agent.outputs`, `stages.execution`, `stages.grader` |
| Agent command | Yes | `stages.agent.command` |
| Agent image | When `agent_site: agent_container` | `ghcr.io/my-org/my-agent-runtime:latest` |
| Agent mount | Optional; declare only when the agent needs mounted files | `stages.agent.mount.source: ./agent`, `stages.agent.mount.mount.path: /opt/agent` |
| Ephemerals | Optional; declare only when a stage needs a per-trial service | `ephemerals.mcp-bash`, `stages.agent.ephemerals: [mcp-bash]` |
| Case workspace image | When workspace source is `container_image` | `case_v2.resources.workspace.image` |
| Grader declaration | Yes, use `strategy: none` if no grader runs | `stages.grader.strategy` |
| Metric declarations | If you want queryable custom metrics | `metrics[].id` plus `metrics[].source` |
| Event captures | If you want live runtime traces/progress | `stages.agent.events[]` |
| Runtime env/secrets | If your agent needs them | `--env OPENAI_API_KEY=...` |
| Compute backend | Yes | `runtime.compute.backend: local-docker` or `modal` |
| Grader inputs/outputs | If benchmark scoring needs a grader | `stages.grader.inputs`, `stages.grader.outputs` |

Schema files live in `schemas/`. Current case rows should use `schemas/case_v2.jsonschema` and set `schema_version: case_v2`. `case_v1` and the older `task_row_v2` row shape remain accepted for migration and existing suites. Agent responses are arbitrary JSON written to `BUCEPHALUS_RESULT_PATH`. Benchmark graders declare native outputs and metrics read from those outputs; graders do not need to emit Bucephalus-specific conclusions.

## Minimal Experiment Shape

```yaml
experiment:
  id: my_eval
  name: My Agent Evaluation

runtime:
  compute: { backend: local-docker }
  storage: { backend: local-fs }
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
  cases:
    source: file
    path: cases.jsonl
  repeats: 1
  seeds: [1]

scheduling:
  max_concurrency: 1
  comparison: none
  random_seed: 1

stages:
  case:
    interface: writable_workspace
    workspace:
      source: container_image
      image: { from: case_row }
      workdir: { from: case_row }
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
    outputs:
      result:
        capture:
          type: file
          path: /bucephalus/out/result.json
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
          path: /bucephalus/out/grader_report.json
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
  task_sandbox: {}
```

Set `runtime.compute.backend` to `local-docker` for local container execution or `modal` for Modal sandbox execution. The CLI `--executor` flag can override the declared backend for an operator-run experiment.

`cases.source` and `matrix.cases.source` are currently local file backed. Use `source: file` with `path: cases.jsonl`.

`policy.sanitization_profile` is optional and defaults to `perf_benchmark`. Omit it for provider-backed or otherwise networked agents. Declare `hermetic_functional` only for experiments where both `runtime.network.task_sandbox` and `runtime.network.agent` are `none`; build and preflight reject hermetic configs that request network access.

Metric declarations are the canonical analytics contract. The runner does not persist arbitrary fields from the agent response as metric rows; declare each custom metric you want to query. See [Metrics](metrics.md).

## Trial Runtime Responsibilities

`stages` is the public stage surface. Do not use the removed top-level `baseline`, `variant_plan`, `dataset`, `design`, `runtime.agent_runtime`, `runtime.agent`, `runtime.dependencies`, `task_runtime`, `known_agent_ref`, or `custom_image` shapes.

Your agent app must:

1. Start from `stages.agent.command`.
2. Read trial input from `BUCEPHALUS_TRIAL_INPUT_PATH`.
3. Work the case according to `stages.case.interface`.
4. Write any valid JSON response to `BUCEPHALUS_RESULT_PATH`.
5. Exit when finished.

If your grader needs a patch, declare it as an agent output and bind it into the grader input surface:

```yaml
stages:
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

`workspace_diff` captures a git patch from writable case workspaces. The grader receives the materialized input declared under `stages.grader.inputs`; it should not know about the runner's internal Transport Envelope or trial layout.

Optional but recommended:

- set `traces.source: protocol` and write JSONL to `BUCEPHALUS_TRAJECTORY_PATH` when command-agent traces should be ingested
- write runtime evidence under declared `stages.agent.output_mounts`
- attach `ephemerals` only for services the stage actually calls
- for artifact cases, write `artifact_envelope_v1` JSON
- produce clear stdout/stderr for debugging

## Ephemeral Responsibilities

Use `ephemerals` for per-trial service containers, not for case data, mounted artifacts, or long-lived external dependencies. Each ephemeral has a top-level declaration and each stage opts in explicitly:

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

Local Docker attaches ephemerals on a per-trial network and tracks them for cleanup. Host stages cannot attach container ephemerals. Modal rejects ephemerals until backend-native support exists. See [Ephemerals](sidecars.md).

## Grader Responsibilities

Your grader declaration must:

1. Start from `stages.grader.command`.
2. Declare any inputs it needs from case fields or upstream stage outputs.
3. Declare outputs the runner should capture after the grader runs.
4. Declare metrics that read from those outputs.

The runner synthesizes its internal trial conclusion from declared grader outputs and metrics. The grader itself should emit its native report.

Pick one grader runtime strategy and declare only the fields for that strategy:

| Strategy | Required declaration | File/path rule |
| --- | --- | --- |
| `none` | no command | Use only when metrics come from agent output and no grader result metrics are declared. |
| `in_task_runtime` | `command` | Requires `case.interface: writable_workspace` and `workspace.source: container_image`. Relative command file paths are sealed into the package and mounted under the case workdir support directory before grading. |
| `injected` | `command`, `injected.bundle`, `injected.copy_dest` | The bundle is sealed into the package, copied into the case sandbox after the agent step, then executed there. |
| `separate` | `command`, `separate.image`, `separate.workdir` | The command runs in a separate grader image. |
| `host` | `command`, `host.capability` | The command must reference a runner-owned host capability, not package-local files or arbitrary host paths. |

Host graders are intentionally narrow. This is valid:

```yaml
stages:
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
          path: /bucephalus/out/swebench_report.json
          format: json
          required: true
```

This is not valid for `strategy: host`:

```yaml
stages:
  grader:
    strategy: host
    command: ["/Users/me/project/grader", "--out", "/bucephalus/out/report.json"]
```

If your grader produces a native report, declare it under `stages.grader.outputs` and point metrics at that output. See [Grader Runtime](graders-and-mappers.md) and [Grader Transport](grader-transport.md).
