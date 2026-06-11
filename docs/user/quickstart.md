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

## 2. Create Your Own Starter

For a new agent, start from the client shape rather than from raw YAML:

```bash
bucephalus init my-eval --client cli --command 'python3 agent.py --input {{input}} --output {{output}}'
```

The `--command` value is *your* agent's invocation, not something `init`
provides. The example above assumes you already have an `agent.py`; substitute
the command that actually runs your agent, where `{{input}}` and `{{output}}`
expand to Buc's trial input and result paths.

`init` writes `experiment.yaml`, `cases.jsonl`, and `agent/buc_agent.py`. The
adapter is the seam where Buc becomes the client for your agent: it reads Buc's
trial input, invokes your CLI/API/SDK shape, and writes Buc's result JSON. If
your command does not write the result file, the adapter falls back to a
scaffold result that reports success — so a green smoke test does not by itself
prove your agent ran. Confirm `bucephalus views <run_id> observability` shows
valid result files before trusting a run. After that, the workflow is the same:

```bash
bucephalus dev my-eval
bucephalus run my-eval/experiment.yaml
```

The cookbook path below is a ready-made version of the same workflow.

## 3. Inspect The Cookbook Inputs

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
from the declared `from: result...` references. The agent reads
`BUCEPHALUS_TRIAL_INPUT_PATH` and writes JSON to `BUCEPHALUS_RESULT_PATH`.

## 4. Validate The Experiment Locally

```bash
bucephalus dev cookbook/agent-eval
```

`dev` is the local happy path for new experiments. It finds `experiment.yaml`,
builds a sealed package in Bucephalus-managed storage, runs package checks,
runs dynamic preflight, and executes a smoke test over the first case for each
variant. If this command fails, fix the reported check before attempting a full
run.

The command prints the package directory, package check report, smoke run id,
and smoke run directory. Pass the YAML file directly when you are not running
from a recipe directory:

```bash
bucephalus dev cookbook/agent-eval/experiment.yaml
```

When you want diagnostics without executing even a smoke trial, use:

```bash
bucephalus doctor cookbook/agent-eval
```

## 5. Run The Experiment

```bash
bucephalus run cookbook/agent-eval/experiment.yaml
```

When `run` receives YAML, it builds a sealed package automatically. If the
package has not been smoke-tested, `run` performs the smoke test first and then
launches the full experiment with full artifact materialization. You can still
run an already-built package:

```bash
bucephalus run <package_dir>
```

Advanced package-level commands remain available when you need to inspect the
internal steps explicitly:

```bash
bucephalus build cookbook/agent-eval/experiment.yaml --out <package_dir> --json
bucephalus check-package <package_dir> --json
bucephalus preflight <package_dir> --json
bucephalus run <package_dir> --smoke-test --materialize full --json
```

The JSON response includes a `run.run_id` and `run.run_dir`.

## 6. Inspect Results

Replace `<run_id>` with the run id from the previous command:

```bash
bucephalus views <run_id>
bucephalus query <run_id> "SELECT * FROM trials LIMIT 20"
```

You can also pass the run directory:

```bash
bucephalus views <run_dir>
```

## Advanced One Command Variant

`build-run` remains available for scripts that already use the explicit package
flow:

```bash
bucephalus build-run cookbook/agent-eval/experiment.yaml --out <package_dir> --smoke-test --materialize full --json
bucephalus build-run cookbook/agent-eval/experiment.yaml --out <package_dir> --materialize full --json
```

For new local development, prefer `bucephalus dev` followed by
`bucephalus run experiment.yaml`.

For more starter shapes, see the root [cookbook](../../cookbook/README.md).
