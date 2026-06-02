# Quickstart: Run A Cookbook Experiment

This path starts from a fresh clone and runs the no-key `agent-eval` cookbook
recipe.

## Prerequisites

- `bucephalus` installed; see the README for the one-command installer
- Docker or OrbStack running
- ability to pull or use the `node:20-alpine` image

No API keys are required for the demo. Real agents often need `--env` or `--env-file`; see [Environment And Secrets](env-and-secrets.md).

## 1. Check The CLI

```bash
bucephalus --help
```

## 2. Inspect The Cookbook Inputs

The starter recipe lives in `cookbook/agent-eval/`:

```bash
ls cookbook/agent-eval
```

Important files:

| File | Purpose |
| --- | --- |
| `cookbook/agent-eval/experiment.yaml` | Experiment config: variants, stages, metrics, and policy. |
| `cookbook/agent-eval/cases.jsonl` | Two `case_v2` rows with prompts and lightweight workspace images. |
| `cookbook/agent-eval/agent/run.js` | Tiny agent runtime app that reads trial input and writes the result JSON. |

The recipe uses `stages.grader.strategy: none`, so the metric rows come directly
from the declared `agent_response` pointers. The agent reads
`BUCEPHALUS_TRIAL_INPUT_PATH` and writes JSON to `BUCEPHALUS_RESULT_PATH`.

## 3. Build A Sealed Package

```bash
bucephalus build cookbook/agent-eval/experiment.yaml --out .lab/builds/agent-eval --json
```

The build stage resolves the experiment, seals files into `.lab/builds/agent-eval`,
computes the package digest, registers the bundle validation state, and writes
`.lab/builds/agent-eval/package_checks.json`.

`experiment.yaml` is a build input. `bucephalus run` takes a sealed package directory
or its `manifest.json`, not raw YAML. Use `bucephalus build-run` when you want the CLI
to build from YAML and then run the produced package in one command.

## 4. Check The Package

```bash
bucephalus check-package .lab/builds/agent-eval --json
```

Package checks are static hygiene checks over the sealed package: variant shape,
scheduling, case ids, metric declarations, result capture, event
declarations, and conditional grader wiring. They do not start Docker or access
secrets/providers.

## 5. Preflight The Package

```bash
bucephalus preflight .lab/builds/agent-eval --json
```

Preflight checks dynamic launch readiness: runtime image, case images, grader
reachability, required env bindings, resources, and contract smoke paths before
the full run.

If preflight fails, fix that first. Do not skip it for a new experiment.

## 6. Run The Experiment

Run a smoke test before the full run:

```bash
bucephalus run .lab/builds/agent-eval --smoke-test --materialize full --json
```

A smoke test is a real end-to-end runner execution over the first case for each
variant. It still runs preflight, prepares the case environment, executes the
agent, runs grading, and writes normal run artifacts. If it completes, the
package digest is marked smoke-tested in the account database.

Full runs are gated by this validation state. If the package digest has not
passed a smoke test, an interactive terminal shows a loud warning and offers:

1. Run a smoke test to validate
2. Skip smoke tests and run dangerously
3. Cancel

For non-interactive or `--json` invocations, choose explicitly:

```bash
bucephalus run .lab/builds/agent-eval --smoke-test --materialize full --json
bucephalus run .lab/builds/agent-eval --run-dangerously --materialize full --json
```

After the smoke test passes, run the full experiment:

```bash
bucephalus run .lab/builds/agent-eval --materialize full --json
```

The JSON response includes a `run.run_id` and `run.run_dir`.

## 7. Inspect Results

Replace `<run_id>` with the run id from the previous command:

```bash
bucephalus views <run_id>
bucephalus query <run_id> "SELECT * FROM trials LIMIT 20"
```

You can also pass the run directory:

```bash
bucephalus views .lab/runs/<run_id>
```

## One Command Variant

After you understand the stages, this runs build and execution together:

```bash
bucephalus build-run cookbook/agent-eval/experiment.yaml --out .lab/builds/agent-eval --smoke-test --materialize full --json
bucephalus build-run cookbook/agent-eval/experiment.yaml --out .lab/builds/agent-eval --materialize full --json
```

For new agent apps, prefer the staged flow first: build, preflight, smoke test,
full run, inspect.

For more starter shapes, see the root [cookbook](../../cookbook/README.md).
