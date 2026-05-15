# Environment And Secrets

AgentLab supports runtime bindings through variant bindings, explicit CLI env, env files, and host env fallback.

## Resolution Order

When `trial_runtime.agent.command` or `trial_runtime.agent.env` contains `$NAME`, the runner resolves it from:

1. variant bindings
2. `--env NAME=value`
3. `--env-file`
4. host environment

Example:

```yaml
baseline:
  variant_id: control
  bindings:
    model: gpt-5.3-codex

trial_runtime:
  agent:
    command: ["python", "-m", "agent.run", "--model", "$model"]
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
```

Run with:

```bash
lab run .lab/builds/my-package --env OPENAI_API_KEY=...
```

Or:

```bash
lab run .lab/builds/my-package --env-file .env
```

`.env` format:

```bash
OPENAI_API_KEY=sk-...
ANTHROPIC_API_KEY=...
```

## Secret Files

Use `trial_runtime.agent.secret_files` for file secrets that should be resolved at launch time instead of packaged into the agent artifact. Secret files can be required for specific variants and are mounted at the declared target path for the agent.

Do not put secret files in `tasks.jsonl`, the agent artifact, or package-local support files.

## What Not To Do

- Do not hard-code secrets in `experiment.yaml`.
- Do not put secret files in the agent artifact.
- Do not rely on removed fields like `env_from_host`, `secret_env`, `runtime.dependencies.secret_files`, or `runtime.dependencies.file_staging`.
- Do not make the grader silently reuse agent secrets unless that is intentional and documented.

## Grader Env

Launch env is also available to graders. That matters for LLM-judge graders and official benchmark integrations that call provider APIs.

Declare the variable explicitly in the command or grader wrapper, then pass it at run time:

```bash
lab run .lab/builds/my-package --env ANTHROPIC_API_KEY=...
```

For container graders, contract path env values are container paths. For `strategy: host`, contract path env values are host paths so the host process can read and write the expected files directly.

## Network Policy

Provider-backed agents usually require:

```yaml
trial_runtime:
  agent:
    network: full
policy:
  task_sandbox:
    network: full
```

Hermetic benchmark runs should prefer:

```yaml
trial_runtime:
  agent:
    network: none
policy:
  task_sandbox:
    network: none
```

If network is disabled and your agent needs a provider API, preflight or execution will fail.

Separate grader containers use the run's effective network mode. Host graders use the host machine's normal network access.
