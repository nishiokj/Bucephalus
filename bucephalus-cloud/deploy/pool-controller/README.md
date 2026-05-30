# Pool Controller

The pool controller is the Cloud capacity loop for one runner pool.

It is not a runner and it does not execute experiments. It watches durable run
demand in Postgres, creates a durable provision request for each queued run that
has no compatible online runner, and calls a provider adapter to create or start
a runner VM.

## Process

Run one controller process per managed runner pool:

```bash
DATABASE_URL=postgres://bucephalus_cloud:change-me@postgres.example:5432/bucephalus_cloud \
BUCEPHALUS_CLOUD_API_URL=https://api.example \
BUCEPHALUS_CLOUD_WORKER_TOKEN=change-me \
BUCEPHALUS_POOL_CONTROLLER_POOL_ID=<runner_pool_id> \
BUCEPHALUS_POOL_CONTROLLER_PROVIDER=exec \
BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON='["/opt/bucephalus-cloud/deploy/provider/provision-runner-vm"]' \
BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON='["/opt/bucephalus-cloud/deploy/provider/reap-runner-vm"]' \
bun run pool-controller
```

## Provisioning Contract

The current provider interface is intentionally small and explicit. The
controller executes `BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON`, writes a
JSON object to stdin, and expects a JSON object on stdout.

Input:

```json
{
  "api_url": "https://api.example",
  "runner_pool_id": "00000000-0000-0000-0000-000000000000",
  "provision_request_id": "11111111-1111-1111-1111-111111111111",
  "run_id": "22222222-2222-2222-2222-222222222222",
  "run_requirements": {
    "executor": "runner-docker",
    "requires": ["core_runner", "docker_daemon", "registry_pull"],
    "image_refs": ["ghcr.io/acme/task@sha256:..."]
  },
  "worker_env": {
    "BUCEPHALUS_CLOUD_API_URL": "https://api.example",
    "BUCEPHALUS_RUNNER_POOL_ID": "00000000-0000-0000-0000-000000000000",
    "BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID": "11111111-1111-1111-1111-111111111111"
  }
}
```

Output:

```json
{
  "provider_instance_id": "i-0123456789abcdef0",
  "instance_name": "buc-runner-i-0123456789abcdef0",
  "metadata": {
    "region": "us-east-1",
    "image_id": "ami-..."
  }
}
```

The provider command must inject `worker_env` plus
`BUCEPHALUS_CLOUD_WORKER_TOKEN` into the VM through cloud-init, a secrets
manager, or equivalent provider-native bootstrapping. The VM must register using
`BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID`; registration reconciles the provision
request to `active`. Registration with a missing, failed, reaped, or already
active provision request is rejected so late VMs do not silently join a pool as
unmanaged capacity.

## Reaping Contract

The same provider boundary owns teardown. The controller executes
`BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON`, writes JSON to stdin, and expects a
JSON object on stdout.

Input:

```json
{
  "api_url": "https://api.example",
  "runner_pool_id": "00000000-0000-0000-0000-000000000000",
  "provision_request_id": "11111111-1111-1111-1111-111111111111",
  "run_id": "22222222-2222-2222-2222-222222222222",
  "provider": "exec",
  "provider_instance_id": "i-0123456789abcdef0",
  "instance_name": "buc-runner-i-0123456789abcdef0",
  "runner_instance_id": "33333333-3333-3333-3333-333333333333",
  "runner_instance_status": "unhealthy",
  "runner_instance_metadata": {},
  "requirements": {
    "executor": "runner-docker",
    "requires": ["core_runner", "docker_daemon", "registry_pull"],
    "image_refs": ["ghcr.io/acme/task@sha256:..."]
  },
  "metadata": {}
}
```

Output:

```json
{
  "metadata": {
    "terminated": true
  }
}
```

The reaper must be idempotent. If the VM is already gone, it should return
success and include provider evidence in `metadata`.

## Lifecycle

- `requested`: Cloud saw demand and recorded durable intent to create capacity.
- `provisioning`: provider invocation started; a provider VM id may be recorded.
- `active`: the VM daemon registered and reconciled to the provision request.
- `failed`: the provider command failed before a VM registered.
- `reaped`: the provider reaper accepted teardown or confirmed the VM is gone.

Unhealthy, offline, and busy runner instances are not counted as free capacity.
The pool controller reaps provider VMs for active provision requests whose
runner instances are `offline` or `unhealthy`, failed provision requests that
returned a provider VM id, and provisioning requests that exceed
`BUCEPHALUS_POOL_CONTROLLER_PROVISIONING_TIMEOUT_SECONDS`. Open requests that
never record a provider VM id are failed after the same timeout so demand can be
retried.

Provider commands are killed after
`BUCEPHALUS_POOL_CONTROLLER_PROVIDER_CMD_TIMEOUT_MS` and must be safe to retry.
