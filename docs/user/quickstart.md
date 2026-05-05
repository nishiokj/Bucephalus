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
cargo build --manifest-path rust/Cargo.toml -p lab-cli --release
LAB="$(pwd)/rust/target/release/lab-cli"
```

During repo development you can also use:

```bash
scripts/lab-cli-fresh.sh --help
```

That wrapper rebuilds `lab-cli` when the Rust sources changed.

## 2. Inspect The Demo Inputs

The demo lives in `demos/`:

```bash
ls demos
```

Important files:

| File | Purpose |
| --- | --- |
| `demos/experiment.yaml` | Experiment config: variants, runtime, benchmark, metrics, policy. |
| `demos/swebench_mini_tasks.jsonl` | Four benchmark-style task rows. |
| `demos/agentlab_demo_harness.js` | Agent runtime app. |
| `demos/agentlab_demo_grader.js` | Grader that writes `trial_conclusion_v1`. |

## 3. Build A Sealed Package

```bash
"$LAB" build demos/experiment.yaml --out .lab/builds/demo --json
```

The build stage resolves the experiment and seals files into `.lab/builds/demo`.

## 4. Describe The Package

```bash
"$LAB" describe .lab/builds/demo --json
```

This should show task count, variant count, total trials, agent runtime command, image, and network policy.

## 5. Preflight The Package

```bash
"$LAB" preflight .lab/builds/demo --json
```

Preflight checks the package, runtime image, task images, grader reachability, required env bindings, and contract smoke paths before the full run.

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

For new agent apps, prefer the staged flow first: build, describe, preflight, run.

## Verify This Doc Path

From the repo root:

```bash
scripts/verify-docs-golden-path.sh
```

The script runs build, describe, and preflight against the demo. It fails if preflight fails.

To execute the full trial run too:

```bash
RUN_FULL=1 scripts/verify-docs-golden-path.sh
```

If you only want to test docs wiring on a machine without Docker running:

```bash
ALLOW_PREFLIGHT_FAILURE=1 scripts/verify-docs-golden-path.sh
```

