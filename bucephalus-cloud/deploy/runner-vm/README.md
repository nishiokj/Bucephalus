# Runner VM Bootstrap

This directory contains the self-hosted runner VM bootstrap contract.

The production boundary is a long-running daemon on a VM. The daemon registers
itself as a runner instance in an existing runner pool, heartbeats that instance,
claims compatible runs, downloads sealed packages through the Cloud API, and
invokes Core.

## Ownership

- Runner pool: durable Cloud capacity configuration.
- Runner instance: one VM daemon in that pool.
- Run attempt: one lease of a Cloud run by a runner instance.
- Core run: local in-VM trial lifecycle and recovery.

## Prerequisites

The VM image or provisioning system must provide:

- Bun on `PATH`
- the Bucephalus Core binary, usually `/usr/local/bin/bucephalus`
- the Cloud worker source directory, usually `/opt/bucephalus-cloud`
- Docker and `/var/run/docker.sock` if advertising `docker_daemon`
- registry credentials for all package image refs the VM may pull
- secret files or a mounted secret directory if runs use `secret_refs`
- network access to the Cloud API and Postgres queue endpoint
- enough free space under `BUCEPHALUS_CLOUD_DATA_DIR` for the pool's advertised
  run shape; the daemon refuses work below `BUCEPHALUS_WORKER_MIN_FREE_BYTES`

## Bootstrap

Create a runner pool through the Cloud CLI or API:

```bash
BUCEPHALUS_CLOUD_API_URL=https://api.example \
bun run cli -- runner-pool create \
  --name self-hosted-runner-docker \
  --executors runner-docker \
  --resources core_runner,docker_daemon,registry_pull
```

On the VM, run:

```bash
export BUCEPHALUS_CLOUD_API_URL=https://api.example
export DATABASE_URL=postgres://runner:password@postgres.example:5432/bucephalus_cloud
export BUCEPHALUS_WORKER_DATABASE_URL=$DATABASE_URL
export BUCEPHALUS_RUN_STORE_SCHEMA=bucephalus_runtime
export BUCEPHALUS_RUNNER_POOL_ID=<runner_pool_id>
export BUCEPHALUS_WORKER_ID=$(hostname -s)
export BUCEPHALUS_CORE_RUNNER_CMD=/usr/local/bin/bucephalus
export BUCEPHALUS_CLOUD_WORKER_DIR=/opt/bucephalus-cloud

sudo -E bash /opt/bucephalus-cloud/deploy/runner-vm/bootstrap-runner-vm.sh
```

Before starting runner VMs, apply Cloud database migrations from the admin/dev
side with `bun run db:migrate`. The runner VM Postgres role must be
least-privilege runtime credentials for the migrated schema; it should not have
permission to create, alter, or drop schemas, tables, or indexes.

The script validates the VM edge dependencies, writes
`/etc/bucephalus-runner/runner.env`, installs
`/etc/systemd/system/bucephalus-runner.service`, enables the service, and starts
the daemon.

## Operations

```bash
systemctl status bucephalus-runner
journalctl -u bucephalus-runner -f
systemctl restart bucephalus-runner
```

Draining should happen through the Cloud API first:

```bash
curl -X POST https://api.example/v1/runner-instances/<runner_instance_id>/drain
```

Then stop the VM or service after active attempts finish.

## Cleanup Contract

Runner VMs are reusable but poisonable. On startup, the daemon removes stale
Bucephalus Docker resources and stale attempt workspaces before claiming any
run. After each attempt, it removes Docker resources labeled with the Core run id
and deletes the attempt workspace unless
`BUCEPHALUS_WORKER_RETAIN_ATTEMPT_WORKSPACES=true`.

If mandatory cleanup fails, the daemon marks its runner instance `unhealthy` and
stops. Do not restart an unhealthy VM back into service without inspecting or
replacing it.
