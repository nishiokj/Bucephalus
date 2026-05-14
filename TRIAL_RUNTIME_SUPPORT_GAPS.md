# Trial Runtime Support Gaps

This note tracks `trial_runtime` DSL surface that is parsed or allowed but not yet fully honored by the runner. It is intentionally not user-facing documentation. The goal is to keep the DSL direction visible while preventing us from mistaking declared shape for implemented behavior.

## Currently Operational

- `agent_site: task_runtime` with `task.interface: writable_workspace` and `workspace.source: container_image`.
- `trial_runtime.agent.command`, `artifact`, `env`, `events`, `output_mounts`, `secret_files`, `network`, and `integration_level` through the existing agent runtime path.
- `grader.strategy: in_task_runtime`, `injected`, `separate`, and `host` through the existing grader machinery when the trial has a task container.
- `grader.strategy: none` for no-grader runs that extract metrics from agent output.
- Task row `runtime.container_image.image`, `workdir`, and `platform`.

## Declared But Not Fully Honored

- `agent_site: agent_container`
  - Validation accepts it.
  - Execution still uses `task_sandbox_plan.image`, which comes from the task row, not `trial_runtime.agent.image`.
  - Input-only agent-container trials can therefore reach execution with no usable container image.

- `agent_site: host` with a grader
  - Validation accepts the shape.
  - The host execution branch only runs when grading is disabled.
  - Host-agent plus grader currently falls through to Docker/task-runtime execution.

- `task.interface: input_only`
  - Plausible only for host agent with no grader today.
  - Other accepted-looking combinations still hit container/task-sandbox assumptions.

- `task.interface: readonly_files`
  - Parsed and validated.
  - `task.files.source`, `path`, and `mount_path` are not materialized or mounted.

- `workspace.source: files`, `archive`, `git`, and `empty`
  - Parsed as DSL values.
  - No workspace materialization path exists yet.
  - Only `workspace.source: container_image` is operational today.

- `trial_runtime.outputs.result.path`
  - Required by validation.
  - Ignored by `prepare_io_paths_for_runtime`, which still hardcodes `/agentlab/out/result.json`.

- `trial_runtime.outputs.patch.mode: file` and `outputs.patch.path`
  - Validated.
  - Patch extraction/reporting still assumes the existing candidate patch/workspace-diff path.

- `trial_runtime.task.workspace.image.rewrites`
  - Package compile reads this location.
  - The trial runtime schema currently models `image` as only `{ from }`, so parser validation rejects manifests that include `rewrites`.

- `trial_runtime.task.files`
  - Schema-only today.
  - No runtime transport or materialization exists yet.

## Honest Support Matrix For Now

- `task_runtime + writable_workspace/container_image`
- `host + input_only + grader none`
- Graders that require execution only when the trial has a task container, except host graders after task-runtime agent execution

Anything outside that matrix should either be implemented next or rejected loudly before execution.
