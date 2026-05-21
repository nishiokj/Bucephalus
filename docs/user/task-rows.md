# Cases And Benchmarks

Cases define what each trial is about and, when needed, where the case workspace image and workdir come from. A trial runs one variant against one case for one repeat.

The preferred JSONL row shape is one `case_v1` object per line. The older `task_row_v2` shape remains accepted for existing benchmark suites.

## Minimal Case Row

```json
{
  "schema_version": "case_v1",
  "id": "CASE001",
  "inputs": {
    "prompt": "Fix the failing test without breaking existing behavior."
  },
  "resources": {
    "workspace": {
      "type": "container_image",
      "image": "python:3.11-slim",
      "workdir": "/workspace/case"
    }
  },
  "limits": {
    "timeout_ms": 600000
  }
}
```

## Field Ownership

| Field | Owner | Purpose |
| --- | --- | --- |
| `schema_version` | Runner contract | Must be `case_v1`. |
| `id` | Benchmark | Stable case id. |
| `inputs` | Benchmark | Object payload passed through the Transport Envelope to stages. |
| `resources.workspace.image` | Benchmark | Case workspace image, required when `stages.case.workspace.source: container_image`. |
| `resources.workspace.workdir` | Benchmark | Absolute working directory inside the case workspace image. |
| `resources.workspace.platform` | Benchmark | Optional Docker platform for image pulls and container creation. |
| `limits.timeout_ms` | Benchmark or policy | Optional case-specific timeout. |

Everything inside `inputs` and `metadata` is benchmark-specific. AgentLab preserves it and passes it through the runner-owned Transport Envelope.

## How Cases Connect To `stages`

`case_v1` describes the case. The experiment decides whether its workspace resource is used:

```yaml
stages:
  case:
    interface: writable_workspace
    workspace:
      source: container_image
      image: { from: case_row }
      workdir: { from: case_row }
```

For `case.interface: input_only` and `case.interface: readonly_files`, the row must not declare a container-image workspace; the case is data or files, not a container workspace.

## Case Interfaces

| Interface | Meaning |
| --- | --- |
| `input_only` | The agent receives only the trial input JSON. No case files or workspace are materialized. |
| `readonly_files` | The agent receives a read-only file set declared by `stages.case.files`. Patch output must be disabled. |
| `writable_workspace` | The agent receives a writable workspace. Use this for coding cases, workspace diffs, and container execution. |

## Existing Benchmark Rows

Existing suites may still emit `task_row_v2` rows with `task` payloads and `runtime.container_image`. Those rows are normalized into the same internal case boundary during package compilation.

## Common Case Row Mistakes

- The row still says `schema_version: task_row_v1`.
- The row uses removed top-level `image`, `workdir`, or `materialization` fields.
- `resources.workspace.workdir` is relative or empty.
- The experiment uses `workspace.source: container_image`, but the case has no `resources.workspace` container image.
- The case image cannot be pulled or is not present locally.
- The case image lacks the tools the agent or grader command expects.
- The case payload does not include the fields your agent or grader reads.
- Hidden grader assets are exposed to the agent instead of being revealed only during grading.
