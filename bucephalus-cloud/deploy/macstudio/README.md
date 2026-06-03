# Mac Studio Host

The portable deployment unit is a Linux control-plane VM. A Mac Studio can host
that VM through OrbStack or another local virtualization layer, but Mac Studio is
not the product boundary. See
[`../control-plane/`](../control-plane/README.md) for the canonical deployment
contract.

When the Mac Studio hosts the local Linux VM, the shape is:

```text
Mac Studio
  Linux control-plane VM
    Postgres
    Cloud API
    pool-controller
    gcloud provider adapter

GCP
  ephemeral runner VM workers
    join Tailscale
    register with Cloud API
    claim runs from Cloud/Postgres
    invoke Core
```

The Mac Studio is just the physical host. The Linux VM is the private control
plane. The runner VMs are still isolated compute owned by the pool controller.

## Network Contract

Do not expose Postgres publicly. Bind Postgres and the API to the control-plane
VM's private/Tailscale address, and make runner VMs join the same private
network before they bootstrap.

```bash
tailscale ip -4
```

Use that address for:

```bash
BUCEPHALUS_CLOUD_HOST=0.0.0.0
BUCEPHALUS_CLOUD_API_URL=http://<control_plane_vm_private_ip>:8099
DATABASE_URL=postgres://bucephalus:bucephalus_dev@127.0.0.1:5432/bucephalus_cloud
BUCEPHALUS_WORKER_DATABASE_URL=postgres://bucephalus_runner:change-me@<control_plane_vm_private_ip>:5432/bucephalus_cloud
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

Use the Linux control-plane VM installer:

```bash
sudo /path/to/release/bucephalus-cloud/deploy/control-plane/install-control-plane.sh \
  --release-dir /path/to/release
```

Then configure `/etc/bucephalus-cloud/control-plane.env`, start Postgres, run
migrations from the admin/deploy side, and enable the API/pool-controller
systemd services as described in [`../control-plane/`](../control-plane/README.md).
