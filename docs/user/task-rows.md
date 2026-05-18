# Task Rows And Benchmarks

Task rows define what each trial is about and, when needed, where the task sandbox image and workdir come from.

The current runner consumes JSONL: one `task_row_v2` object per line.

## Minimal Task Row

```json
{
  "schema_version": "task_row_v2",
  "id": "TASK001",
  "task": {
    "id": "TASK001",
    "input": {
      "prompt": "Fix the failing test without breaking existing behavior."
    }
  },
  "runtime": {
    "container_image": {
      "image": "python:3.11-slim",
      "workdir": "/workspace/task"
    }
  },
  "time_limit_ms": 600000
}
```

## Field Ownership

| Field | Owner | Purpose |
| --- | --- | --- |
| `schema_version` | Runner contract | Must be `task_row_v2`. |
| `id` | Benchmark | Stable task id. |
| `task` | Benchmark | Object payload passed through to agent and grader. |
| `runtime.container_image.image` | Benchmark | Task sandbox image, required when `trial_runtime.task.workspace.source: container_image`. |
| `runtime.container_image.workdir` | Benchmark | Absolute working directory inside the task sandbox image. |
| `runtime.container_image.platform` | Benchmark | Optional Docker platform for image pulls and container creation. |
| `time_limit_ms` | Benchmark or policy | Optional task-specific timeout. |

Everything inside `task` is benchmark-specific. AgentLab preserves it and passes it to the agent and grader.

## How Rows Connect To `trial_runtime`

`task_row_v2` no longer has top-level `image`, `workdir`, or `materialization` fields. The experiment decides whether those row values are used:

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

For `task.interface: input_only` and `task.interface: readonly_files`, the row must not declare `runtime.container_image`; the task is data or files, not a task container.

## Task Interfaces

| Interface | Meaning |
| --- | --- |
| `input_only` | The agent receives only the trial input JSON. No task files or task workspace are materialized. |
| `readonly_files` | The agent receives a read-only file set declared by `trial_runtime.task.files`. Patch output must be disabled. |
| `writable_workspace` | The agent receives a writable workspace. Use this for coding tasks, workspace diffs, and task-container execution. |

## Real Benchmark Example

The demo task rows are in `demos/swebench_mini_tasks.jsonl`. Each row represents a small SWE-bench-style issue:

```json
{
  "schema_version": "task_row_v2",
  "id": "swebench_astropy_13398",
  "task": {
    "id": "swebench_astropy_13398",
    "source": "swebench-lite",
    "input": {
      "repo": "astropy/astropy",
      "instance_id": "astropy__astropy-13398",
      "prompt": "Astropy issue: ..."
    },
    "gold": {
      "difficulty_bin": "hard"
    }
  },
  "runtime": {
    "container_image": {
      "image": "node:20-alpine",
      "workdir": "/workspace"
    }
  },
  "time_limit_ms": 600000
}
```

The sample agent reads the prompt and predicts difficulty. A real coding-agent benchmark would instead provide a task image with repository state, tests, and grading logic.

## Common Task Row Mistakes

- The row still says `schema_version: task_row_v1`.
- The row uses removed top-level `image`, `workdir`, or `materialization` fields.
- `runtime.container_image.workdir` is relative or empty.
- The experiment uses `workspace.source: container_image`, but the row has no `runtime.container_image`.
- The task image cannot be pulled or is not present locally.
- The task image lacks the tools the agent or grader command expects.
- The task payload does not include the fields your agent or grader reads.
- Hidden grader assets are exposed to the agent instead of being revealed only during grading.
