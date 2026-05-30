# GCP Runner Provider

This adapter turns a pool-controller provision request into a Google Compute
Engine runner VM. It speaks the Cloud provider JSON contract over stdin/stdout
and keeps the GCP lifecycle outside the API server.

## Shape

Provision:

```bash
BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON='["bun","run","deploy/provider/gcp/provision-runner-vm.js"]'
```

Reap:

```bash
BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON='["bun","run","deploy/provider/gcp/reap-runner-vm.js"]'
```

The provision adapter creates one VM per provision request. The VM receives a
GCP `startup-script` that exports the worker environment and runs
`deploy/runner-vm/bootstrap-runner-vm.sh`. The worker daemon then registers
against the provision request, heartbeats, claims compatible runs, and invokes
Core inside the VM.

## Required Environment

Use `gcp.env.example` as the base provider configuration. The pool controller
process also needs its normal environment:

```bash
DATABASE_URL=postgres://bucephalus_cloud:change-me@postgres.example:5432/bucephalus_cloud
BUCEPHALUS_CLOUD_API_URL=https://api.example
BUCEPHALUS_CLOUD_WORKER_TOKEN=change-me
BUCEPHALUS_POOL_CONTROLLER_POOL_ID=<runner_pool_id>
BUCEPHALUS_POOL_CONTROLLER_PROVIDER=exec
BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON='["bun","run","deploy/provider/gcp/provision-runner-vm.js"]'
BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON='["bun","run","deploy/provider/gcp/reap-runner-vm.js"]'
```

The VM must be able to reach the Cloud API and Postgres from its VPC. If the run
requires `registry_pull`, it must also be able to pull every remote image ref in
the sealed package.

If the control plane is reachable only over Tailscale, set
`BUCEPHALUS_TAILSCALE_AUTHKEY` for the provider adapter. The startup script
installs Tailscale, joins the tailnet, and only then bootstraps the runner
daemon. Use an ephemeral, tagged, pre-approved auth key for runner VMs.

For Mac-hosted control planes, set `BUCEPHALUS_WORKER_DATABASE_URL` when the
pool controller uses a local loopback `DATABASE_URL`. The provider sends
`BUCEPHALUS_WORKER_DATABASE_URL` to the VM as `DATABASE_URL`, so the VM can
connect over Tailscale without forcing local control-plane processes to hairpin
through the Mac's own tailnet address.

## Release Contract

There are two supported bootstrap modes:

- Baked image: the image already contains Bun, `/usr/local/bin/bucephalus`,
  `/opt/bucephalus-cloud`, Docker when advertised, and registry credentials.
- Release URL: set `BUCEPHALUS_RELEASE_URL` and optionally
  `BUCEPHALUS_RELEASE_SHA256`; startup downloads the release archive, installs
  Core into `/usr/local/bin/bucephalus`, and installs the Cloud bundle into
  `/opt/bucephalus-cloud`. Bun must still be present on the image.

## Notes on the Console Command

The Google Console command is a useful starting point, but two settings should
change for Buc runners:

- `--create-disk ... size=10` is too small. The worker currently refuses work
  below 20 GiB free and the provider defaults to at least 64 GB.
- `e2-medium` is valid only for small runs. The adapter chooses `e2-medium` for
  runs requiring at most 2 vCPU and 4096 MB, and otherwise chooses an
  `e2-standard-N` shape unless `BUCEPHALUS_GCP_MACHINE_TYPE` is set.

Snapshot schedules are intentionally not part of this adapter. Runner VMs are
ephemeral capacity owned by the pool controller; durable state belongs in
Postgres, package storage, logs, and registries.
