# Task Rows And Benchmarks

Task rows define what each trial is about and where the task sandbox comes from.

The current runner consumes JSONL: one `task_row_v1` object per line.

## Minimal Task Row

```json
{
  "schema_version": "task_row_v1",
  "id": "TASK001",
  "image": "python:3.11-slim",
  "workdir": "/workspace/task",
  "time_limit_ms": 600000,
  "task": {
    "id": "TASK001",
    "input": {
      "prompt": "Fix the failing test without breaking existing behavior."
    }
  },
  "materialization": {
    "kind": "task_image"
  }
}
```

## Field Ownership

| Field | Owner | Purpose |
| --- | --- | --- |
| `schema_version` | Runner contract | Must be `task_row_v1`. |
| `id` | Benchmark | Stable task id. |
| `image` | Benchmark | Task sandbox image. |
| `workdir` | Benchmark | Working directory inside task sandbox. |
| `time_limit_ms` | Benchmark or policy | Task-specific timeout. |
| `task` | Benchmark | Payload passed through to agent and grader. |
| `materialization` | Benchmark | How task workspace is prepared. |

Everything inside `task` is benchmark-specific. AgentLab preserves it and passes it to the agent and grader.

## Real Benchmark Example

The demo task rows are in `demos/swebench_mini_tasks.jsonl`. Each row represents a small SWE-bench-style issue:

```json
{
  "schema_version": "task_row_v1",
  "id": "swebench_astropy_13398",
  "image": "node:20-alpine",
  "workdir": "/workspace",
  "task": {
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
  "materialization": {
    "kind": "task_image"
  }
}
```

The sample agent reads the prompt and predicts difficulty. A real coding-agent benchmark would instead provide a task image with repository state, tests, and grading logic.

## Common Task Row Mistakes

- The task image cannot be pulled or is not present locally.
- The `workdir` does not exist in the image.
- The task image lacks the tools the agent or grader command expects.
- The task payload does not include the fields your agent reads.
- The task payload does not include the fields your grader reads.
- Hidden grader assets are accidentally exposed to the agent.

