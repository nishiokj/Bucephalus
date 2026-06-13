# What You Must Provide

Bucephalus is not a magic wrapper around arbitrary apps. A successful experiment needs explicit cases, a stage chain, and declared ephemerals and externals.

For the full field-level YAML surface, use [Experiment YAML Reference](experiment-yaml-reference.md).

## Required Pieces

| Piece | Required | Example |
| --- | --- | --- |
| Experiment YAML | Yes | `experiment.yaml` |
| Project manifest | For hosted Cloud YAML builds | `bucephalus.project.yaml` with `schema_version: bucephalus_project_v1`, `project.id`, `package_sources`, and `targets.hosted_cloud` |
| Cases | Yes | `cases.jsonl` with `case_v2` rows |
| Stages | Yes | `stages.case`, `stages.agent`; add `stages.grader` for custom grading and `stages.execution` when the agent site is not inferred |
| Agent command | Yes | `stages.agent.command` |
| Agent image | When the agent runs in its own container | `ghcr.io/my-org/my-agent-runtime:latest` |
| Agent mount | Optional; declare only when the agent needs mounted files | `stages.agent.mount.source: ./agent`, `stages.agent.mount.mount.path: /opt/agent` |
| Ephemerals | Optional; declare only when a stage needs a per-trial service | `ephemerals.mcp-bash`, `stages.agent.ephemerals: [mcp-bash]` |
| Case workspace image | When workspace source is `container_image` | `case_v2.resources.workspace.image` |
| Grader declaration | Only when benchmark scoring needs a grader | `stages.grader.strategy`; omit `stages.grader` for the no-grader default |
| Metric declarations | If you want queryable custom metrics | `metrics[].id` plus `metrics[].from` |
| Event captures | If you want live runtime traces/progress | `stages.agent.events[]` |
| Runtime env/secrets | If your agent needs them | `runtime.secrets[]` plus `--env OPENAI_API_KEY=...` |
| Compute backend | Optional | Defaults to `runtime.compute.backend: local-docker`; declare `modal` when needed |
| Grader inputs/outputs | If benchmark scoring needs a grader | `stages.grader.inputs`, `stages.grader.outputs` |

Schema files live in `schemas/`. Current case rows should use
`schemas/case_v2.jsonschema` and set `schema_version: case_v2`. Hosted Cloud
YAML builds use `schemas/bucephalus_project_v1.jsonschema` for the project
manifest that defines the upload boundary. `case_v1` and the older
`task_row_v2` row shape remain accepted for migration and existing suites.
Agent responses are arbitrary JSON written to `BUCEPHALUS_RESULT_PATH`.
Benchmark graders declare native outputs and metrics read from those outputs;
graders do not need to emit Bucephalus-specific conclusions.

## Minimal Experiment Shape

```yaml
experiment:
  id: my_eval

matrix:
  cases:
    path: cases.jsonl

runtime:
  secrets:
    - { name: OPENAI_API_KEY }

stages:
  case:
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
    command: ["my-agent", "run", "--model", "gpt-5.3-codex"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
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
    from: grader.report.resolved
    direction: maximize
```

Omitted runtime compute defaults to `runtime.compute.backend: local-docker`.
Storage and trace sinks are runner-owned today, so do not declare
`runtime.storage` or `runtime.traces`. Omitted network fields default to `none`,
so declare `runtime.network.agent: full` or `runtime.network.task_sandbox: full`
only when that process needs egress. Use `runtime.network.agent: llm_egress`
only for the agent plane; `runtime.network.default` and
`runtime.network.task_sandbox` accept `none`, `full`, or `allowlist_enforced`.
`runtime.network.default` is an exclusive authoring shorthand for setting both
planes to the same value; do not combine it with explicit `agent` or
`task_sandbox`. Write both planes explicitly for mixed network modes. The build
lowers the shorthand away. The CLI `--executor` flag can
override the declared compute backend for an operator-run experiment.

`stages.execution.agent_site` is inferred in the common cases. An agent image
defaults to `agent_container`; a writable container-image case workspace without
an agent image defaults to `task_runtime`; an input-only case without an agent
image defaults to `host`. For other valid boundaries, such as read-only file
cases without an agent image, declare `stages.execution.agent_site`.

`matrix.cases.source` defaults to `file` when `path: cases.jsonl` is present.
Omitted `matrix.variants` defaults to one `baseline` variant. If you declare
one variant without `baseline`, build marks it as the baseline. If you declare
multiple variants, mark exactly one with `baseline: true`; variant names do not
select the baseline. Build writes explicit `baseline: true`/`false` flags into
the sealed package. Omitted variant `config` defaults to `{}` in authoring YAML
and is written explicitly into the sealed package.

`stages.case.interface` defaults from the case resource: no resource means
`input_only`, `files` means `readonly_files`, and `workspace` means
`writable_workspace`. If you set `interface` yourself, it must match the
resource block you declare.

A single declared metric defaults to `primary: true`. When declaring multiple
metrics, mark exactly one as primary. Declared metrics default to
`required: true`; use `required: false` only for optional diagnostics.

Declared `file` and `result_json` output captures default to `required: true`;
build writes that flag explicitly into the sealed package. Use `required: false`
only for optional diagnostics.

`policy.sanitization_profile` is optional and defaults to `standard_runtime`. Declare `hermetic_functional` only for experiments where both `runtime.network.task_sandbox` and `runtime.network.agent` are `none`; build and preflight reject hermetic configs that request network access.

`policy.task_sandbox.hardening.no_new_privileges` and
`policy.task_sandbox.hardening.drop_all_caps` both default to `true`; build
writes those hardening defaults explicitly into the sealed package. Declare
either as `false` only when the task sandbox image requires the weaker Docker
runtime setting.

Agent integration level is inferred by build and written into sealed packages:
plain command agents become `cli_basic`, and declared command-agent event sinks
become `cli_events`. Authoring YAML does not need an integration-level knob.
When you declare event sinks directly, write `ingest` and `retain_raw`
explicitly. Declared agent output mounts require `persist`. Use
`traces.source: protocol` for the default command-agent event sink without
writing the lowered event declaration yourself.

`policy.policies` is optional and closed in authoring YAML. Omit it for default
scheduling, isolated per-trial state, one attempt, no retry triggers, no pruning
limit, and chain leases enabled. Build writes those choices explicitly into the
sealed package. Declared keys are `scheduling`, `state`, `retry`, `pruning`, and
`concurrency`; `pruning.max_consecutive_failures: 0` means no pruning limit. Use
`scheduling.comparison: paired` as exclusive shorthand for
`policy.policies.scheduling: paired_interleaved`; do not declare both.
This keeps misspelled or future-looking policy knobs from silently doing
nothing.

Metric declarations are the canonical analytics contract. The runner does not persist arbitrary fields from the agent response as metric rows; declare each custom metric you want to query. See [Metrics](metrics.md).

## Trial Runtime Responsibilities

`stages` is the public stage surface. Do not use the removed top-level `baseline`, `variant_plan`, `dataset`, `design`, `runtime.agent_runtime`, `runtime.agent`, `runtime.dependencies`, `task_runtime`, `known_agent_ref`, or `custom_image` shapes.

Your agent app must:

1. Start from `stages.agent.command`.
2. Read trial input from `BUCEPHALUS_TRIAL_INPUT_PATH`.
3. Work the case according to the resolved case interface.
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
          format: unified_diff
  grader:
    inputs:
      candidate_file:
        source:
          output: agent.candidate
          field: patch
        materialize:
          as: file
          path: /patch.diff
```

`workspace_diff` captures a git patch from writable case workspaces. The grader receives the materialized input declared under `stages.grader.inputs`; it should not know about the runner's internal Transport Envelope or trial layout.
Declared grader inputs default to `required: true`; build writes that boolean
into the sealed package. Use `required: false` only for optional grader context.
For active graders, omitted `inputs` defaults to an empty map in authoring YAML
and is written explicitly into the sealed package. Declare at least one grader
output when the grader is active.

Optional but recommended:

- set `traces.source: protocol` and write JSONL to the injected `BUCEPHALUS_TRAJECTORY_PATH` when command-agent traces should be ingested
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

Local Docker attaches ephemerals on a per-trial network and tracks them for cleanup. Host stages cannot attach container ephemerals. Modal rejects ephemerals until backend-native support exists. See [Ephemerals](ephemerals.md).

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
