# Environment And Secrets

AgentLab supports runtime bindings through variant bindings, explicit CLI env, env files, and host env fallback.

## Resolution Order

When `runtime.agent_runtime.command` or `runtime.agent_runtime.env` contains `$NAME`, the runner resolves it from:

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

runtime:
  agent_runtime:
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

## What Not To Do

- Do not hard-code secrets in `experiment.yaml`.
- Do not put secret files in the agent artifact.
- Do not rely on removed fields like `env_from_host`, `secret_env`, or `runtime.dependencies.secret_files`.
- Do not make the grader silently reuse agent secrets unless that is intentional and documented.

## Network Policy

Provider-backed agents usually require:

```yaml
runtime:
  agent_runtime:
    network: full
policy:
  task_sandbox:
    network: full
```

Hermetic benchmark runs should prefer:

```yaml
runtime:
  agent_runtime:
    network: none
policy:
  task_sandbox:
    network: none
```

If network is disabled and your agent needs a provider API, preflight or execution will fail.

