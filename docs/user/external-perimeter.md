# Externals

Externals are boundary crossings outside runner jurisdiction: credentials, network egress, third-party APIs, and any other service whose lifecycle the runner does not own. Declare them so the run has hard accounting for everything that crossed the boundary.

The public authoring noun is `externals`. Existing `runtime.secrets` and `runtime.network` remain the canonical resolved shape:

```yaml
externals:
  apis: [api.openai.com]
  credentials: [OPENAI_API_KEY, CODEX_OAUTH]

runtime:
  secrets:
    - { name: OPENAI_API_KEY, from: env }
    - name: CODEX_OAUTH
      from: file
      mount:
        target: /root/.codex/auth.json
        required_for_variants: [codex_agent]
      credential_cache:
        kind: run_scoped
        target: /bucephalus/credentials/codex_oauth/auth.json
        env: CODEX_AUTH_CACHE_FILE
  network:
    default: none
    task_sandbox: none
    agent: full
    egress: [api.openai.com]
```

`from: env` secrets are available for `$NAME` substitution in commands and environment values. `from: file` secrets may declare a `mount.target`; the operator supplies the local source with `bucephalus run --secret-file NAME=/path/to/file`.

For Cloud runs, use provider secret refs instead of plaintext `--env` values:

```bash
bucephalus-cloud package secrets <package_digest>
bucephalus-cloud run create \
  --package-digest <package_digest> \
  --secret-ref OPENAI_API_KEY=gcp-secret-manager://projects/<project>/secrets/<secret>/versions/<version>
```

`bucephalus-cloud run create --env KEY=value` is for non-secret runtime
configuration. Secret-looking env keys are blocked by default because those
values are sent to the Cloud API as run configuration; use `--allow-secret-env`
only when the listed keys are intentionally non-secret.

`runtime.network.egress` is declarative in this patch. Local Docker enforcement still uses the selected network mode.

Ephemerals are not externals. In Local Docker runs, attaching ephemerals creates a per-trial network for the sandbox and ephemeral containers. When `runtime.network.task_sandbox: none`, that network is internal: attached containers can talk to each other by ephemeral id, but the network does not grant external egress.
