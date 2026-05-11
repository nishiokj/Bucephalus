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
- `benchmark.grader.command` references a file that does not match the selected grader strategy.
- `strategy: host` is pointing at a local or absolute script path instead of a declared `benchmark.grader.host.capability`.
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
cat .lab/runs/<run_id>/trials/<trial_id>/out/mapped_grader_output.json
```

## Agent Contract Failures

The agent must write valid `agent_result_v1` to `AGENTLAB_RESULT_PATH`.

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
- `strategy: host` command references package-local files, task-workdir support paths, `/agentlab` paths, or arbitrary absolute host paths

Fixes:

- For a project-owned grader script, use `in_task_image`, `injected`, or `separate`.
- For official SWE-bench host grading, declare `host.capability: swebench_official` and use `__AGENTLAB_RUNNER_BUILTIN_GRADER__/swebench_official/run_official_swebench_eval_from_agentlab.py`.
- Do not add `benchmark.grader.conclusion.mapper` to `strategy: host`; host graders must emit mapped output directly or use a runner-owned capability.

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
