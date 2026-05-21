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
- `policy.sanitization_profile: hermetic_functional` is combined with non-`none` `runtime.network.task_sandbox` or `runtime.network.agent`.
- a declared runtime output cannot be captured or materialized into a declared grader input.
- `tasks.jsonl` is malformed JSONL.
- `trial_runtime.agent.mount.source` does not exist, or `trial_runtime.agent.mount.mount.path/read_only` is missing.
- a `trial_runtime.*.sidecars` entry references an unknown sidecar id, or a sidecar id is not a valid runtime alias.
- `trial_runtime.grader.command` references a file that does not match the selected grader strategy.
- `strategy: host` is pointing at a local or absolute script path instead of a declared `trial_runtime.grader.host.capability`.
- A command or env value references `$NAME`, but no variant config value or runtime env provides it.

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
- Required sidecar image cannot be pulled or does not stay running.
- Agent runtime image lacks the command executable.
- Task image lacks the grader runtime, such as `python3` or `node`.
- Required runtime env var is missing.
- grader output capture fails.
- Host grader capability is missing or unknown.
- Modal executor is selected for an experiment that declares sidecars.
- The planned trial footprint exceeds `AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS` or `AGENTLAB_MODAL_MAX_ACTIVE_SANDBOXES`.

Fix preflight before running the full experiment.

## Smoke Validation Blocks A Run

`lab run` and `lab build-run` check whether the sealed package digest has
passed a smoke test. If it has not, interactive runs prompt you to smoke test,
run dangerously, or cancel. Non-interactive and `--json` runs fail fast unless
you make the choice explicit.

Run the validation path:

```bash
lab run .lab/builds/my-package --smoke-test --env-file .env --json
```

Or, for a one-command build from YAML plus smoke run:

```bash
lab build-run experiment.yaml --out .lab/builds/my-package --smoke-test --env-file .env --json
```

After the smoke run completes successfully, the same package digest is marked
smoke-tested in the account database and a full run can proceed:

```bash
lab run .lab/builds/my-package --env-file .env --json
```

To bypass validation intentionally:

```bash
lab run .lab/builds/my-package --run-dangerously --env-file .env --json
```

If validation never seems to stick, check whether `AGENTLAB_HOME` or
`AGENTLAB_DB` points at a fresh path on every invocation. The smoke-tested flag
is durable only within the account database selected by those variables.

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

If trials appear to wait before launch during a very concurrent run, check the active runtime caps. Local Docker defaults to `24` active AgentLab-owned containers on the Docker daemon, counting task sandboxes, sidecars, and separate grader sandboxes. Modal defaults to `64` active sandboxes per runner process.

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
- For host grading, declare a runner-owned `host.capability` and use that capability's supported command surface.
- Declare grader inputs and outputs explicitly; do not make the grader discover runner internals.

## Network And Secrets

Symptoms:

- provider API failures
- missing API key errors
- network unreachable errors

Fixes:

- pass `--env KEY=value` or `--env-file .env`
- set `runtime.network.agent: full`
- set `runtime.network.task_sandbox: full` only when the task sandbox also needs network
- leave `policy.sanitization_profile` unset or use a non-hermetic profile for networked experiments

For sidecars, use the sidecar id as the hostname, for example `http://mcp-bash:8080`. If `runtime.network.task_sandbox: none`, Local Docker still allows sandbox-to-sidecar traffic on the internal per-trial network, but not external egress.

Host stages cannot attach container sidecars. Move the stage into a container runtime, or call a host-owned capability directly instead of declaring a sidecar.

## Storage Growth

Use `--materialize` deliberately:

```bash
lab run .lab/builds/my-package --materialize outputs_only
lab run .lab/builds/my-package --materialize full
```

For debugging, `full` is easiest. For repeated large experiments, prefer a smaller materialization mode once you know what evidence you need.
