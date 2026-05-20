# Sidecars

Sidecars are per-trial services that the runner starts as part of the trial apparatus:

```yaml
sidecars:
  mcp_bash:
    image: ghcr.io/acme/mcp-bash-server:v0.4
    lifecycle: per-trial
    # Optional. Omit command to use the image default command.
    # command: ["mcp-bash-server", "--port", "8080"]
    expose:
      MCP_URL: http://mcp_bash:8080

trial_runtime:
  agent:
    sidecars: [mcp_bash]
  grader:
    sidecars: [mcp_bash]
```

Only `lifecycle: per-trial` is supported in v1. Stage-level `sidecars` lists must reference top-level `sidecars` entries.

Local Docker runs place sidecars and attached task/grader containers on a per-trial network. If `runtime.network.task_sandbox: none`, that network is internal so the sidecar can be reached by alias without opening external egress. Modal sidecars are rejected until a backend-native service attachment exists.
