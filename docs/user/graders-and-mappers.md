# Graders And Mappers

The grader turns an agent result into a benchmark conclusion.

AgentLab separates agent execution from grading so that infrastructure failures, agent contract failures, and scientific verdicts are not collapsed into one status.

## Direct Grader

Use direct mode when the grader can write `trial_conclusion_v1` itself.

```yaml
benchmark:
  grader:
    strategy: in_task_image
    command: ["python3", "grader.py"]
    conclusion:
      mode: direct
```

The grader receives:

| Variable | Purpose |
| --- | --- |
| `AGENTLAB_GRADER_INPUT_PATH` | JSON input with ids, task, agent result, paths, and execution metadata. |
| `AGENTLAB_MAPPED_GRADER_OUTPUT_PATH` | Write valid `trial_conclusion_v1` here. |
| `AGENTLAB_RAW_GRADER_OUTPUT_PATH` | Optional raw grader output path. |

Minimal direct grader:

```python
import json
import os

grader_input = json.load(open(os.environ["AGENTLAB_GRADER_INPUT_PATH"]))
result = json.load(open(grader_input["paths"]["result_path"]))
score = float(result.get("metrics", {}).get("resolved", 0.0))

conclusion = {
    "schema_version": "trial_conclusion_v1",
    "reported_outcome": "success" if score >= 1.0 else "failure",
    "primary_metric": {"name": "resolved", "value": score},
    "payload": {"resolved": score, "task_id": grader_input["ids"]["task_id"]},
    "grader": {"name": "my_grader", "strategy": "in_task_image"},
}

json.dump(conclusion, open(os.environ["AGENTLAB_MAPPED_GRADER_OUTPUT_PATH"], "w"))
```

## Mapper Mode

Use mapper mode when your grader writes a raw native output and a second command normalizes it into `trial_conclusion_v1`.

```yaml
benchmark:
  grader:
    strategy: in_task_image
    command: ["python3", "run_native_grader.py"]
    conclusion:
      mode: mapper
      mapper: "./mappers/normalize.py"
```

The mapper reads `AGENTLAB_RAW_GRADER_OUTPUT_PATH` and writes `AGENTLAB_MAPPED_GRADER_OUTPUT_PATH`.

Mapper mode is useful when:

- you are integrating an existing benchmark
- the native grader output is not AgentLab-shaped
- you want to preserve raw output and normalize separately

## Failure Semantics

| Situation | Meaning |
| --- | --- |
| Agent exits 0 but result is missing | Agent contract failure. |
| Agent exits non-zero but writes valid result | Grader still runs; exit code is evidence. |
| Grader exits non-zero but writes valid conclusion | Valid conclusion can still be recorded. |
| Grader exits 0 but mapped conclusion is missing or invalid | Grading failed. |
| Mapper fails or writes invalid conclusion | Grading failed; raw output is preserved when available. |

The runner should never fabricate a scientific verdict when the mapped conclusion is missing or invalid.

