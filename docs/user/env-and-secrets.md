# Environment And Secrets

Secrets and network authorization live in [External Perimeter](external-perimeter.md).

Use `$NAME` in `stages.agent.command` or `stages.agent.env` to bind values supplied by variant `config` or explicit launch-time `--env` / `--env-file` inputs.
Agent env values are scalar process environment values. Buc does not infer file staging from env values that happen to look like local paths; put file path arguments in `stages.agent.command` or use explicit stage/runtime asset fields.

For credentials, declare the required operator-provided names in `runtime.secrets`:

```yaml
runtime:
  secrets:
    - { name: OPENAI_API_KEY }

stages:
  agent:
    env:
      OPENAI_API_KEY: "$OPENAI_API_KEY"
```

Ambient host environment variables are not runtime bindings. Pass them explicitly with `--env NAME=...` or `--env-file`.
Env files use one `KEY=VALUE` binding per line. Keys must be unique across all launch-time sources: within each env file, across repeated env files, and across `--env KEY=...` flags.
