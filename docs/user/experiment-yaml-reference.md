# Experiment YAML Reference

This is the canonical authoring reference for v1 `experiment.yaml`.

## Minimal Shape

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

Only this v1 shape is accepted by package authoring and validation.

## Sidecars

`sidecars` is optional. Each top-level sidecar defines a per-trial service container. A stage attaches a sidecar by listing its id under `trial_runtime.agent.sidecars` or `trial_runtime.grader.sidecars`.

```yaml
sidecars:
  service_id:
    image: ghcr.io/acme/service:latest
    lifecycle: per-trial
    command: ["service", "--port", "8080"] # optional
    workdir: /srv/service                  # optional
    env: { LOG_LEVEL: info }               # optional, for the service
    expose: { SERVICE_URL: "http://service_id:8080" } # optional, for attached stages
```

Ids use portable DNS-label syntax because the id is also the runtime hostname alias: lowercase letters, numbers, and `-`, starting and ending with a letter or number. Only `lifecycle: per-trial` is supported. Local Docker supports sidecars; Modal currently rejects them.
