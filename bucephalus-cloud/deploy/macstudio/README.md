# Mac Studio Control Plane

This is the recommended self-hosted prototype shape when the Mac Studio already
has Tailscale:

```text
Mac Studio
  Postgres
  Cloud API
  pool-controller
  gcloud provider adapter

GCP
  ephemeral runner VM workers
    join Tailscale
    register with Cloud API
    claim runs from Postgres
    invoke Core
```

The Mac Studio is the private control plane. The runner VMs are still isolated
compute owned by the pool controller.

## Network Contract

Do not expose Postgres publicly. Bind Postgres and the API to the Mac Studio
Tailscale address, and make runner VMs join the same tailnet before they
bootstrap.

```bash
tailscale ip -4
```

Use that address for:

```bash
BUCEPHALUS_POSTGRES_TAILSCALE_BIND_ADDR=<macstudio_tailscale_ip>
BUCEPHALUS_CLOUD_HOST=<macstudio_tailscale_ip>
BUCEPHALUS_CLOUD_API_URL=http://<macstudio_tailscale_ip>:8099
DATABASE_URL=postgres://bucephalus:bucephalus_dev@127.0.0.1:55432/bucephalus_cloud
BUCEPHALUS_WORKER_DATABASE_URL=postgres://bucephalus:bucephalus_dev@<macstudio_tailscale_ip>:55433/bucephalus_cloud
```

For GCP runner VMs, set:

```bash
BUCEPHALUS_TAILSCALE_AUTHKEY=tskey-auth-...
BUCEPHALUS_TAILSCALE_EXTRA_ARGS=--accept-routes
```

Use an ephemeral, pre-approved, tagged auth key. The key is currently passed via
the provider startup script, which is acceptable for a controlled smoke path but
should move to a provider secret manager before production.

## Start the Control Plane

Postgres:

```bash
cd bucephalus-cloud
BUCEPHALUS_POSTGRES_TAILSCALE_BIND_ADDR=<macstudio_tailscale_ip> docker compose up -d postgres
bun run db:migrate
```

API:

```bash
BUCEPHALUS_CLOUD_HOST=<macstudio_tailscale_ip> \
PORT=8099 \
DATABASE_URL=postgres://bucephalus:bucephalus_dev@127.0.0.1:55432/bucephalus_cloud \
BUCEPHALUS_CLOUD_WORKER_TOKEN=<worker-token> \
bun run start
```

Pool controller:

```bash
DATABASE_URL=postgres://bucephalus:bucephalus_dev@127.0.0.1:55432/bucephalus_cloud \
BUCEPHALUS_WORKER_DATABASE_URL=postgres://bucephalus:bucephalus_dev@<macstudio_tailscale_ip>:55433/bucephalus_cloud \
BUCEPHALUS_CLOUD_API_URL=http://<macstudio_tailscale_ip>:8099 \
BUCEPHALUS_CLOUD_WORKER_TOKEN=<worker-token> \
BUCEPHALUS_POOL_CONTROLLER_POOL_ID=<runner_pool_id> \
BUCEPHALUS_POOL_CONTROLLER_PROVIDER=exec \
BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON='["bun","run","deploy/provider/gcp/provision-runner-vm.js"]' \
BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON='["bun","run","deploy/provider/gcp/reap-runner-vm.js"]' \
BUCEPHALUS_GCP_PROJECT=<gcp-project> \
BUCEPHALUS_GCP_ZONE=us-central1-a \
BUCEPHALUS_TAILSCALE_AUTHKEY=<ephemeral-auth-key> \
bun run pool-controller
```

## Isolation

For a first smoke, running these as three processes on the Mac Studio is fine.
For stronger isolation, run the control plane inside a Linux VM on the Mac
Studio and install Tailscale inside that VM. Keep the same contract: API and
Postgres bind to the VM's Tailscale IP, and GCP runner VMs join the tailnet.

Docker Compose is useful for Postgres. The pool controller should only run in a
container after the container image includes `gcloud` and has an explicit,
read-only credential mount.
