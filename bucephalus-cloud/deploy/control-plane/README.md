# Linux Control Plane VM

This is the portable Bucephalus Cloud deployment unit. The host can be an
OrbStack VM on a Mac Studio, a local Linux VM, or a future cloud VM. The product
boundary is not the host machine; it is a Linux control plane that runs:

```text
Linux control-plane VM
  Postgres
  Bucephalus Cloud API
  pool-controller
  provider adapter credentials

GCP runner VMs
  runner daemon
  Core binary
  Docker/runtime dependencies
```

Runner VMs are separate ephemeral compute capacity. The control plane records
durable run and capacity state, then asks the provider adapter to create or reap
runner VMs.

## Release Input

Install a Cloud release bundle built by:

```bash
scripts/release/build-buc-release.sh --version <version> --target x86_64-unknown-linux-gnu
```

The bundle must contain `bin/bucephalus` and `bucephalus-cloud/`. Runner VM
images and control-plane VMs should consume this bundle, not a live checkout.

## Install

On the Linux control-plane VM:

```bash
sudo /path/to/bucephalus-<version>-<target>/bucephalus-cloud/deploy/control-plane/install-control-plane.sh \
  --release-dir /path/to/bucephalus-<version>-<target>
```

This installs:

```text
/opt/bucephalus/releases/<release>/
/opt/bucephalus/current -> /opt/bucephalus/releases/<release>
/usr/local/bin/bucephalus
/etc/bucephalus-cloud/control-plane.env
/etc/systemd/system/bucephalus-cloud-api.service
/etc/systemd/system/bucephalus-cloud-pool-controller.service
/var/lib/bucephalus-cloud/
```

Edit `/etc/bucephalus-cloud/control-plane.env` before starting services.

## Postgres

Postgres may run natively, through Docker Compose, or as another private service
reachable from the control-plane VM. Do not expose Postgres publicly.

For a local VM smoke, Docker Compose is acceptable if Docker is installed:

```bash
cd /opt/bucephalus/current/bucephalus-cloud
docker compose up -d postgres
```

Run migrations from the deploy/admin side:

```bash
sudo /opt/bucephalus/current/bucephalus-cloud/deploy/control-plane/install-control-plane.sh \
  --release-dir /opt/bucephalus/current \
  --migrate
```

Runner VM database credentials must be least-privilege runtime credentials for
an already-migrated schema. Runner VMs should not have permission to create,
alter, or drop schemas, tables, or indexes.

## Start Services

```bash
sudo systemctl enable bucephalus-cloud-api.service
sudo systemctl enable bucephalus-cloud-pool-controller.service
sudo systemctl restart bucephalus-cloud-api.service
sudo systemctl restart bucephalus-cloud-pool-controller.service
```

Or run install plus migrations plus service restart in one step once the env file
is correct:

```bash
sudo /opt/bucephalus/current/bucephalus-cloud/deploy/control-plane/install-control-plane.sh \
  --release-dir /opt/bucephalus/current \
  --migrate \
  --start
```

## Smoke Check

```bash
/opt/bucephalus/current/bucephalus-cloud/deploy/control-plane/smoke-control-plane.sh
```

The smoke checks systemd service state, `/healthz`, `/readyz`, runner pools, and
provision requests.

## Runner Pool

Create one managed runner pool and store its id in
`BUCEPHALUS_POOL_CONTROLLER_POOL_ID`.

```bash
cd /opt/bucephalus/current/bucephalus-cloud
BUCEPHALUS_CLOUD_API_URL=http://<control-plane-private-ip>:8099 \
BUCEPHALUS_CLOUD_USER_TOKEN=<user-token> \
bun run cli -- runner-pool create \
  --name managed-gcp-runner-pool \
  --executors runner-docker \
  --resources core_runner,docker_daemon,registry_pull \
  --arch x86_64 \
  --cpu-count 2 \
  --memory-mb 4096 \
  --disk-mb 65536
```

## Provider Boundary

The pool-controller uses the `exec` provider boundary:

```text
pool-controller -> deploy/provider/gcp/provision-runner-vm.js
pool-controller -> deploy/provider/gcp/reap-runner-vm.js
```

Those commands speak JSON over stdio. They are intentionally outside the API
process so provider code and credentials do not become part of the public
control-plane server.

For a first smoke, `BUCEPHALUS_TAILSCALE_AUTHKEY` and worker credentials can be
injected through the GCP startup script. Before production, move runner VM
secret retrieval to a provider-native secret manager or equivalent mechanism.

## Local VM Example

OrbStack is only a host substrate:

```bash
orbctl create ubuntu bucephalus-control-plane
orbctl push dist/releases/bucephalus-<version>-x86_64-unknown-linux-gnu bucephalus-control-plane:/tmp/
orbctl run -m bucephalus-control-plane sudo /tmp/bucephalus-<version>-x86_64-unknown-linux-gnu/bucephalus-cloud/deploy/control-plane/install-control-plane.sh --release-dir /tmp/bucephalus-<version>-x86_64-unknown-linux-gnu
```

The same release and service contract should work on any Linux VM.
