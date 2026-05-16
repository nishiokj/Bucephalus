# Troubleshooting

Start with `lab check-package`, then `lab preflight`.

`check-package` catches static package wiring problems:

```bash
lab check-package .lab/builds/my-package --json
```

`preflight` catches dynamic launch problems:

```bash
lab preflight .lab/builds/my-package --env-file .env --json
```

## Build Fails

Common causes:

- `experiment.yaml` has a removed or misspelled field.
- `design.sanitization_profile: hermetic_functional` is combined with `policy.task_sandbox.network: full` or an explicit non-`none` `trial_runtime.agent.network`.
- a declared runtime output cannot be captured or materialized into a declared grader input.
- `tasks.jsonl` is malformed JSONL.
- `trial_runtime.agent.artifact.source` does not exist, or `artifact.mount.path/read_only` is missing.
- `trial_runtime.grader.command` references a file that does not match the selected grader strategy.
- `strategy: host` is pointing at a local or absolute script path instead of a declared `trial_runtime.grader.host.capability`.
- A command or env binding references `$NAME`, but no variant binding or runtime env provides it.

What to inspect:

```bash
lab build experiment.yaml --out .lab/builds/debug --json
```

The build response includes `package_checks_path`. Inspect it directly or run:

```bash
lab check-package .lab/builds/debug --json
```

## Package Checks Fail

Common causes:

- `comparison: paired` is declared with only one resolved variant.
- variant ids or task ids are duplicated.
- no primary metric is declared, or multiple metrics are marked primary.
- a no-grader experiment declares a `grader_output` metric.
- agent result output capture is missing a path.
- declared hidden grader paths overlap agent output mounts.

Fix package-check failures before running dynamic preflight.

## Preflight Fails

Common causes:

- Docker or OrbStack is not running.
- Required image cannot be pulled.
- Agent runtime image lacks the command executable.
- Task image lacks the grader runtime, such as `python3` or `node`.
- Required runtime env var is missing.
- grader output capture fails.
- Host grader capability is missing or unknown.

Fix preflight before running the full experiment.

## Run Starts But Trials Fail

Inspect logs:

```bash
ls .lab/runs/<run_id>/trials/<trial_id>
cat .lab/runs/<run_id>/trials/<trial_id>/agent_stderr.log
cat .lab/runs/<run_id>/trials/<trial_id>/grader_stderr.log
```

Inspect outputs:

```bash
cat .lab/runs/<run_id>/trials/<trial_id>/out/result.json
ls .lab/runs/<run_id>/trials/<trial_id>/out
```

## Agent Contract Failures

The agent must write valid JSON to `AGENTLAB_RESULT_PATH`.

Symptoms:

- `result.json` missing
- invalid JSON
- missing values referenced by declared metric `source.pointer` fields
- artifact paths point to files that do not exist

If a value appears in the agent response but not in `lab query <run_id> "SELECT * FROM metrics_long"`, check that `experiment.yaml` declares that metric. AgentLab does not persist undeclared custom metrics.

## Grader Transport Failures

For declarative grader transport, failures usually mean one link in the chain broke:

- an agent output was not captured
- a grader input source points at a missing output or field
- a grader input could not be materialized
- the grader command did not produce a declared output
- a metric transform cannot read the declared output

Symptoms:

- a required declared grader output is missing
- a declared grader output is invalid JSON
- a metric source points at a missing output or field
- grader command depends on a tool absent from the task image
- `strategy: host` command references package-local files, task-workdir support paths, `/agentlab` paths, or arbitrary absolute host paths

Fixes:

- For a project-owned grader script, use `in_task_runtime`, `injected`, or `separate`.
- For official SWE-bench host grading, declare `host.capability: swebench_official` and use `__AGENTLAB_RUNNER_BUILTIN_GRADER__/swebench_official/run_official_swebench_eval_from_agentlab.py`.
- Declare grader inputs and outputs explicitly; do not make the grader discover runner internals.

## Network And Secrets

Symptoms:

- provider API failures
- missing API key errors
- network unreachable errors

Fixes:

- pass `--env KEY=value` or `--env-file .env`
- set `trial_runtime.agent.network: full`
- set `policy.task_sandbox.network: full` only when the task sandbox also needs network
- leave `design.sanitization_profile` unset or use a non-hermetic profile for networked experiments

## Storage Growth

Use `--materialize` deliberately:

```bash
lab run .lab/builds/my-package --materialize outputs_only
lab run .lab/builds/my-package --materialize full
```

For debugging, `full` is easiest. For repeated large experiments, prefer a smaller materialization mode once you know what evidence you need.
