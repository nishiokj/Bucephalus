# Experiment YAML Reference

This is the canonical authoring reference for v1 `experiment.yaml`. Authoring uses `cases`, `stages`, `ephemerals`, and `externals`; packaging normalizes those names to the current internal runtime shape.

## Minimal Shape

```yaml
experiment:
  id: smoke_eval
  name: Smoke eval
  mode: answer

runtime:
  compute: { backend: local-docker }
  storage: { backend: local-fs }
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
  cases:
    source: file
    path: cases.jsonl
  repeats: 1
  seeds: [1]

scheduling:
  max_concurrency: 1
  shuffle_tasks: false
  random_seed: 1
  comparison: none

stages:
  case:
    interface: writable_workspace
    workspace:
      source: container_image
      image: { from: case_row }
      workdir: { from: case_row }
  agent:
    image: ghcr.io/acme/agent:latest
    command: ["agent", "run", "--model", "$model"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
    outputs:
      result:
        capture: { type: file, path: /bucephalus/out/result.json, format: json }
      patch:
        capture: { type: workspace_diff, format: unified_diff }
  execution:
    agent_site: agent_container
  grader:
    strategy: none

metrics: []

policy:
  timeout_ms: 600000
  task_sandbox: {}
```

Only this v1 shape is accepted by package authoring and validation.

`experiment.mode` is the evaluation intent. Current recipes mostly use `answer`
or patch-like grader wiring; future authoring shortcuts will use this field to
derive default outputs, metrics, and grading contracts.

`stages.agent.protocol` is how Bucephalus invokes and observes the agent. It
defaults to `command`, which launches `stages.agent.command`, injects
runner-owned input/output paths, and can ingest command-agent event streams.
Future values such as `http` and `acp` will make Buc act as that client while
preserving the same downstream result, trace, and grading model.

`traces.source` is separate from invocation. Omit `traces` or set
`source: none` for runner lifecycle only. Set `source: protocol` when you want
Buc to use the trace channel implied by the agent protocol. Today, for
`protocol: command`, that creates a runner-owned JSONL event path exposed as
`BUCEPHALUS_TRAJECTORY_PATH`. Your agent must append JSONL there; Buc does not
guess or scrape arbitrary trace files.

`traces.retain` accepts `never`, `on_failure`, or `always`. In the current
command-agent implementation, `always` retains the raw JSONL file; `never` and
`on_failure` both ingest events without retaining the raw file yet.

`runtime.compute.backend` selects where trials execute. Supported values are `local-docker` and `modal`; CLI `--executor` can override this for a run without changing the package.

## Ephemerals

`ephemerals` is optional. Each top-level ephemeral defines a per-trial resource whose lifecycle the runner owns, but which is not a link in the stage chain. A stage attaches an ephemeral by listing its id under `stages.agent.ephemerals` or `stages.grader.ephemerals`.

```yaml
ephemerals:
  service_id:
    image: ghcr.io/acme/service:latest
    lifecycle: per-trial
    command: ["service", "--port", "8080"] # optional
    workdir: /srv/service                  # optional
    env: { LOG_LEVEL: info }               # optional, for the service
    expose: { SERVICE_URL: "http://service_id:8080" } # optional, for attached stages

stages:
  agent:
    ephemerals: [service_id]
```

Ids use portable DNS-label syntax because the id is also the runtime hostname alias: lowercase letters, numbers, and `-`, starting and ending with a letter or number. Only `lifecycle: per-trial` is supported. Local Docker supports ephemerals; Modal currently rejects them.

## Legacy Files

Older suites that still use `matrix.tasks`, `trial_runtime`, or `sidecars` remain loadable during migration. New experiments should use `cases`, `stages`, `ephemerals`, and `externals`.
