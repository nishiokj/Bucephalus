# Services

Services are per-trial resources that the runner starts and tears down, but
that are not links in the stage chain. Use them for local resources a stage
calls during execution, such as a sidecar container, MCP server, proxy, fixture
service, or benchmark helper daemon.

Authoring files should use `services`. The older `ephemerals` field is still
accepted as a compatibility alias and lowers to the same resolved package
contract.

## Shape

```yaml
services:
  mcp-bash:
    image: ghcr.io/acme/mcp-bash-server:v0.4
    lifecycle: trial
    # Optional. Omit command to use the image default command.
    command: ["mcp-bash-server", "--port", "8080"]
    # Optional.
    workdir: /srv/mcp
    # Optional env for the service container itself.
    env:
      LOG_LEVEL: info
    # Optional env injected into stages that attach this service.
    expose:
      MCP_URL: http://mcp-bash:8080
    # Optional readiness probe.
    readiness:
      timeout_ms: 10000
      http:
        url: http://mcp-bash:8080/health
        expect_status: 200

stages:
  agent:
    services: [mcp-bash]
  grader:
    services: [mcp-bash]
```

`lifecycle: trial` is the public authoring value. The build step writes the
resolved package as `per-trial`.

Service ids become runtime aliases, so they use portable DNS-label syntax:
lowercase letters, numbers, and `-`; they must start and end with a letter or
number. `image` is required. `command`, when present, must be an argv array of
non-empty strings. `env` and `expose` values must be string maps.

## Readiness

Readiness can be command-based or structured HTTP:

```yaml
services:
  data-api:
    image: ghcr.io/acme/data-api:latest
    lifecycle: trial
    command: ["data-api", "--port", "9757"]
    expose:
      DATA_API_URL: http://data-api:9757
    readiness:
      timeout_ms: 10000
      http:
        url: http://data-api:9757
        method: POST
        json:
          command: overview
        expect_status: 200
        interval_ms: 200
```

Use HTTP readiness when the check is a protocol call. Do not write a
`python -c` or shell loop into YAML for a normal HTTP probe; the runner owns
that polling behavior.

## Runtime Behavior

Local Docker runs place the trial sandbox, any separate grader sandbox, and
declared services on a per-trial Docker network. Each service is reachable by
its id as a hostname, such as `http://mcp-bash:8080`.

Modal supports `placement: same_sandbox` services. Same-sandbox services start
inside the task sandbox before the agent command, and structured HTTP readiness
is evaluated inside that sandbox. Modal does not yet support separate service
sandboxes.

Services are started by stage. Agent services start before the agent command.
Grader-only services start only if a grader phase runs, so a large run does not
keep grader services alive throughout long agent phases.

Services count against active runtime caps:

- Local Docker counts case sandboxes, service containers, and separate grader
  sandboxes toward `BUCEPHALUS_DOCKER_MAX_ACTIVE_CONTAINERS`.
- Modal counts case sandboxes and separate grader sandboxes toward
  `BUCEPHALUS_MODAL_MAX_ACTIVE_SANDBOXES`. Same-sandbox services do not add a
  separate sandbox.

If `runtime.network.task_sandbox: none`, the per-trial network is internal:
attached containers can talk to each other, but the service network does not
open external egress. If the trial needs external egress, configure the runtime
network explicitly.

`expose` values are injected only into stages that list the service. A service
declared under `stages.agent.services` does not leak its env into the grader
unless the grader also declares that service. Each stage-level `services` list
is duplicate-free, and attached services must not expose the same env name to
that stage.

Host stages cannot attach container services. `stages.execution.agent_site:
host` rejects `stages.agent.services`, and `stages.grader.strategy: host`
rejects `stages.grader.services`.

## Cleanup

Services are tracked in `trial_runtime_state.json` alongside case and grader
sandboxes. Trial cleanup and run kill paths include those runtime ids, so a
stopped or interrupted run should not leave service containers or same-sandbox
service processes behind.
