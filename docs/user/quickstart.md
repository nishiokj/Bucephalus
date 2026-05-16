# Quickstart: Run The Benchmark Demo

This path starts from a fresh clone and runs a complete benchmark-style experiment.

## Prerequisites

- Rust toolchain with `cargo`
- Docker or OrbStack running
- ability to pull or use the `node:20-alpine` image

No API keys are required for the demo. Real agents often need `--env` or `--env-file`; see [Environment And Secrets](env-and-secrets.md).

## 1. Build The CLI

From the repo root:

```bash
cargo build --manifest-path rust/Cargo.toml --bin lab --release
LAB="$(pwd)/rust/target/release/lab"
```

During repo development you can also use:

```bash
scripts/lab-fresh.sh --help
```

That wrapper rebuilds `lab` when the Rust sources changed.

## 2. Inspect The Demo Inputs

The demo lives in `demos/`:

```bash
ls demos
```

Important files:

| File | Purpose |
| --- | --- |
| `demos/experiment.yaml` | Experiment config: variants, `trial_runtime`, metrics, and policy. |
| `demos/swebench_mini_tasks.jsonl` | Four benchmark-style task rows. |
| `demos/agentlab_demo_harness.js` | Agent runtime app. |
| `demos/agentlab_demo_grader.js` | Grader that writes a native JSON report captured by `trial_runtime.grader.outputs`. |

The demo uses `trial_runtime.grader.strategy: in_task_runtime`, so its grader file is package-owned and runs inside the task sandbox after the agent step. For new benchmark authoring, prefer declared `agent.outputs`, `grader.inputs`, `grader.outputs`, and metric extraction from those outputs. Host graders are only for runner-owned capabilities such as official SWE-bench evaluation.

## 3. Build A Sealed Package

```bash
"$LAB" build demos/experiment.yaml --out .lab/builds/demo --json
```

The build stage resolves the experiment, seals files into `.lab/builds/demo`,
and writes `.lab/builds/demo/package_checks.json`.

## 4. Check The Package

```bash
"$LAB" check-package .lab/builds/demo --json
```

Package checks are static hygiene checks over the sealed package: variant shape,
scheduling, task row ids, metric declarations, result capture, event
declarations, and conditional grader wiring. They do not start Docker or access
secrets/providers.

## 5. Preflight The Package

```bash
"$LAB" preflight .lab/builds/demo --json
```

Preflight checks dynamic launch readiness: runtime image, task images, grader
reachability, required env bindings, resources, and contract smoke paths before
the full run.

If preflight fails, fix that first. Do not skip it for a new experiment.

## 6. Run The Experiment

```bash
"$LAB" run .lab/builds/demo --materialize full --json
```

The JSON response includes a `run.run_id` and `run.run_dir`.

## 7. Inspect Results

Replace `<run_id>` with the run id from the previous command:

```bash
"$LAB" views <run_id>
"$LAB" variants <run_id>
"$LAB" query <run_id> "SELECT * FROM trials LIMIT 20"
```

You can also pass the run directory:

```bash
"$LAB" views .lab/runs/<run_id>
```

## One Command Variant

After you understand the stages, this runs build and execution together:

```bash
"$LAB" build-run demos/experiment.yaml --out .lab/builds/demo --materialize full --json
```

For new agent apps, prefer the staged flow first: build, preflight, run, inspect.

## Verify This Doc Path

From the repo root:

```bash
scripts/verify-docs-golden-path.sh
```

The script runs build and preflight against the demo. It fails if preflight fails.

To execute the full trial run too:

```bash
RUN_FULL=1 scripts/verify-docs-golden-path.sh
```

If you only want to test docs wiring on a machine without Docker running:

```bash
ALLOW_PREFLIGHT_FAILURE=1 scripts/verify-docs-golden-path.sh
```
