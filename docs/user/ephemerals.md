# Ephemerals

Ephemerals are per-trial resources that the runner starts and tears down, but that are not links in the stage chain. Use them for local services a stage calls during execution, such as a sidecar container, MCP server, proxy, fixture service, or benchmark helper daemon.

Ephemerals are attached to the stages that declare them, recorded in runtime state when started, and cleaned up with the trial. Authoring files use `ephemerals`; the build step lowers them into the resolved package contract.

## Shape

```yaml
ephemerals:
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

stages:
  agent:
    ephemerals: [mcp-bash]
  grader:
    ephemerals: [mcp-bash]
```

Only `lifecycle: per-trial` is supported in v1. Stage-level `ephemerals` lists must reference top-level `ephemerals` entries.

Ephemeral ids become runtime aliases, so they use portable DNS-label syntax: lowercase letters, numbers, and `-`; they must start and end with a letter or number. `image` is required. `command`, when present, must be an argv array of non-empty strings. `env` and `expose` values must be string maps.

## Runtime Behavior

Local Docker runs place the trial sandbox, any separate grader sandbox, and declared ephemerals on a per-trial Docker network. Each ephemeral is reachable by its id as a hostname, such as `http://mcp-bash:8080`.

Ephemerals are started by stage. Agent ephemerals start before the agent command. Grader-only ephemerals start only if a grader phase runs, so a large run does not keep grader services alive throughout long agent phases.

Ephemerals count against the Local Docker active container cap. The default cap is `24` active Bucephalus-owned containers on the Docker daemon, including case sandboxes, separate grader sandboxes, and ephemerals. Configure it with `BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS`.

If `runtime.network.task_sandbox: none`, the per-trial network is internal: attached containers can talk to each other, but the ephemeral network does not open external egress. If the trial needs external egress, configure the runtime network explicitly.

`expose` values are injected only into stages that list the ephemeral. An ephemeral declared under `stages.agent.ephemerals` does not leak its env into the grader unless the grader also declares that ephemeral.

Host stages cannot attach container ephemerals. `stages.execution.agent_site: host` rejects `stages.agent.ephemerals`, and `stages.grader.strategy: host` rejects `stages.grader.ephemerals`.

Modal ephemerals are not implemented yet. Modal runs reject experiments with ephemerals instead of silently ignoring them.

## Cleanup

Ephemerals are tracked in `trial_runtime_state.json` alongside case and grader sandboxes. Trial cleanup and run kill paths include those container ids, so a stopped or interrupted run should not leave ephemeral containers behind.
