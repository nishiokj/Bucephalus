# Experiment YAML Reference

Minimal v1 shape:

```yaml
experiment:
  id: smoke_eval
  name: Smoke eval

runtime:
  compute: { backend: local-docker }
  storage: { backend: local-fs, config: { root: .lab/runs/ } }
  traces: { backend: local-stdout }
  secrets:
    - { name: OPENAI_API_KEY, from: env }
  network:
    task_sandbox: none
    agent: full

matrix:
  variants:
    - id: baseline
      baseline: true
      config: { model: gpt-5.5 }
  tasks:
    source: file
    path: tasks.jsonl
  repeats: 1
  seeds: [1]

scheduling:
  max_concurrency: 1
  shuffle_tasks: false
  random_seed: 1
  comparison: none

trial_runtime:
  task:
    interface: writable_workspace
    workspace:
      source: container_image
      image: { from: task_row }
      workdir: { from: task_row }
  agent:
    image: ghcr.io/acme/agent:latest
    command: ["agent", "run", "--model", "$model"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
    outputs:
      result:
        capture: { type: file, path: /agentlab/out/result.json, format: json }
      patch:
        capture: { type: workspace_diff, format: unified_diff }
  execution:
    agent_site: agent_container
  grader:
    strategy: none

metrics: []

policy:
  timeout_ms: 600000
  sanitization_profile: perf_benchmark
  task_sandbox: {}
```

Vocabulary changes from v0:

| v0 | v1 |
| --- | --- |
| `baseline` + `variant_plan` | `matrix.variants` |
| `bindings` | `config` |
| `runtime_overrides` | `overrides` |
| `dataset` | `matrix.tasks` |
| `design.replications` | `matrix.repeats` |
| `design.random_seed` | `scheduling.random_seed` |
| `design.max_concurrency` | `scheduling.max_concurrency` |
| `design.sanitization_profile` | `policy.sanitization_profile` |
| `policy.task_sandbox.network` | `runtime.network.task_sandbox` |
| `trial_runtime.agent.network` | `runtime.network.agent` |
| `trial_runtime.agent.secret_files` | `runtime.secrets` |
| `trial_runtime.agent.artifact` | `trial_runtime.agent.mount` |
| `validity` | `policy.validity` |
| `artifacts` | `extra_outputs` |

The runner rejects v0 paths at validation time.
