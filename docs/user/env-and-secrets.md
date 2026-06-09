# Environment And Secrets

Secrets and network authorization live in [External Perimeter](external-perimeter.md).

Use `$NAME` in `stages.agent.command` or `stages.agent.env` to bind values supplied through `runtime.secrets` and launch-time `--env` / `--env-file` inputs.

For Cloud runs, pass secret material by provider reference with
`bucephalus-cloud run create --secret-ref NAME=...`; plaintext `--env` is for
non-secret configuration and blocks secret-looking keys by default.
