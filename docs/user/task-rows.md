# Cases And Benchmarks

Cases define what each trial is about and, when needed, where the case workspace image and workdir come from. A trial runs one variant against one case for one repeat.

The preferred JSONL row shape is one `case_v2` object per line. `case_v1` remains accepted during migration. The older `task_row_v2` shape is compatibility-only for existing benchmark suites.

AgentLab does not ingest arbitrary benchmark-native rows. Benchmark acquisition or authoring code must map each native benchmark example into an AgentLab case row before package build. Benchmark-specific fields belong under `inputs` or `metadata`; runner-owned fields describe how the case becomes a workspace/runtime boundary.

## Case v2 Row

`case_v2` separates benchmark payload from runtime and workspace materialization:

```json
{
  "schema_version": "case_v2",
  "id": "CASE001",
  "inputs": {
    "prompt": "Fix the failing test without breaking existing behavior."
  },
  "resources": {
    "workspace": {
      "source": "container_image",
      "mode": "patch",
      "image": "python:3.11-slim",
      "workdir": "/workspace/task"
    }
  },
  "materialization": [],
  "limits": {
    "timeout_ms": 600000
  }
}
```

`resources.workspace.source` currently supports `container_image`, `empty`, `dataset_pack`, and `git_checkout` as declarations. The current local executor runs the existing container-image path. `materialization` currently supports `stage: case` plus `operation: command`, executed in the case sandbox before the agent starts; other materialization stages and operations are rejected until their lowering is implemented.

## Minimal Case Row

`case_v1` is still loadable, but it folds the runtime image and workspace into `resources.workspace.type: container_image`:

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
| `schema_version` | Runner contract | Prefer `case_v2`; `case_v1` is migration compatibility. |
| `id` | Benchmark | Stable case id. |
| `inputs` | Benchmark | Object payload passed through the Transport Envelope to stages. |
| `resources.workspace.image` | Benchmark | Case workspace image, required when `stages.case.workspace.source: container_image`. |
| `resources.workspace.workdir` | Benchmark | Absolute working directory inside the case workspace image. |
| `resources.workspace.platform` | Benchmark | Optional Docker platform for image pulls and container creation. |
| `limits.timeout_ms` | Benchmark or policy | Optional case-specific timeout. |

Everything inside `inputs` and `metadata` is benchmark-specific. AgentLab preserves it and passes it through the runner-owned Transport Envelope.

## How Cases Connect To `stages`

`case_v2` describes the case resources. The experiment decides whether the declared workspace is used:

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

Existing suites may still emit `task_row_v2` rows with `task` payloads and `runtime.container_image`. Those rows are normalized into the same internal case boundary during package compilation, but new benchmark integrations should emit `case_v2`.

## Common Case Row Mistakes

- The row still says `schema_version: task_row_v1`.
- The row uses removed top-level `image` or `workdir` fields instead of `resources.workspace`.
- The row uses ad hoc setup fields instead of explicit `case_v2.materialization` steps.
- `resources.workspace.workdir` is relative or empty.
- The experiment uses `workspace.source: container_image`, but the case has no `resources.workspace` container image.
- The case image cannot be pulled or is not present locally.
- The case image lacks the tools the agent or grader command expects.
- The case payload does not include the fields your agent or grader reads.
- Hidden grader assets are exposed to the agent instead of being revealed only during grading.
