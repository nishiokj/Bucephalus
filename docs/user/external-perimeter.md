# External Perimeter

The external perimeter lives under `runtime`:

```yaml
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
        target: /agentlab/credentials/codex_oauth/auth.json
        env: CODEX_AUTH_CACHE_FILE
  network:
    default: none
    task_sandbox: none
    agent: full
    egress: [api.openai.com]
```

`from: env` secrets are available for `$NAME` substitution in commands and environment values. `from: file` secrets may declare a `mount.target`; the operator supplies the local source with `lab run --secret-file NAME=/path/to/file`.

`runtime.network.egress` is declarative in this patch. Local Docker enforcement still uses the selected network mode.

Sidecars are part of the trial apparatus, not the external perimeter. In Local Docker runs, attaching sidecars creates a per-trial network for the sandbox and sidecar containers. When `runtime.network.task_sandbox: none`, that network is internal: attached containers can talk to each other by sidecar id, but the network does not grant external egress.
