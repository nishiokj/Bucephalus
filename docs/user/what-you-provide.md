# What You Must Provide

AgentLab is not a magic wrapper around arbitrary apps. A successful experiment needs a few explicit pieces from you.

## Required Pieces

| Piece | Required | Example |
| --- | --- | --- |
| Experiment YAML | Yes | `experiment.yaml` |
| Task rows | Yes | `tasks.jsonl` |
| Task sandbox image | Yes | `node:20-alpine`, `python:3.11`, benchmark image |
| Agent runtime image | Yes | image with your agent dependencies |
| Agent artifact | Yes | repo dir, tarball, or packaged runtime files |
| Agent command | Yes | `["python", "-m", "my_agent.run"]` |
| Grader declaration | Yes for benchmark runs | `benchmark.grader.strategy` plus the strategy-specific fields below |
| Metric declarations | If you want custom metrics | `metrics[].id` plus `metrics[].source.pointer` |
| Runtime env/secrets | If your agent needs them | `--env OPENAI_API_KEY=...` |
| Mapper | Only if grader raw output is not already `trial_conclusion_v1` | `benchmark.grader.conclusion.mode: mapper` |

Schema files live in `schemas/`. The agent result contract is `schemas/agent_result_v1.jsonschema`, task rows use `schemas/task_row_v1.jsonschema`, and grader conclusions use `schemas/trial_conclusion_v1.jsonschema`.

## Minimal Experiment Shape

```yaml
experiment:
  id: my_eval
  name: My Agent Evaluation
  workload_type: agent_runtime

dataset:
  suite_id: my_tasks
  provider: local_jsonl
  path: tasks.jsonl
  split_id: eval

baseline:
  variant_id: control
  bindings:
    model: gpt-5.3-codex

variant_plan: []

runtime:
  agent_runtime:
    artifact: ./agent
    image: ghcr.io/my-org/my-agent-runtime:latest
    command: ["python", "-m", "my_agent.run", "--model", "$model"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
    network: full

benchmark:
  grader:
    strategy: in_task_image
    command: ["python3", "./grader.py"]
    conclusion:
      mode: direct

metrics:
  - id: resolved
    source:
      type: agent_result
      pointer: /metrics/resolved
    direction: maximize
    primary: true

design:
  replications: 1
  max_concurrency: 1

policy:
  timeout_ms: 600000
  task_sandbox:
    network: full
```

`dataset.provider` is optional and currently has one supported value: `local_jsonl`. If you include it, it must be exactly `local_jsonl`.

Metric declarations are the canonical analytics contract. The runner does not persist arbitrary fields from `agent_result_v1`; declare each custom metric you want to query. See [Metrics](metrics.md).

## Agent Runtime Responsibilities

Your agent app must:

1. Start from `runtime.agent_runtime.command`.
2. Read trial input from `AGENTLAB_TRIAL_INPUT_PATH`.
3. Do the task inside the task sandbox workdir.
4. Write valid `agent_result_v1` JSON to `AGENTLAB_RESULT_PATH`.
5. Exit when finished.

Optional but recommended:

- write hook events if using `integration_level: cli_events`
- write runtime evidence, such as context snapshots or debug bundles, under declared `runtime.agent_runtime.output_mounts`
- write artifacts and list them in `agent_result_v1.artifacts`
- produce clear stdout/stderr for debugging

## Grader Responsibilities

Your grader must:

1. Start from `benchmark.grader.command`.
2. Read `AGENTLAB_GRADER_INPUT_PATH`.
3. Write a valid `trial_conclusion_v1` to `AGENTLAB_MAPPED_GRADER_OUTPUT_PATH` when `conclusion.mode: direct`.

Pick one grader runtime strategy and declare only the fields for that strategy:

| Strategy | Required declaration | File/path rule |
| --- | --- | --- |
| `in_task_image` | `command` | Relative command file paths are sealed into the package and mounted under the task workdir support directory. |
| `injected` | `command`, `injected.bundle`, `injected.copy_dest` | The bundle is sealed into the package, copied into the task sandbox, then executed there. |
| `separate` | `command`, `separate.image`, `separate.workdir` | The command runs in the separate grader image. |
| `host` | `command`, `host.capability` | The command must reference a runner-owned host capability, not package-local files or arbitrary host paths. |

Host graders are intentionally narrow. This is valid:

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

This is not valid for `strategy: host`:

```yaml
benchmark:
  grader:
    strategy: host
    command: ["python3", "/Users/me/project/grader.py"]
```

If your grader produces a raw native format first, use mapper mode. See [Graders And Mappers](graders-and-mappers.md).
