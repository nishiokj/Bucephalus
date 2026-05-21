# Sidecars

Sidecars are per-trial services that the runner starts as part of the trial apparatus. Use them for local services a stage calls during execution, such as an MCP server, proxy, fixture service, or benchmark helper daemon.

The public authoring noun is `sidecars`. Internally these are ephemeral runtime resources: they are attached to the stages that declare them, recorded in runtime state when started, and cleaned up with the trial.

## Shape

```yaml
sidecars:
  mcp-bash:
    image: ghcr.io/acme/mcp-bash-server:v0.4
    lifecycle: per-trial
    # Optional. Omit command to use the image default command.
    command: ["mcp-bash-server", "--port", "8080"]
    # Optional.
    workdir: /srv/mcp
    # Optional env for the sidecar container itself.
    env:
      LOG_LEVEL: info
    # Optional env injected into stages that attach this sidecar.
    expose:
      MCP_URL: http://mcp-bash:8080

trial_runtime:
  agent:
    sidecars: [mcp-bash]
  grader:
    sidecars: [mcp-bash]
```

Only `lifecycle: per-trial` is supported in v1. Stage-level `sidecars` lists must reference top-level `sidecars` entries.

Sidecar ids become runtime aliases, so they use portable DNS-label syntax: lowercase letters, numbers, and `-`; they must start and end with a letter or number. `image` is required. `command`, when present, must be an argv array of non-empty strings. `env` and `expose` values must be string maps.

## Runtime Behavior

Local Docker runs place the trial sandbox, any separate grader sandbox, and declared sidecars on a per-trial Docker network. Each sidecar is reachable by its id as a hostname, such as `http://mcp-bash:8080`.

Sidecars are started by stage. Agent sidecars start before the agent command. Grader-only sidecars start only if a grader phase runs, so a large run does not keep grader services alive throughout long agent phases.

Sidecars count against the Local Docker active container cap. The default cap is `24` active AgentLab-owned containers on the Docker daemon, including task sandboxes, separate grader sandboxes, and sidecars. Configure it with `AGENTLAB_DOCKER_MAX_ACTIVE_CONTAINERS`.

If `runtime.network.task_sandbox: none`, the per-trial network is internal: attached containers can talk to each other, but the sidecar network does not open external egress. If the trial needs external egress, configure the runtime network explicitly.

`expose` values are injected only into stages that list the sidecar. A sidecar declared under `trial_runtime.agent.sidecars` does not leak its env into the grader unless the grader also declares that sidecar.

Host stages cannot attach container sidecars. `trial_runtime.execution.agent_site: host` rejects `trial_runtime.agent.sidecars`, and `trial_runtime.grader.strategy: host` rejects `trial_runtime.grader.sidecars`.

Modal sidecars are not implemented yet. Modal runs reject experiments with sidecars instead of silently ignoring them.

## Cleanup

Sidecars are tracked in `trial_runtime_state.json` alongside task and grader sandboxes. Trial cleanup and run kill paths include those container ids, so a stopped or interrupted run should not leave sidecar containers behind.
