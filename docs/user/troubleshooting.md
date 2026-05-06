# Troubleshooting

Start with `lab preflight`. It catches many problems before a full run.

```bash
lab preflight .lab/builds/my-package --env-file .env --json
```

## Build Fails

Common causes:

- `experiment.yaml` has a removed or misspelled field.
- `tasks.jsonl` is malformed JSONL.
- `runtime.agent_runtime.artifact` does not exist.
- `benchmark.grader.command` references a file that is not packageable.
- A command or env binding references `$NAME`, but no variant binding or runtime env provides it.

What to inspect:

```bash
lab build experiment.yaml --out .lab/builds/debug --json
```

## Preflight Fails

Common causes:

- Docker or OrbStack is not running.
- Required image cannot be pulled.
- Agent runtime image lacks the command executable.
- Task image lacks the grader runtime, such as `python3` or `node`.
- Required runtime env var is missing.
- Grader cannot produce valid `trial_conclusion_v1`.

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
cat .lab/runs/<run_id>/trials/<trial_id>/out/mapped_grader_output.json
```

## Agent Contract Failures

The agent must write valid `trial_output_v1` to `AGENTLAB_RESULT_PATH`.

Symptoms:

- `result.json` missing
- invalid JSON
- wrong `schema_version`
- missing objective/metrics expected by the experiment
- artifact paths point to files that do not exist

## Grader Or Mapper Failures

The grader or mapper must write valid `trial_conclusion_v1`.

Symptoms:

- `mapped_grader_output.json` missing
- invalid conclusion schema
- mapper path is missing or outside allowed support paths
- grader command depends on a tool absent from the task image

## Network And Secrets

Symptoms:

- provider API failures
- missing API key errors
- network unreachable errors

Fixes:

- pass `--env KEY=value` or `--env-file .env`
- set `runtime.agent_runtime.network: full`
- set `policy.task_sandbox.network: full` only when the task sandbox also needs network

## Storage Growth

Use `--materialize` deliberately:

```bash
lab run .lab/builds/my-package --materialize outputs_only
lab run .lab/builds/my-package --materialize full
```

For debugging, `full` is easiest. For repeated large experiments, prefer a smaller materialization mode once you know what evidence you need.

