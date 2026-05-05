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
| Grader command | Yes for benchmark runs | `["python3", "grader.py"]` |
| Runtime env/secrets | If your agent needs them | `--env OPENAI_API_KEY=...` |
| Mapper | Only if grader raw output is not already `trial_conclusion_v1` | `benchmark.grader.conclusion.mode: mapper` |

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
    command: ["python3", "grader.py"]
    conclusion:
      mode: direct

design:
  replications: 1
  max_concurrency: 1

policy:
  timeout_ms: 600000
  task_sandbox:
    network: full
```

## Agent Runtime Responsibilities

Your agent app must:

1. Start from `runtime.agent_runtime.command`.
2. Read trial input from `AGENTLAB_TRIAL_INPUT_PATH`.
3. Do the task inside the task sandbox workdir.
4. Write valid `trial_output_v1` JSON to `AGENTLAB_RESULT_PATH`.
5. Exit when finished.

Optional but recommended:

- write hook events if using `integration_level: cli_events`
- write artifacts and list them in `trial_output_v1.artifacts`
- produce clear stdout/stderr for debugging

## Grader Responsibilities

Your grader must:

1. Start from `benchmark.grader.command`.
2. Read `AGENTLAB_GRADER_INPUT_PATH`.
3. Write a valid `trial_conclusion_v1` to `AGENTLAB_MAPPED_GRADER_OUTPUT_PATH` when `conclusion.mode: direct`.

If your grader produces a raw native format first, use mapper mode. See [Graders And Mappers](graders-and-mappers.md).

