# AgentLab

Run controlled evaluations of AI agents. Define an experiment, point it at your agent, get scored results.

## Documentation

Start with the product-facing docs in [`docs/user/`](docs/user/index.md).

Those docs are ordered from first clone to full run and cover what users must provide: agent runtime, task rows, grader, env vars, and troubleshooting. The rest of `docs/` contains design notes, patch specs, audits, and implementation history.

## Quickstart

```bash
# Build the CLI (one time)
cargo build --manifest-path rust/Cargo.toml --bin lab --release
LAB="$(pwd)/rust/target/release/lab"

# Create an experiment workspace
mkdir my-eval && cd my-eval

# Add experiment.yaml and tasks.jsonl, then:
"$LAB" build-run experiment.yaml --out .lab/builds/run1 \
  --env OPENAI_API_KEY=... \
  --materialize full

# See results
"$LAB" views .lab/runs/<run_id>
"$LAB" query .lab/runs/<run_id> "SELECT * FROM trials"
```

Profiles: `agent-eval` (single variant), `ab-test` (A/B comparison), `sweep` (parameter grid), `regression` (tracking over time).

## Experiment Config

`experiment.yaml` is the control plane. This example runs two model variants head-to-head with a custom grader:

```yaml
experiment:
  id: glm5_vs_codex
  name: GLM-5 vs Codex
  workload_type: agent_runtime

baseline:
  variant_id: glm_5
  bindings:
    model_provider: z.ai-coder
    model: glm-5

variant_plan:
  - variant_id: codex
    bindings:
      model_provider: codex
      model: gpt-5.3-codex

dataset:
  suite_id: my_benchmark
  provider: local_jsonl
  path: tasks.jsonl

trial_runtime:
  task:
    interface: writable_workspace
    workspace:
      source: container_image
      image:
        from: task_row
      workdir:
        from: task_row
  agent:
    image: ghcr.io/my-org/agent-image:latest
    command: [my-agent, run, --provider, $model_provider, --model, $model]
    env:
      API_KEY: $API_KEY
    outputs:
      result:
        capture:
          type: file
          path: /agentlab/out/result.json
          format: json
    network: full
  execution:
    agent_site: agent_container
  grader:
    strategy: in_task_runtime
    command: [my-grader, --out, /agentlab/out/grade.json]
    outputs:
      grade:
        capture:
          type: file
          path: /agentlab/out/grade.json
          format: json
          required: true

metrics:
  - id: resolved
    source:
      type: grader_output
      output: grade
      pointer: /resolved
    direction: maximize
    primary: true

design:
  replications: 1
  max_concurrency: 2

policy:
  timeout_ms: 600000
  task_sandbox:
    network: full
```

### Variants

Variants are how you compare different configurations without changing the runtime setup.

- `baseline` is the control variant
- Each entry in `variant_plan` is a treatment variant
- `$NAME` in `command` or `env` resolves from that variant's bindings
- Unresolved bindings fall through to `--env`, `--env-file`, then host environment

### Tasks

`tasks.jsonl` — one JSON object per line, each a `task_row_v2`:

```json
{
  "schema_version": "task_row_v2",
  "id": "TASK001",
  "time_limit_ms": 600000,
  "task": {
    "id": "TASK001",
    "input": {
      "prompt": "Fix the failing test without breaking existing behavior."
    }
  },
  "runtime": {
    "container_image": {
      "image": "ghcr.io/my-org/task-image:latest",
      "workdir": "/workspace/task"
    }
  }
}
```

`runtime.container_image` controls task sandbox execution when the experiment uses `workspace.source: container_image`. Everything inside `task` is benchmark-specific and passed through to your agent and grader.

## Grader

The grader reads a structured input and writes a conclusion. It runs after your agent finishes.

Env vars available to the grader:

| Variable | Purpose |
|----------|---------|
| `AGENTLAB_GRADER_INPUT_PATH` | JSON with trial IDs, agent output, and task context |
| `AGENTLAB_MAPPED_GRADER_OUTPUT_PATH` | Where to write the conclusion |

Declare grader outputs under `trial_runtime.grader.outputs`, then point metrics at those captured outputs. Graders can be any executable available in the selected runtime.

## Agent Runtime Contract

Your agent process runs inside a container with this contract:

**Filesystem:**

| Path | Access | Purpose |
|------|--------|---------|
| cwd (task `workdir`) | read/write | Working directory |
| `/agentlab/in/` | read | Trial input |
| `/agentlab/out/` | write | Agent output |

**Environment variables:**

| Variable | Value |
|----------|-------|
| `AGENTLAB_TRIAL_INPUT_PATH` | Path to trial input JSON |
| `AGENTLAB_RESULT_PATH` | Where to write your result |
| `AGENTLAB_RUN_ID` | Current run identifier |
| `AGENTLAB_TRIAL_ID` | Current trial identifier |
| `AGENTLAB_VARIANT_ID` | Which variant is running |
| `AGENTLAB_TASK_ID` | Which task is running |
| `AGENTLAB_TIMEOUT_MS` | Time limit in milliseconds |

Read the trial input. Do your work. Write a result JSON to the result path.

## Workflow

```
author  -->  build  -->  check-package  -->  preflight  -->  smoke-test  -->  run  -->  inspect
```

| Stage | Command | What it does |
|-------|---------|-------------|
| Author | Edit `experiment.yaml` + `tasks.jsonl` | Define the experiment |
| Build | `lab build experiment.yaml --out .lab/builds/x` | Seal a portable package |
| Check package | `lab check-package .lab/builds/x` | Run static hygiene checks over the sealed package |
| Preflight | `lab preflight .lab/builds/x --env-file .env` | Check dynamic resources before running |
| Smoke test | `lab run .lab/builds/x --smoke-test --env-file .env` | Execute a small end-to-end run and validate the package digest |
| Run | `lab run .lab/builds/x --env-file .env` | Execute all trials |
| Inspect | `lab views <run_id>` | Read results |

Or build and smoke test in one command: `lab build-run experiment.yaml --out .lab/builds/x --smoke-test --env-file .env`

`experiment.yaml` is a build input. `lab run` takes a sealed package directory
or package `manifest.json`; `lab build-run` is the command that accepts YAML and
then runs the built package. Full runs warn or fail fast when the package digest
has not passed smoke validation. Use `--run-dangerously` only when automation is
intentionally skipping that gate.

### Inspect Commands

```bash
"$LAB" runs                                           # list runs
"$LAB" views <run_id>                                 # summary tables
"$LAB" query <run_id> "SELECT * FROM trials LIMIT 20" # SQL over results
```

### Resume a Stopped Run

```bash
"$LAB" continue --run-dir .lab/runs/<run_id> --env-file .env
```

## Reference

**Run package inputs:**

| Field | Purpose |
|-------|---------|
| `design.replications` | Repeat count per task/variant |
| `design.max_concurrency` | Parallel trial limit |
| `policy.timeout_ms` | Per-trial time limit |
| `policy.task_sandbox.network` | `none` or `full` |
| `runtime.agent_runtime.network` | Agent network access |
| `--env KEY=VAL` | Runtime secrets |
| `--env-file .env` | Secrets from file |

**Run artifacts** live under `.lab/runs/<run_id>/`. Durable run facts are written to the account SQLite database, by default `$HOME/.agentlab/agentlab.sqlite` unless `AGENTLAB_DB` or `AGENTLAB_HOME` is set.

| File | Content |
|------|---------|
| `trials/<trial_id>/trial_state.json` | Trial status |
| `trials/<trial_id>/out/result.json` | Agent output |

Custom metrics are declarative. Agent response JSON is not swept into storage automatically; each custom metric must be declared in `experiment.yaml` with a canonical `id` and a `source.pointer`. See [`docs/user/metrics.md`](docs/user/metrics.md).
