# Deployment Surface Retired

The previous local-VM, SSH, systemd, startup-script, and handwritten provider
deployment materials have been intentionally removed.

Do not rebuild production deployment from deleted shell scripts or env examples.
The active goal-state is documented in:

`../../docs/specs/CLOUD_DEPLOYMENT_GOAL_STATE.md`

The next deployment surface should be rebuilt around:

- declared cloud infrastructure
- immutable build artifacts and images
- digest-based deploy promotion
- provider-managed secrets and identity
- API-mediated runner execution
- first-class runtime user-secret flow

It is acceptable for implementation work to stop on missing cloud credentials,
DNS, account setup, managed database access, or secret-manager policy. Do not
replace those blockers with local deployment shims.

The first Path 1 replacement surface is:

- `../infra/gcp/`: GCP Terraform substrate for durable resources.
- `contracts/path1-gcp-promotion.md`: digest promotion, migration, smoke,
  observation, and rollback contract.
- `path1-readiness.md`: remaining end-to-end readiness blockers.
- `.github/workflows/bucephalus-gcp-deploy.yml`: manual GCP deploy workflow
  that consumes verified pushed-image promotion evidence instead of handwritten
  image digests.

The deploy workflow is intentionally phased:

- `substrate-plan` / `substrate-apply`: declared GCP substrate, no app images.
- `api-plan` / `api-apply`: migration job plus API service. This phase needs
  image digests and DB/worker secret versions, but not a runner pool ID.
- `pool-plan` / `pool-apply`: pool-controller service. This phase runs after
  the API has created or confirmed a runner pool ID.

Runner provisioning for Path 1 is GCE per-run Docker VMs:

- the release workflow builds and pushes API, pool-controller, migrations, and
  worker images
- `gcp-image-digests.tfvars` carries all four immutable image refs into
  Terraform
- Terraform injects the worker image digest and GCE settings into the
  pool-controller service
- pool-controller command secrets point at
  `deploy/provider/gcp/provision-runner-vm.js` and
  `deploy/provider/gcp/reap-runner-vm.js`
- provisioned runner VMs have no public IP and use Cloud NAT for egress
