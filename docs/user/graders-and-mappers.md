# Graders And Mappers

The grader turns an agent result into a benchmark conclusion.

AgentLab separates agent execution from grading so that infrastructure failures, agent contract failures, and scientific verdicts are not collapsed into one status.

## Direct Grader

Use direct mode when the grader can write `trial_conclusion_v1` itself.

```yaml
benchmark:
  grader:
    strategy: in_task_image
    command: ["python3", "./grader.py"]
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

## Grader Strategies

| Strategy | Where it runs | Use when |
| --- | --- | --- |
| `in_task_image` | In the task sandbox image | The task image already contains the grader dependencies. |
| `injected` | In the task sandbox after copying a sealed grader bundle | The benchmark grader should be sealed into the package and copied into each task workspace. |
| `separate` | In a separate grader container | The grader needs a different image from the task sandbox. |
| `host` | On the runner host | The grader is a runner-owned integration such as official SWE-bench evaluation that must invoke host tooling. |

`separate` uses the same network mode as the trial run. `host` graders receive launch env such as `--env ANTHROPIC_API_KEY=...`, but their contract paths point to host filesystem paths.

## Strategy Declarations

Each strategy has a different packaging boundary. Declare the boundary directly instead of relying on host paths.

### In Task Image

Use this when the task image already has the grader runtime and dependencies.

```yaml
benchmark:
  grader:
    strategy: in_task_image
    command: ["python3", "./grader.py"]
    conclusion:
      mode: direct
```

Relative command file paths, such as `./grader.py`, are package-owned files. The runner seals them into the package and mounts them in the task workdir support directory before grading.

### Injected

Use this when the grader is a sealed bundle that should be copied into the task sandbox for grading.

```yaml
benchmark:
  grader:
    strategy: injected
    command: ["python3", "/opt/grader/run.py"]
    injected:
      bundle: ./grader_bundle.tar.gz
      copy_dest: /opt/grader
    conclusion:
      mode: direct
```

### Separate

Use this when the grader needs a different image from the task sandbox.

```yaml
benchmark:
  grader:
    strategy: separate
    command: ["python3", "/grader/run.py"]
    separate:
      image: ghcr.io/my-org/my-grader:latest
      workdir: /grader
    conclusion:
      mode: direct
```

The separate grader container uses the run's effective network mode.

### Host

Host graders are an explicit runner-runtime boundary. They must name a runner-owned capability and reference that capability in the command; package-local files, task-workdir support paths, and arbitrary absolute host script paths are rejected during packaging or preflight.

```yaml
benchmark:
  grader:
    strategy: host
    host:
      capability: swebench_official
    command:
      - python3
      - __AGENTLAB_RUNNER_BUILTIN_GRADER__/swebench_official/run_official_swebench_eval_from_agentlab.py
      - --grader-input
    conclusion:
      mode: direct
```

Do not use `host` for your own local grader script. Use `in_task_image`, `injected`, or `separate` so the grader is package-owned and portable.

## Mapper Mode

Use mapper mode when your grader writes a raw native output and a second command normalizes it into `trial_conclusion_v1`.

```yaml
benchmark:
  grader:
    strategy: in_task_image
    command: ["python3", "./run_native_grader.py"]
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
