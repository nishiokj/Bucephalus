# Patch Spec: Trial Runtime Interface Hard Cutover

Status: Draft (Hard Cut Required)
Date: 2026-05-14
Owner: `lab-runner`, `lab-cli`, `docs/user`, schemas
Priority: P0 Runtime Contract / Experiment Generality

## 1. Intent

Replace the current SWE-bench-shaped runtime model with one first-class trial runtime contract.

The runner should not decide whether a trial is valid by asking isolated questions such as:

1. Does every task row have an image?
2. Does every experiment have a grader command?
3. Does every agent runtime have a container image?
4. Is `task_runtime.kind` equal to `docker`?

Those questions encode one narrow benchmark shape. The correct question is:

> Can the runner construct a complete trial execution plan that connects the task interface, agent process, output collection, and optional grader?

After this patch:

1. `trial_runtime` is the canonical runtime configuration surface.
2. Task rows describe task payloads and per-task materialization data only when the declared task interface requires it.
3. No grader is a valid experiment shape when metrics/artifacts come from agent output.
4. No task image is required for `input_only` or file-backed task interfaces.
5. Agent execution is validated as part of the trial runtime plan, not as an unconditional container requirement.
6. Preflight validates the composed trial plan instead of validating unrelated fragments with Docker-only assumptions.
7. Old top-level `runtime.agent_runtime` and `task_runtime` authoring shapes are rejected. No aliases, migrations, fallbacks, or silent defaults.

## 2. Problem

The current implementation has several useful pieces, but their validation boundaries are wrong for general experiments.

Current hard-coded assumptions:

1. `rust/crates/lab-runner/src/package/validate.rs` requires `/runtime/agent_runtime/artifact`, `/runtime/agent_runtime/image`, `/runtime/agent_runtime/command`, `/task_runtime/kind`, and `/benchmark/grader/command`.
2. `rust/crates/lab-runner/src/trial/spec.rs` defines `TaskRow.image: String` and rejects empty task images.
3. `rust/crates/lab-runner/src/experiment/preflight.rs` rejects any `task_runtime.kind` except `docker`.
4. Trial execution assumes a task sandbox container is always materialized before the agent runs.
5. `in_task_image` grading is treated as the implicit default strategy when the grader strategy is missing.
6. Runtime docs present agent runtime and task runtime as separate top-level concerns even though the runner composes them into one execution plan.

These assumptions make real non-SWE-bench experiments awkward or impossible:

1. Data-only experiments that pass JSON tasks into an agent.
2. Pipeline experiments where the agent emits several metrics and no grader is needed.
3. File-backed experiments where the task is a local fixture, archive, or git checkout, not a task image.
4. Agent-container experiments where the task workspace is mounted into the agent image.
5. Host-script experiments where a local script consumes task input and emits result JSON.

## 3. Non-Negotiable Invariants

1. There is one public runtime surface: `trial_runtime`.
2. Public authoring must not accept both old and new runtime shapes.
3. `runtime.agent_runtime`, `runtime.agent`, `runtime.sandbox`, `task_runtime`, and `benchmark.grader` as the grader execution surface are rejected in authored experiments.
4. The runner must not synthesize a fake task image, fake workdir, fake grader, fake patch, or fake metric to preserve old paths.
5. A task image is required only when the selected trial plan needs a task container.
6. An agent image is required only when the selected trial plan runs the agent in an agent-owned container.
7. A grader is optional. If no grader is declared, grading stages are `not_applicable`/`not_run`, and score/metrics must come from declared agent-output metrics or the documented fallback.
8. Every accepted interface/source/execution-site combination has one explicit planner path and one explicit preflight path.
9. Unsupported combinations are rejected with direct compatibility errors.
10. Reserved future values are not accepted.
11. Documentation, examples, schemas, package validation, preflight, run, replay, fork, and analysis must agree on the same contract.

## 4. New Public Contract

### 4.1 Top-level shape

The runtime surface moves under `trial_runtime`:

```yaml
trial_runtime:
  task:
    interface: input_only | readonly_files | writable_workspace
    ...

  agent:
    command: [...]
    ...

  execution:
    agent_site: task_runtime | agent_container | host

  outputs:
    result:
      path: /agentlab/out/result.json
    patch:
      mode: none | workspace_diff | file
    metrics:
      source: agent_response | grader_result

  grader:
    strategy: none | in_task_runtime | injected | separate | host
    ...
```

`trial_runtime` is not a loose namespace. It is the unit the planner validates.

### 4.2 Task interface

`trial_runtime.task.interface` describes what the task provides to the agent.

Accepted values in this patch:

1. `input_only`
2. `readonly_files`
3. `writable_workspace`

Do not accept `service`, `external`, `browser`, `api`, or other future-looking values in this patch.

#### `input_only`

The task is just payload data. The runner writes it into the trial input envelope. No task image, task workdir, workspace, or patch extraction is required.

Valid task row:

```json
{
  "schema_version": "task_row_v2",
  "id": "case_001",
  "task": {
    "question": "..."
  }
}
```

Requirements:

1. `task_row_v2.id` is non-empty.
2. `task_row_v2.task` is an object.
3. No `task_row_v2.image`.
4. No `task_row_v2.workdir`.
5. No task workspace source.

Compatible agent sites:

1. `agent_container`
2. `host`

Incompatible agent sites:

1. `task_runtime`, because there is no task process/runtime to run inside.

#### `readonly_files`

The task provides files the agent may inspect but not modify.

Example:

```yaml
trial_runtime:
  task:
    interface: readonly_files
    files:
      source: files
      path: ./fixtures/docs
      mount_path: /workspace/task
```

Requirements:

1. `task.files.source` is `files` or `archive`.
2. `task.files.path` is package-relative after build.
3. `task.files.mount_path` is absolute.
4. The runner mounts/copies files read-only into the selected agent site.
5. Patch extraction must be `none`.

Compatible agent sites:

1. `agent_container`
2. `host`

Incompatible agent sites:

1. `task_runtime`, unless a future task process runtime is explicitly added. Do not add that in this patch.

#### `writable_workspace`

The task provides a writable workspace. The agent may edit files, and the runner may collect a patch or changed-file artifact.

Example using a task container:

```yaml
trial_runtime:
  task:
    interface: writable_workspace
    workspace:
      source: container_image
      image:
        from: task_row
      workdir:
        from: task_row
```

Example using packaged files:

```yaml
trial_runtime:
  task:
    interface: writable_workspace
    workspace:
      source: files
      path: ./fixtures/project
      workdir: /workspace/task
```

Example using git:

```yaml
trial_runtime:
  task:
    interface: writable_workspace
    workspace:
      source: git
      repo: https://github.com/acme/project
      ref: 4d2c1b0
      workdir: /workspace/task
```

Accepted workspace sources in this patch:

1. `container_image`
2. `files`
3. `archive`
4. `git`
5. `empty`

Requirements by source:

| Source | Required Fields | Requires Container | Provides Process Runtime |
| --- | --- | --- | --- |
| `container_image` | image, workdir | yes | yes |
| `files` | path, workdir | no | no |
| `archive` | path, workdir | no | no |
| `git` | repo, ref, workdir | no | no |
| `empty` | workdir | no | no |

Compatibility:

1. `agent_site: task_runtime` requires `workspace.source: container_image`.
2. `agent_site: agent_container` works with all workspace sources; the workspace is mounted into the agent container.
3. `agent_site: host` works with all workspace sources; the workspace is a host path.
4. Patch extraction with `workspace_diff` requires `task.interface: writable_workspace`.

### 4.3 Agent execution

`trial_runtime.agent` describes the agent application and invocation contract.

It does not own a `placement` field. The relationship between agent and task belongs under `trial_runtime.execution`.

Base shape:

```yaml
trial_runtime:
  agent:
    artifact: ./agent
    command: ["agent", "run"]
    env: {}
    event_sinks: []
    output_mounts: []
```

`trial_runtime.execution.agent_site` controls where the agent process runs.

Accepted values:

1. `task_runtime`
2. `agent_container`
3. `host`

Do not accept `external` in this patch.

#### `agent_site: task_runtime`

The agent artifact is copied or mounted into the task process runtime and executed there.

Requirements:

1. `trial_runtime.task.interface` is `writable_workspace`.
2. `trial_runtime.task.workspace.source` is `container_image`.
3. `trial_runtime.agent.artifact` is required.
4. `trial_runtime.agent.command` is required.
5. `trial_runtime.agent.image` is forbidden.
6. The task row must provide the image/workdir if the workspace image/workdir use `from: task_row`.

This is the current SWE-bench-like execution path, renamed and constrained.

#### `agent_site: agent_container`

The agent runs in its own container. Task payload/files/workspace are mounted or copied into that container according to the task interface.

Requirements:

1. `trial_runtime.agent.image` is required.
2. `trial_runtime.agent.command` is required.
3. `trial_runtime.agent.artifact` is optional. If present, it is staged into the agent container.
4. `input_only`, `readonly_files`, and `writable_workspace` are all supported.
5. For `writable_workspace`, the planner must mount the writable workspace into the agent container at the declared workdir.

#### `agent_site: host`

The agent runs as a host process.

Requirements:

1. `trial_runtime.agent.command` is required.
2. `trial_runtime.agent.image` is forbidden.
3. `trial_runtime.agent.artifact` is optional. If present, it is package-local and executed/read from the package/run materialization area.
4. Host execution must be explicit in the experiment. Do not infer it from a missing image.
5. Host paths must be package-local, run-local, or declared secret/runtime bindings. Arbitrary authored absolute paths are rejected.

### 4.4 Execution connection

The runner derives concrete pathing from task interface and agent site.

User-facing concepts:

1. `input_only`: task payload is available in `trial_input.json`.
2. `readonly_files`: task files are available read-only at the declared task path.
3. `writable_workspace`: task workspace is writable at the declared task path.

Do not expose terms such as `contract_only`, `mounted_workspace`, or `shared_filesystem` in public docs.

Internal planner output may include mount/copy strategies, but those are implementation details.

### 4.5 Outputs

`trial_runtime.outputs` declares what the runner collects.

Minimum shape:

```yaml
trial_runtime:
  outputs:
    result:
      path: /agentlab/out/result.json
    patch:
      mode: none
```

Patch modes:

1. `none`: no patch expected.
2. `workspace_diff`: diff the writable workspace.
3. `file`: collect a declared patch file.

Rules:

1. `workspace_diff` requires `task.interface: writable_workspace`.
2. `file` requires `outputs.patch.path`.
3. `none` is valid for metrics-only and pipeline experiments.
4. Do not infer patch mode from benchmark name, task payload shape, or agent result fields.

### 4.6 Metrics and no-grader experiments

Metrics are first-class and may come from the agent response or grader result.

Example with no grader:

```yaml
trial_runtime:
  grader:
    strategy: none

metrics:
  - id: latency_ms
    source:
      type: agent_response
      pointer: /metrics/latency_ms
    value_type: number
    unit: ms
    direction: minimize
    primary: true
```

Rules:

1. `trial_runtime.grader.strategy: none` means no grader command is required.
2. `benchmark.grader` is no longer the public grader execution surface.
3. If no grader is declared, `grader_input_mapping`, `grader_execution`, and `grade_mapping` are reported as not applicable/not run.
4. Declared metrics with `source.type: agent_response` are extracted from the agent response body.
5. Declared metrics with `source.type: grader_result` require a non-`none` grader.
6. A no-grader experiment must declare at least one metric or accept the documented fallback `success` metric.
7. The fallback `success` metric is derived only from agent execution/result validity, not from hidden grader behavior.

### 4.7 Grader execution

Move grader execution under `trial_runtime.grader`.

Accepted strategies:

1. `none`
2. `in_task_runtime`
3. `injected`
4. `separate`
5. `host`

Hard rename:

1. Old `in_task_image` becomes `in_task_runtime`.
2. Old `benchmark.grader` is rejected.
3. No alias accepts `in_task_image`.

#### `none`

No grader runs.

Requirements:

1. No `command`.
2. No grader runtime fields.
3. No metrics may use `source.type: grader_result`.

#### `in_task_runtime`

The grader runs in the task runtime after the agent.

Requirements:

1. `task.interface: writable_workspace`.
2. `task.workspace.source: container_image`.
3. `command` required.
4. No separate image.
5. No host capability.

#### `injected`

The grader bundle is copied into the task runtime and run there.

Requirements:

1. Same task runtime requirements as `in_task_runtime`.
2. `injected.bundle` required.
3. `injected.copy_dest` required.
4. `command` required.

#### `separate`

The grader runs in a grader-owned container.

Requirements:

1. `separate.image` required.
2. `separate.workdir` required.
3. `command` required.
4. The task output/result artifacts needed by the grader are mounted/copied into that container.

#### `host`

The grader runs as a host process through a declared package-scoped host grader tool/capability.

Requirements:

1. `host.capability` required.
2. `command` required.
3. Command may reference only packaged host grader capability files and runner contract paths.
4. Host grader files are staged into `host_grader_capabilities/<id>/...`.

Naming note:

The implementation may keep the internal term `capability`, but user docs should prefer `host grader tool` unless the permission boundary is being explained.

## 5. New Task Row Contract

### 5.1 `task_row_v2`

The public task row schema changes to `task_row_v2`.

Base shape:

```json
{
  "schema_version": "task_row_v2",
  "id": "task_001",
  "task": {}
}
```

Optional per-task runtime fields are grouped under `runtime`.

Container-backed workspace task:

```json
{
  "schema_version": "task_row_v2",
  "id": "swebench_astropy_12907",
  "task": {},
  "runtime": {
    "container_image": {
      "image": "ghcr.io/epoch-research/swe-bench.eval.x86_64.astropy__astropy-12907:latest",
      "workdir": "/testbed",
      "platform": "linux/amd64"
    }
  }
}
```

Rules:

1. `id` required.
2. `task` object required.
3. `runtime.container_image` allowed only when `trial_runtime.task.workspace.source: container_image`.
4. `runtime.container_image.image` required if experiment says `image.from: task_row`.
5. `runtime.container_image.workdir` required if experiment says `workdir.from: task_row`.
6. No top-level `image`.
7. No top-level `workdir`.
8. No `materialization.kind`.
9. No `task_bundle_ref` in public task rows.

### 5.2 Rejection of `task_row_v1`

Hard cut:

1. Package build rejects public `task_row_v1`.
2. Runtime rejects packaged `task_row_v1`.
3. Tests and bundled manifests are rewritten to `task_row_v2`.
4. No converter runs at build time.
5. No compatibility parser remains on the production path.

If internal tests need old rows to prove rejection, name them `unsupported_task_row_v1_*`.

## 6. Trial Planner

Add a first-class planner module:

```text
rust/crates/lab-runner/src/trial/plan.rs
```

The planner consumes:

1. resolved experiment JSON
2. selected variant runtime bindings
3. selected `task_row_v2`
4. package/run root
5. execution policy

The planner produces:

```rust
TrialExecutionPlan {
    task: PlannedTask,
    agent: PlannedAgent,
    execution: PlannedExecution,
    outputs: PlannedOutputs,
    grader: PlannedGrader,
}
```

The exact Rust names can vary, but the boundary cannot: execution code receives one complete trial plan, not scattered runtime fragments.

### 6.1 Compatibility matrix

The planner must enforce this matrix:

| Task Interface | Workspace Source | Agent Site `task_runtime` | Agent Site `agent_container` | Agent Site `host` |
| --- | --- | --- | --- | --- |
| `input_only` | none | reject | accept | accept |
| `readonly_files` | `files` | reject | accept | accept |
| `readonly_files` | `archive` | reject | accept | accept |
| `writable_workspace` | `container_image` | accept | accept | accept |
| `writable_workspace` | `files` | reject | accept | accept |
| `writable_workspace` | `archive` | reject | accept | accept |
| `writable_workspace` | `git` | reject | accept | accept |
| `writable_workspace` | `empty` | reject | accept | accept |

Reject reasons must name the missing capability, not the old field:

Good:

```text
agent_site=task_runtime requires a task workspace source that provides a process runtime; source 'files' does not
```

Bad:

```text
task row field 'image' must be non-empty
```

### 6.2 Grader compatibility matrix

| Grader Strategy | Required Task Shape | Required Grader Runtime |
| --- | --- | --- |
| `none` | any | none |
| `in_task_runtime` | `writable_workspace` + `container_image` | task container |
| `injected` | `writable_workspace` + `container_image` | task container + injected bundle |
| `separate` | any | grader container |
| `host` | any | host grader capability |

Rules:

1. `grader_result` metrics require non-`none` grader.
2. `in_task_runtime` and `injected` are rejected unless the task workspace has a process runtime.
3. `host` grader paths are package-scoped only.
4. `separate` grader must not depend on task container image unless explicitly mounted data is available.

## 7. Package Build Changes

### 7.1 Authoring normalization

Files:

1. `rust/crates/lab-runner/src/package/authoring.rs`
2. `rust/crates/lab-runner/src/package/registry.rs`
3. benchmark manifests under `manifests/benchmarks`

Required changes:

1. Emit `trial_runtime`, not `runtime.agent_runtime` plus `task_runtime`.
2. Built-in benchmark manifests define their task interface and workspace source.
3. Built-in SWE-bench manifest becomes:

```yaml
trial_runtime:
  task:
    interface: writable_workspace
    workspace:
      source: container_image
      image:
        from: task_row
      workdir:
        from: task_row
  execution:
    agent_site: task_runtime
  grader:
    strategy: host
    host:
      capability: swebench_official
    command: [...]
```

4. The simple demo benchmark becomes either `input_only` with no grader or `writable_workspace` with an explicit file/container source.
5. Remove authoring defaults that silently invent `in_task_image`, Docker task runtime, or required grader command.

### 7.2 Task packaging

Files:

1. `rust/crates/lab-runner/src/package/compile.rs`
2. `rust/crates/lab-runner/src/trial/spec.rs`
3. `scripts/acquire_swebench_lite.py`
4. `scripts/build_swebench_lite_task_boundary_v3.py`

Required changes:

1. Replace `TaskRow` with `TaskRowV2`.
2. Remove top-level `image`, `workdir`, and `materialization`.
3. Move per-task container fields under `runtime.container_image`.
4. Image rewrite rules operate on `/runtime/container_image/image`.
5. Platform is stored under `/runtime/container_image/platform`.
6. Package validation uses the selected `trial_runtime.task` to decide which task row fields are required.
7. No package build converter accepts old rows.

### 7.3 Runtime package layout

No new top-level package storage system is required in this patch.

However, package manifests and run dirs must stop implying all tasks are task-image tasks:

1. prepared task environment JSON should record `task_interface`, `workspace_source`, and `agent_site`.
2. task sandbox state should be optional for `input_only`, `readonly_files`, and host/agent-container workspace execution that does not create a task container.
3. fields named `task_sandbox_image` should be replaced or narrowed to `task_container_image` where they truly mean a task container.

## 8. Runtime Execution Changes

Files:

1. `rust/crates/lab-runner/src/trial/prepare.rs`
2. `rust/crates/lab-runner/src/trial/execution.rs`
3. `rust/crates/lab-runner/src/trial/env.rs`
4. `rust/crates/lab-runner/src/trial/grade.rs`
5. `rust/crates/lab-runner/src/trial/state.rs`
6. `rust/crates/lab-runner/src/trial/schedule.rs`

Required changes:

1. Build a `TrialExecutionPlan` before materialization.
2. `execute_trial_runtime` takes the plan.
3. Task container creation is conditional.
4. Agent execution dispatches on `agent_site`.
5. Agent contract env is generated from the plan, not from task-image assumptions.
6. Grader execution dispatches on `trial_runtime.grader.strategy`.
7. Result, patch, events, output mounts, and metrics are collected through `trial_runtime.outputs`.
8. Attempt state records absent task container as absent, not as an error.
9. Summary JSON reports grader status as `not_configured` or `not_run` when strategy is `none`.
10. Contract trace says `score_source: agent_response` for no-grader metrics.

### 8.1 Execution paths

#### Path A: `input_only` + `agent_container`

1. Write `trial_input.json`.
2. Start agent container.
3. Mount `/agentlab/in`, `/agentlab/out`, runtime artifact if present, and any declared output mounts.
4. Run agent command.
5. Read result JSON.
6. Extract metrics from agent response.
7. Skip grader.

No task container exists.

#### Path B: `input_only` + `host`

1. Write `trial_input.json`.
2. Run host command with contract env.
3. Read result JSON.
4. Extract metrics.
5. Skip grader.

No task container exists.

#### Path C: `writable_workspace/container_image` + `task_runtime`

This is the current task-container injection path.

1. Materialize task container from task row container image.
2. Stage agent artifact into task container.
3. Run agent command in task container workdir.
4. Collect result and patch according to outputs.
5. Run grader if configured.

#### Path D: `writable_workspace/files|archive|git|empty` + `agent_container`

1. Materialize workspace on host.
2. Start agent container.
3. Mount workspace writable at declared workdir.
4. Run agent command.
5. Diff workspace or collect patch file if declared.
6. Extract metrics and optionally run separate/host grader.

#### Path E: `writable_workspace/files|archive|git|empty` + `host`

1. Materialize workspace on host.
2. Run host agent command with workspace env/path.
3. Diff workspace or collect patch file if declared.
4. Extract metrics and optionally run separate/host grader.

## 9. Validation Changes

### 9.1 Replace scattered required-field validation

File:

```text
rust/crates/lab-runner/src/package/validate.rs
```

Delete unconditional requirements for:

1. `/runtime/agent_runtime`
2. `/runtime/agent_runtime/artifact`
3. `/runtime/agent_runtime/image`
4. `/runtime/agent_runtime/command`
5. `/task_runtime/kind`
6. `/benchmark/grader/command`

Replace with:

1. `/trial_runtime/task/interface`
2. `/trial_runtime/agent/command`
3. `/trial_runtime/execution/agent_site`
4. `/trial_runtime/outputs/result/path`
5. `/trial_runtime/grader/strategy`
6. conditional requirements from the planner matrix

### 9.2 Reject old shapes

Add hard rejection for:

1. `/runtime`
2. `/task_runtime`
3. `/benchmark/grader`
4. `/benchmark/adapter`
5. task row `schema_version=task_row_v1`
6. `trial_runtime.grader.strategy=in_task_image`
7. `benchmark.image_source`

Error messages must say what the current field is, not describe migration history.

Example:

```text
/runtime is not a supported experiment field; define execution under /trial_runtime
```

### 9.3 Validate task row against trial runtime

Validation of task rows requires experiment context.

Examples:

1. `input_only` rejects any per-task `runtime.container_image`.
2. `container_image` source with `image.from: task_row` requires each task row to provide `runtime.container_image.image`.
3. `container_image` source with a fixed experiment image forbids per-task image unless an explicit override field is added. Do not add override in this patch.
4. `readonly_files` rejects patch outputs.
5. `writable_workspace` with `outputs.patch.mode=workspace_diff` requires a workspace source.

## 10. Preflight Changes

File:

```text
rust/crates/lab-runner/src/experiment/preflight.rs
```

Replace Docker-centric checks with plan-centric checks.

New preflight stages:

1. `trial_runtime_schema`
2. `trial_runtime_compatibility`
3. `agent_executor_reachable`
4. `task_materialization_reachable`
5. `workspace_materialization`
6. `grader_reachable`
7. `metric_sources`
8. `output_collection`

### 10.1 `trial_runtime_schema`

Validates accepted enum values and required fields.

### 10.2 `trial_runtime_compatibility`

Runs the planner against at least one representative task row per distinct task materialization shape.

### 10.3 `agent_executor_reachable`

Checks only the executor the plan actually uses:

1. `agent_container`: image can be pulled/inspected and command smoke can run.
2. `host`: command binary/path is available or package-local artifact can be materialized.
3. `task_runtime`: defer image check to task materialization and verify artifact staging.

### 10.4 `task_materialization_reachable`

Checks only if the task interface/source needs materialization:

1. `input_only`: pass.
2. `readonly_files/files`: packaged files exist.
3. `readonly_files/archive`: archive exists and can be listed/unpacked.
4. `writable_workspace/container_image`: task images can be pulled/inspected.
5. `writable_workspace/git`: repo/ref can be resolved if network policy allows it; otherwise package build must have vendored the workspace.
6. `writable_workspace/empty`: pass.

### 10.5 `grader_reachable`

Checks only if `grader.strategy != none`.

No-grader experiments must not fail preflight because grader outputs do not exist.

### 10.6 `metric_sources`

Validates:

1. `agent_response` metrics are allowed for all task interfaces.
2. `grader_result` metrics require `grader.strategy != none`.
3. At least one primary metric exists, or the documented fallback is accepted explicitly.

## 11. Persistence, Summaries, and Analysis

No new database storage model is required in this patch.

Required data-model changes:

1. Store `task_interface`.
2. Store `workspace_source`.
3. Store `agent_site`.
4. Store `grader_strategy`, including `none`.
5. Store metric source type for each extracted metric.
6. Trial summary must not always include SWE-bench-specific or grader-specific artifact keys.

Summary shape:

```json
{
  "schema_version": "trial_summary_v2",
  "trial_runtime": {
    "task_interface": "input_only",
    "workspace_source": null,
    "agent_site": "agent_container",
    "grader_strategy": "none"
  },
  "agent": {},
  "grader": {
    "status": "not_configured"
  },
  "outputs": {},
  "metrics": {}
}
```

Hard cut:

1. Do not keep `trial_summary_v1` writer on production path.
2. Do not write fixed `official_swebench_eval` artifact entries in generic summaries.
3. Declared benchmark artifacts remain declared artifacts and are copied only when declared.

## 12. Documentation Changes

Files:

1. `docs/user/what-you-provide.md`
2. `docs/user/bring-your-own-agent.md`
3. `docs/user/task-rows.md`
4. `docs/user/graders-and-mappers.md`
5. `docs/user/metrics.md`
6. `docs/user/quickstart.md`
7. `docs/user/agent-runtime-contract.md`
8. `docs/user/troubleshooting.md`

Required docs:

1. Explain `trial_runtime` as the thing the runner executes.
2. Explain task interfaces in plain language:
   - input only
   - read-only files
   - writable workspace
3. Explain agent sites:
   - task runtime
   - agent container
   - host
4. Explain no-grader experiments.
5. Explain when images are required.
6. Explain when patch extraction is possible.
7. Replace old `runtime.agent_runtime` examples.
8. Replace old `task_runtime.kind: docker` examples.
9. Replace old `benchmark.grader` examples.
10. Document `task_row_v2`.

Do not include a migration guide in the user docs. The product has no compatibility promise here.

## 13. Test Plan

### 13.1 Unit tests

Add or replace tests for:

1. `input_only + agent_container + no grader`
2. `input_only + host + no grader`
3. `input_only + task_runtime` rejected
4. `readonly_files + agent_container`
5. `readonly_files + workspace_diff` rejected
6. `writable_workspace/container_image + task_runtime`
7. `writable_workspace/files + task_runtime` rejected
8. `writable_workspace/files + agent_container`
9. `writable_workspace/files + host`
10. `grader.strategy=none` with `grader_result` metric rejected
11. `grader.strategy=none` with `agent_response` metric accepted
12. `in_task_runtime` grader without task container rejected
13. `separate` grader with data-only task accepted
14. old `/runtime` rejected
15. old `/task_runtime` rejected
16. old `/benchmark/grader` rejected
17. old `task_row_v1` rejected
18. old `in_task_image` rejected

### 13.2 Integration tests

Required runnable fixtures:

1. data-only agent-container experiment, no grader, metrics from agent response
2. data-only host-script experiment, no grader, metrics from agent response
3. file-backed writable workspace with agent-container, patch extraction
4. SWE-bench-style container workspace with agent in task runtime and host grader

### 13.3 Audit commands

Add or update audit script:

```bash
rg -n "runtime\\.agent_runtime|task_runtime|benchmark\\.grader|task_row_v1|in_task_image|image_source" \
  rust/crates/lab-runner/src docs/user manifests scripts
```

Allowed hits:

1. rejection tests
2. this patch spec
3. audit docs that explicitly discuss removed surfaces

No production code path should contain old accepted-shape parsing.

## 14. Implementation Order

### Step 1: Add new model types

Files:

1. `model.rs`
2. new `trial/plan.rs`
3. `trial/spec.rs`

Add:

1. `TaskInterface`
2. `WorkspaceSource`
3. `AgentSite`
4. `TrialRuntimeConfig`
5. `TaskRowV2`
6. `TrialExecutionPlan`
7. `GraderStrategy::None`
8. renamed `GraderStrategy::InTaskRuntime`

Remove public use of:

1. `TaskMaterializationKind`
2. `TaskRow.image`
3. `TaskRow.workdir`
4. `TaskRow.materialization`

### Step 2: Replace validation

Replace required-field validation with trial planner validation.

The validation entrypoint should parse the resolved experiment into `TrialRuntimeConfig`, parse task rows as `TaskRowV2`, and run compatibility validation.

### Step 3: Rewrite manifests and fixtures

Rewrite:

1. `manifests/benchmarks/bench_v0/benchmark.yaml`
2. `manifests/benchmarks/swebench_lite_curated/benchmark.yaml`
3. task row fixture generators
4. `.labs/experiment.yaml` if present
5. docs examples

### Step 4: Update package compilation

Compile `task_row_v2`, stage workspace/files/archive/git sources according to `trial_runtime.task`, and remove task-image assumptions from generic package build code.

### Step 5: Update preflight

Preflight consumes `TrialExecutionPlan` and runs only relevant checks.

### Step 6: Update execution

Split materialization into:

1. task input materialization
2. task workspace materialization
3. agent executor materialization
4. grader materialization
5. output collection

Each step is driven by `TrialExecutionPlan`.

### Step 7: Update summaries and analysis

Emit `trial_summary_v2` and store normalized runtime-plan fields in SQLite.

### Step 8: Delete old branches

Remove production parsing/writing for:

1. old runtime paths
2. old task runtime path
3. old benchmark grader path
4. `task_row_v1`
5. `in_task_image`
6. `benchmark.image_source`

### Step 9: Docs and audits

Update docs and add hard-cut audit command to CI or local test checklist.

## 15. Acceptance Criteria

1. No authored experiment with `/runtime`, `/task_runtime`, or `/benchmark/grader` is accepted.
2. No public task row with `schema_version=task_row_v1` is accepted.
3. A no-grader data-only experiment runs and records metrics from agent response.
4. A data-only experiment does not require any task image.
5. An agent-container experiment requires an agent image.
6. A host-agent experiment forbids an agent image and validates host command/path boundaries.
7. A task-runtime agent experiment requires a task workspace source that provides a process runtime.
8. SWE-bench-style experiments still run through the new `trial_runtime` shape.
9. Preflight checks are plan-specific and do not run Docker image checks for data-only host experiments.
10. Trial summaries no longer contain generic SWE-bench artifact hardcoding.
11. Docs show only the new contract.
12. Audit finds no old accepted-shape production path.

## 16. Explicit Non-Goals

1. Do not add browser/service task interfaces in this patch.
2. Do not add remote/external agent execution in this patch.
3. Do not add a generic dependency manager.
4. Do not add plugin registries for task interfaces.
5. Do not preserve old YAML compatibility.
6. Do not auto-convert old task rows.
7. Do not add hidden default graders.
8. Do not add fallback task images.

## 17. Open Design Decisions To Resolve Before Implementation

These must be resolved before code changes begin:

1. Should `host` agent execution be allowed by default, or require an explicit policy flag such as `policy.host_execution: true`?
2. Should `git` workspace source clone at package build time only, or can it clone at run/preflight time when network is allowed?
3. Should `workspace_diff` be implemented uniformly for host and agent-container workspaces in this patch, or should only `file` patch mode be accepted outside task containers initially?
4. Should `trial_summary_v2` fully replace `trial_summary_v1` immediately, or should old summaries only be readable by analysis tools?

Do not implement around these with fallback branches. Decide, then encode the decision in validation.

