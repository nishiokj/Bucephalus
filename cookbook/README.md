# Bucephalus Cookbook

Copy one of these starter workspaces when you want a known-good `experiment.yaml`
shape before swapping in your own agent, cases, and metrics.

Each recipe is self-contained:

```bash
cd cookbook/agent-eval
bucephalus build experiment.yaml --out .lab/builds/agent-eval --json
bucephalus check-package .lab/builds/agent-eval --json
bucephalus run .lab/builds/agent-eval --smoke-test --materialize full --json
```

Use the staged flow first for a new experiment: `build`, `check-package`,
`preflight`, `run --smoke-test`, then full `run`.

## Recipes

| Recipe | Start here when | What it demonstrates |
| --- | --- | --- |
| `agent-eval` | You want the smallest no-key agent evaluation. | One variant, container-backed cases, agent response metrics. |
| `ab-test` | You want to compare a baseline and treatment. | Paired variants, repeats, primary metric comparison. |
| `parameter-sweep` | You want to try several config values quickly. | Multi-variant sweep with variant config projected into the agent command. |
| `swebench-lite-codex` | You want a recognizable real benchmark with a real coding agent and grader. | SWE-bench Lite `astropy__astropy-12907`, Codex CLI headless execution, workspace diff capture, and real pytest grading. |

The first three recipes use `node:20-alpine` and run without provider
credentials. The SWE-bench recipe is intentionally heavier: it uses a real
SWE-bench task image, `OPENAI_API_KEY`, and a test-based grader.

Every agent reads `BUCEPHALUS_TRIAL_INPUT_PATH` and writes JSON to
`BUCEPHALUS_RESULT_PATH`, which is the same contract your real agent should implement.

The case files use `case_v2` rows and the recipe YAML uses the public
`matrix.cases` and `stages` authoring surface.
