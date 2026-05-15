# Experiment YAML Reference

This is the canonical authoring reference for `experiment.yaml`.

Use this page when you need to see the whole public YAML surface in one place. The examples below describe the current hard-cutover surface: task behavior, agent execution, grader transport, metrics, policy, and run design are declared explicitly under `trial_runtime`, `metrics`, `policy`, and `design`.

## Minimal Shape

```yaml
experiment:
  id: my_experiment
  name: My Experiment
  workload_type: agent_runtime

dataset:
  suite_id: my_suite
  provider: local_jsonl
  path: ./tasks.jsonl
  split_id: eval
  limit: 10

baseline:
  variant_id: baseline
  bindings: {}

variant_plan:
  - variant_id: candidate
    bindings:
      MODEL: gpt-5.4

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
    artifact:
      source: ./agent
      mount:
        path: /opt/agent
        read_only: true
    command: ["node", "/opt/agent/index.js"]
    network: none
    env:
      MODEL: "$MODEL"
    outputs:
      result:
        capture:
          type: file
          path: /agentlab/out/result.json
          format: json

  execution:
    agent_site: task_runtime

  grader:
    strategy: none

metrics:
  - id: answer_quality
    label: Answer Quality
    value_type: number
    direction: maximize
    source:
      type: agent_response
      pointer: /score

design:
  replications: 1
  random_seed: 1
  shuffle_tasks: false
  max_concurrency: 1
  sanitization_profile: perf_benchmark

policy:
  timeout_ms: 600000
  task_sandbox:
    network: none

validity:
  fail_on_state_leak: true
  fail_on_profile_invariant_violation: true
```

## Top-Level Keys

| Key | Required | Type | Description |
| --- | --- | --- | --- |
| `experiment` | Yes | object | Experiment identity and workload type. |
| `dataset` | Yes | object | Local task source. |
| `baseline` | Yes | object | Baseline variant. The baseline is also included in the resolved variant list. |
| `variant_plan` | No | array | Candidate variants in addition to the baseline. |
| `trial_runtime` | Yes | object | Task interface, agent runtime, execution site, and grader transport. |
| `metrics` | No | array | Declared metrics the runner may persist. |
| `design` | Yes | object | Replication, scheduling, and sanitization design. |
| `policy` | Yes | object | Timeout and task sandbox policy. |
| `validity` | No | object | Failure policy for run validity checks. |
| `artifacts` | No | object | Additional artifact collection behavior. |

## Experiment

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `experiment.id` | Yes | non-empty string | Stable experiment id. |
| `experiment.name` | No | string | Human-readable name. |
| `experiment.workload_type` | Yes | `agent_runtime` | Current public workload type. |
| `experiment.description` | No | string | Optional context. |
| `experiment.owner` | No | string | Optional owner or team. |

## Dataset

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `dataset.suite_id` | Yes | non-empty string | Dataset or benchmark suite id. |
| `dataset.provider` | No | `local_jsonl` | Optional today, but if set it must be `local_jsonl`. |
| `dataset.path` | Yes | path | Local JSONL file. Each non-empty line must be one task row. |
| `dataset.schema_version` | No | string | Informational dataset schema marker. Task rows themselves currently use `schema_version: task_row_v2`. |
| `dataset.split_id` | No | string | Dataset split label such as `eval`, `dev`, or `test`. |
| `dataset.limit` | No | unsigned integer | Maximum number of task rows to load. `0` loads zero tasks. |

## Variants

The baseline variant is declared once under `baseline`. Additional variants go under `variant_plan`.

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `baseline.variant_id` | Yes | non-empty string | Baseline variant id. Legacy `baseline.id` is still accepted, but prefer `variant_id`. |
| `baseline.bindings` | No | object | Variables available to `$NAME` interpolation in runtime command and env values. |
| `baseline.runtime_overrides` | No | object | Variant-specific overrides applied to `trial_runtime`. Keep narrowly scoped. |
| `baseline.image` | No | string | Compatibility shorthand for `runtime_overrides.agent.image`. Prefer `runtime_overrides`. |
| `variant_plan[].variant_id` | Yes | non-empty string | Candidate variant id. Legacy `id` is still accepted, but prefer `variant_id`. |
| `variant_plan[].bindings` | No | object | Variant variables. |
| `variant_plan[].runtime_overrides` | No | object | Variant-specific overrides applied to `trial_runtime`. |
| `variant_plan[].image` | No | string | Compatibility shorthand for `runtime_overrides.agent.image`. Prefer `runtime_overrides`. |

Runtime strings support `$NAME` interpolation from variant bindings and launch-time environment inputs. Removed `${...}` templates are rejected.

## Trial Runtime

`trial_runtime` is the main execution contract. It has four required sections:

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `trial_runtime.task` | Yes | object | Describes what the task exposes to the agent. |
| `trial_runtime.agent` | Yes | object | Describes how to run and capture the agent. |
| `trial_runtime.execution` | Yes | object | Selects where the agent process runs. |
| `trial_runtime.grader` | Yes | object | Describes the downstream grader runtime, or `strategy: none`. |

### Task

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `task.interface` | Yes | `input_only`, `readonly_files`, `writable_workspace` | Task interface exposed to the agent. |
| `task.files` | When `interface: readonly_files` | object | Read-only task files mounted for the agent. |
| `task.files.source` | Yes with `files` | `files`, `archive` | Source type for read-only files. |
| `task.files.path` | Yes with `files` | path | Local file or archive path. |
| `task.files.mount_path` | Yes with `files` | absolute runtime path | Mount location inside the runtime. |
| `task.workspace` | When `interface: writable_workspace` | object | Writable workspace source. |
| `task.workspace.source` | Yes with `workspace` | `container_image`, `files`, `archive`, `git`, `empty` | Workspace source. `task_runtime` execution requires `container_image`. |
| `task.workspace.path` | Source-dependent | path | Local path for `files` or `archive`. |
| `task.workspace.repo` | Source-dependent | string | Git repository URL or path for `git`. |
| `task.workspace.rev` | No | string | Git revision. |
| `task.workspace.image.from` | With `container_image` | `task_row` | Reads the image from each task row. |
| `task.workspace.workdir.from` | With `container_image` | `task_row` | Reads the workdir from each task row. |

Valid combinations:

| Task interface | Allowed execution notes |
| --- | --- |
| `input_only` | No `task.files` or `task.workspace`. Cannot use `agent_site: task_runtime`. |
| `readonly_files` | Requires `task.files.source: files` or `archive`. Rejects task row container images. |
| `writable_workspace` | Requires `task.workspace`. `agent_site: task_runtime`, `grader.strategy: in_task_runtime`, and `grader.strategy: injected` require `workspace.source: container_image`. |

### Agent

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `agent.command` | Yes | string array | Command argv. Each part must be a non-empty string. |
| `agent.image` | With `agent_site: agent_container` | image reference | Container image for the agent process. Forbidden with `task_runtime` and `host`. |
| `agent.artifact` | No | object | Explicit agent artifact mount declaration. Omit it for image-native or host-native agents. |
| `agent.artifact.source` | With `agent.artifact` | path or agent build id | Host/package source to stage and mount. |
| `agent.artifact.mount.path` | With `agent.artifact` | absolute runtime path | Container mount path for the artifact, for example `/opt/agent`. |
| `agent.artifact.mount.read_only` | With `agent.artifact` | boolean | Whether the artifact mount is read-only. |
| `agent.artifact.digest` | No | string | Optional expected artifact digest. Usually package-generated. |
| `agent.artifact.resolved_path` | No | string | Optional package-generated resolved source path. |
| `agent.integration_level` | No | `cli_basic`, `cli_events` | Agent integration level. Defaults to `cli_basic`. |
| `agent.network` | No | `none`, `full`, `allowlist_enforced`, `llm_egress` | Agent network request. Defaults from `policy.task_sandbox.network`, then `none`. |
| `agent.env` | No | object of string values | Environment variables for the agent. Values may use `$NAME`. |
| `agent.events` | No | array | Optional file-backed JSONL event capture declarations. |
| `agent.telemetry` | No | object | Optional telemetry settings. |
| `agent.output_mounts` | No | array | Extra writable directories under `/agentlab/out`. |
| `agent.secret_files` | No | array | Launch-time file secrets mounted into the agent runtime. |
| `agent.outputs` | Yes | map | Named outputs captured after the agent runs. Must include `result`. |

`agent.outputs.result` is mandatory and must capture the canonical result file:

```yaml
result:
  capture:
    type: file
    path: /agentlab/out/result.json
    format: json
```

#### Agent Events

`agent.events` declares file-backed JSONL event streams produced by the agent or
its tools. Declaring an event capture means the runner ingests new rows into
SQLite while the trial is still running. This is the live observability surface;
metrics remain separate derived values declared under top-level `metrics`.

```yaml
trial_runtime:
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

The placeholder `__AGENTLAB_EVENT_PATH_<id>__` resolves to the capture path at
launch. Event rows are stored as native JSON payloads; the runner adds internal
row identity and transport metadata when writing SQLite. Do not declare that
internal envelope in YAML. Retaining the source JSONL file as an artifact is
separate and opt-in with `retain_raw: true`.

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `id` | Yes | transport id | Letters, digits, `_`, or `-`. |
| `format` | No | `jsonl` | Capture format. Defaults to `jsonl`. |
| `path` | No | path under `/agentlab/out/` | Event file path. |
| `mode` | No | `jsonl` | File mode. Defaults to `jsonl`. |
| `retain_raw` | No | boolean | Whether to keep the raw JSONL as an artifact. Defaults to `false`. |
| `ingest` | No | boolean | Whether to ingest events into SQLite while the trial runs and at finalization. Defaults to `true`. |

#### Agent Output Mounts

```yaml
output_mounts:
  - id: traces
    kind: directory
    path: traces
    env: AGENTLAB_TRACES_DIR
    persist: true
```

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `id` | Yes | transport id | Letters, digits, `_`, or `-`; must be unique. |
| `kind` | No | `directory` | Only `directory` is supported. |
| `path` | Yes | relative path | Relative to `/agentlab/out`; cannot contain empty, `.`, or `..` segments. |
| `env` | No | uppercase env var | Optional env var pointing at the mounted directory. |
| `persist` | No | boolean | Defaults to `true`. |

#### Agent Secret Files

```yaml
secret_files:
  - id: api_key
    target: /run/secrets/api_key
    required_for_variants: ["candidate"]
```

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `id` | Yes | non-empty string | Secret id resolved from launch-time secret file inputs. |
| `target` | Yes | absolute runtime path | Mount target. Cannot be under reserved runner paths `/agentlab/in`, `/agentlab/out`, or `/opt/agent`. |
| `required_for_variants` | No | string array | Variant ids that must provide this secret. Empty means optional for all variants. |

### Execution

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `execution.agent_site` | Yes | `task_runtime`, `agent_container`, `host` | Where the agent command runs. |

| Value | Description |
| --- | --- |
| `task_runtime` | Runs the agent inside the task container. Requires `task.interface: writable_workspace` and `workspace.source: container_image`. Forbids `agent.image`. Declare `agent.artifact` only when the command needs mounted agent files. |
| `agent_container` | Runs the agent in its own container. Requires `agent.image`. |
| `host` | Runs the agent on the host. Forbids `agent.image`. |

### Grader

```yaml
grader:
  strategy: in_task_runtime
  command: ["bash", "-lc", "python /workspace/eval.py --patch /tmp/candidate.patch --out /tmp/report.json"]
  inputs:
    candidate_patch:
      source:
        output: agent.patch
      materialize:
        as: file
        path: /tmp/candidate.patch
      required: true
  outputs:
    report:
      capture:
        type: file
        path: /tmp/report.json
        format: json
```

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `grader.strategy` | Yes | `none`, `in_task_runtime`, `injected`, `separate`, `host` | Grader execution mode. |
| `grader.command` | Yes unless `strategy: none` | string array | Grader command argv. Must be omitted or empty with `strategy: none`. |
| `grader.inputs` | No | map | Named inputs materialized for the grader. Forbidden with `strategy: none`. |
| `grader.outputs` | Yes unless `strategy: none` | map | Named outputs captured after grading. Forbidden with `strategy: none`. |
| `grader.max_concurrency` | No | positive integer | Optional grader concurrency hint. |
| `grader.in_task_runtime.hidden_paths` | No | string array | Paths hidden before an in-task grader runs. |
| `grader.in_task_runtime.revealed_paths` | No | string array | Paths revealed to an in-task grader. |
| `grader.injected.bundle` | With `strategy: injected` | path | Grader bundle to inject. |
| `grader.injected.copy_dest` | With `strategy: injected` | absolute runtime path | Destination path for injected grader files. |
| `grader.separate.image` | With `strategy: separate` | image reference | Container image for a separate grader. |
| `grader.separate.workdir` | With `strategy: separate` | absolute runtime path | Workdir for a separate grader. |
| `grader.host.capability` | With `strategy: host` | string | Host grader capability id. |

Strategy constraints:

| Strategy | Description |
| --- | --- |
| `none` | No grading phase. No grader command, inputs, or outputs. |
| `in_task_runtime` | Runs grader command in the task runtime. Requires writable container-image task workspace. |
| `injected` | Injects grader files into the task runtime, then runs the command. Requires writable container-image task workspace. |
| `separate` | Runs grader command in a separate container. Requires `separate.image` and `separate.workdir`. |
| `host` | Runs a host grader capability. Requires `host.capability`. |

#### Grader Inputs

Each grader input has exactly one source and one materialization.

```yaml
inputs:
  candidate_patch:
    source:
      output: agent.patch
    materialize:
      as: file
      path: /tmp/candidate.patch
    required: true

  task_meta:
    source:
      object:
        task_id:
          task: /id
        language:
          task: /metadata/language
    materialize:
      as: json_file
      path: /tmp/task_meta.json
```

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `source.output` | One source required | `agent.<output_id>` | Reads a captured agent output. |
| `source.field` | No | string | Optional field selection from a captured output. |
| `source.task` | One source required | JSON pointer or dotted field | Reads from the task row. |
| `source.object` | One source required | object | Builds an object from nested input sources. |
| `materialize.as` | Yes | `file`, `json_file`, `env` | How the input is exposed to the grader command. |
| `materialize.path` | With `file` or `json_file` | absolute runtime path | File path to create for the grader. |
| `materialize.name` | With `env` | uppercase env var | Environment variable name. |
| `required` | No | boolean | Whether missing input data fails the trial. |

Reserved materializations `stdin`, `json_body`, and `multipart_field` are intentionally not executable yet. The runner rejects them today.

## Output Captures

Agent and grader outputs use the same capture schema. Output ids must be non-empty and contain only ASCII letters, digits, `_`, or `-`.

| Capture type | Required fields | Description |
| --- | --- | --- |
| `file` | `path`, `format` | Captures an absolute runtime file as `json`, `text`, or `bytes`. |
| `result_json` | `path` | Reads a JSON result file and optionally selects `field`. |
| `workspace_diff` | `format: unified_diff` | Captures a git diff from a writable workspace. Requires a container runtime. |

Examples:

```yaml
outputs:
  result:
    capture:
      type: file
      path: /agentlab/out/result.json
      format: json

  patch:
    capture:
      type: workspace_diff
      format: unified_diff

  score:
    capture:
      type: result_json
      path: /tmp/report.json
      field: score
```

## Metrics

Metrics are declarations. The runner only persists metric observations for declared metric ids.

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `id` | Yes | non-empty string | Metric id. |
| `label` | No | string | Human-readable label. |
| `semantic_key` | No | string | Cross-experiment semantic identifier. |
| `value_type` | No | string | Suggested value type such as `number`, `boolean`, or `string`. |
| `unit` | No | string | Unit label. |
| `direction` | No | string | Suggested optimization direction such as `maximize` or `minimize`. |
| `required` | No | boolean | Missing metric should be treated as a validity problem by consumers. |
| `primary` | No | boolean | Marks the primary metric. |
| `source` | Yes | object | Where to read the value. |

Metric sources:

| `source.type` | Required fields | Description |
| --- | --- | --- |
| `agent_response` | `pointer` | Reads from the canonical agent result JSON. |
| `grader_output` | `output` | Reads from a named grader output. |
| `runtime_output` | `output` | Currently limited to `agent.result` for metric extraction without a grader. |

```yaml
metrics:
  - id: pass_rate
    label: Pass Rate
    value_type: number
    unit: ratio
    direction: maximize
    primary: true
    source:
      type: grader_output
      output: report
      pointer: /pass_rate
      transform:
        type: identity
```

Executable transforms today are `identity` and `pytest_json_report_pass_rate`.

## Design

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `design.replications` | Yes | positive integer | Number of times to run each task and variant slot. |
| `design.random_seed` | No | unsigned integer | Seed for deterministic scheduling. Defaults to `1`. |
| `design.shuffle_tasks` | No | boolean | Whether to shuffle scheduled slots. |
| `design.max_concurrency` | No | positive integer | Max concurrent slots. Defaults to `1`; values below `1` are clamped to `1`. |
| `design.comparison` | No | `paired` | Comparison design label. |
| `design.sanitization_profile` | No | `perf_benchmark`, `hermetic_functional`, `replay_strict` | Optional profile. Defaults to `perf_benchmark`. |

Hermetic rule: if `design.sanitization_profile`, `policy.sanitization_profile`, or `policy.task_sandbox.profile` is `hermetic_functional`, then `policy.task_sandbox.network` must be `none` and any explicit `trial_runtime.agent.network` must also be `none`.

## Policy

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `policy.timeout_ms` | Yes | positive integer | Trial timeout in milliseconds. |
| `policy.sanitization_profile` | No | `perf_benchmark`, `hermetic_functional`, `replay_strict` | Alternate profile location. Prefer `design.sanitization_profile`. |
| `policy.task_sandbox.network` | Yes | `none`, `full`, `allowlist_enforced`, `llm_egress` | Task sandbox network request. `none` maps to Docker network none; the others currently map to Docker default networking. |
| `policy.task_sandbox.profile` | No | `perf_benchmark`, `hermetic_functional`, `replay_strict` | Task sandbox profile. |
| `policy.task_sandbox.allowed_hosts` | No | string array | Host allowlist metadata for restricted network modes. |
| `policy.task_sandbox.resources.cpu_count` | No | unsigned integer | CPU count hint for the task sandbox. |
| `policy.task_sandbox.resources.memory_mb` | No | unsigned integer | Memory hint in MiB. |
| `policy.task_sandbox.hardening.no_new_privileges` | No | boolean | Defaults to `true`. |
| `policy.task_sandbox.hardening.drop_all_caps` | No | boolean | Defaults to `true`. |

## Validity

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `validity.fail_on_state_leak` | No | boolean | Fails the run when state leak checks detect invalid reuse. |
| `validity.fail_on_profile_invariant_violation` | No | boolean | Fails when effective profile invariants are violated. Use `true` for CI and benchmark runs. |

## Artifacts

| Field | Required | Values | Description |
| --- | --- | --- | --- |
| `artifacts.collect` | No | string array | Globs or paths for additional artifact collection. |
| `artifacts.diff` | No | boolean | Whether to collect diffs where supported. |
| `artifacts.base_dir` | No | path | Base directory for artifact collection. |

## Removed Or Unsupported Fields

These fields are intentionally not part of the public YAML surface:

| Field | Replacement |
| --- | --- |
| `runtime` | `trial_runtime` |
| `task_runtime` | `trial_runtime.task` |
| `runtime.agent`, `runtime.agent_runtime` | `trial_runtime.agent` |
| `runtime.dependencies` | `trial_runtime.agent.artifact`, `secret_files`, `output_mounts`, and declared task workspace/files |
| `trial_runtime.outputs` | `trial_runtime.agent.outputs` and `trial_runtime.grader.outputs` |
| `trial_runtime.grader.conclusion` | Declared grader outputs plus `metrics` |
| `trial_runtime.grader.support_files` | Strategy-specific grader files, such as `grader.injected.bundle` |
| `trial_runtime.agent.secret_env` | `$NAME` interpolation in `agent.env` or launch-time env inputs |
| `trial_runtime.agent.env_from_host` | Explicit env values or launch-time inputs |
| `trial_runtime.agent.workspace_patches` | `outputs.<id>.capture.type: workspace_diff` |
| `grader_result` metric source | `grader_output` |
| `grader_input_v1`, `AGENTLAB_GRADER_INPUT_PATH` | Declared `grader.inputs` materialized as `file`, `json_file`, or `env` |

If a field shapes execution, transport, metrics, policy, or validity, it should be visible in this reference. If it is not here, assume it is unsupported unless a later reference update adds it.
