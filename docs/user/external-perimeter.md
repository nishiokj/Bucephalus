# Externals

Externals are boundary crossings outside runner jurisdiction: credentials, network egress, third-party APIs, and any other service whose lifecycle the runner does not own. Declare them so the run has hard accounting for everything that crossed the boundary.

The public authoring noun is `externals`. Use `runtime.secrets` and
`runtime.network` for credentials and network policy:

```yaml
externals:
  apis: [api.openai.com]
  credentials: [OPENAI_API_KEY, CODEX_OAUTH]

runtime:
  secrets:
    - { name: OPENAI_API_KEY }
    - name: CODEX_OAUTH
      mount:
        target: /root/.codex/auth.json
        required_for_variants: [codex_agent]
      credential_cache:
        kind: run_scoped
        target: /bucephalus/credentials/codex_oauth/auth.json
        env: CODEX_AUTH_CACHE_FILE
  network:
    default: none
    agent: full
    egress: [api.openai.com]
```

`externals` is a closed accounting object. It supports `apis` and
`credentials`, each as a duplicate-free list of non-empty strings. In authoring
YAML, omit `externals.credentials` when it is exactly the names in
`runtime.secrets`, and omit `externals.apis` when it is exactly
`runtime.network.egress`; the build derives those lists into the sealed
package. If you write an `externals` list explicitly, authoring validation
requires it to match the concrete runtime declarations before resolved package
checks are written.

Secrets default from their shape. Secret `name` values are unique runtime keys;
declare each credential once. A name-only secret defaults to `from: env`, which
makes it available for `$NAME` substitution in commands and environment values.
A secret with `mount` defaults to `from: file`; file secrets must declare
`mount.target`, and the operator supplies the local source with
`bucephalus run --secret-file NAME=/path/to/file`. Env secrets do not declare
mounts or credential caches.
Set `from` explicitly only when it matches the same shape: `from: env` has no
`mount` or `credential_cache`, and `from: file` always has `mount.target`.

`credential_cache` is only for file secrets that need a writable run-scoped
cache file derived from a read-only launch secret, so it must be paired with
that secret's `mount.target`. It is a closed object: `target` is required,
`kind` may be `run_scoped`, and `env` may expose the cache file path to the
agent. Credential cache `env` names must be unique across runtime secrets.
Both `mount.target` and `credential_cache.target` are
absolute container paths and cannot live under runner-owned paths such as
`/bucephalus/in`, `/bucephalus/out`, `/bucephalus/state`,
`/bucephalus/workspace`, `/workspace/task`, `/testbed`, or `/opt/agent`.

In authoring YAML, `runtime.network` is closed to `default`, `task_sandbox`,
`agent`, and `egress`. `default` is exclusive shorthand for setting both
runtime planes to the same `none`, `full`, or `allowlist_enforced` value; write
`task_sandbox` and `agent` explicitly for mixed modes. Use
`runtime.network.agent: llm_egress` only on the agent plane. The sealed package
keeps explicit `task_sandbox` and `agent` modes and rejects the shorthand.
`runtime.network.egress` is declarative in this patch and must be a
duplicate-free list. Local Docker enforcement still uses the selected network
mode. When `egress` is non-empty, at least one runtime plane must be able to use
it: set `runtime.network.agent` or `runtime.network.task_sandbox` to a non-`none`
mode.

Ephemerals are not externals. In Local Docker runs, attaching ephemerals creates a per-trial network for the sandbox and ephemeral containers. When `runtime.network.task_sandbox: none`, that network is internal: attached containers can talk to each other by ephemeral id, but the network does not grant external egress.
