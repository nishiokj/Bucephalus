# Sidecars

Sidecars are per-trial services that the runner starts as part of the trial apparatus:

```yaml
sidecars:
  mcp_bash:
    image: ghcr.io/acme/mcp-bash-server:v0.4
    lifecycle: per-trial
    expose:
      MCP_URL: http://mcp_bash:8080

trial_runtime:
  agent:
    sidecars: [mcp_bash]
  grader:
    sidecars: [mcp_bash]
```

Only `lifecycle: per-trial` is supported in v1. Stage-level `sidecars` lists must reference top-level `sidecars` entries.
