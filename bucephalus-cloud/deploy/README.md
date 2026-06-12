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
- `.github/workflows/bucephalus-cloud-candidate.yml`: fast Cloud candidate
  workflow that builds deployable x86_64 images after `main` Cloud CI succeeds
  and classifies the change before deciding whether to deploy, plan, or stop.
- `.github/workflows/bucephalus-gcp-deploy.yml`: manual GCP deploy workflow
  and reusable deployment backend that consumes verified pushed-image promotion
  evidence instead of handwritten image digests.

The candidate workflow separates changes into four high-level lanes:

- Runtime/release-bundle changes build pushed candidate images and auto-apply
  `deployment_stage=services` to `bucephalus-dev`.
- Deploy/Terraform boundary changes run a plan-only services deploy against the
  latest promotion evidence.
- Mixed runtime plus deploy-boundary changes build the candidate images, then
  run plan-only against that same candidate evidence.
- Runtime changes bundled with candidate/CI policy changes build the candidate
  images, then run plan-only.
- Docs, tests, examples, and CI-policy-only changes stop after Cloud CI and the
  classifier summary; unknown new paths are treated as runtime-affecting.

The normal deploy workflow is a single service promotion:

- `deployment_stage=services`: API, migration job, pool-controller, active
  worker image promotion, and smoke checks from one verified promotion evidence
  bundle.
- `deployment_stage=substrate`: rare bootstrap/substrate-only changes, no app
  images.
- `deployment_stage=api` and `deployment_stage=pool`: compatibility aliases for
  older manual runbooks; new promotions should use `services`.

The `bucephalus-dev` GitHub Environment is the default development target. The
deploy workflow maps it to the shorter Terraform environment label `dev` unless
`BUCEPHALUS_DEPLOYMENT_ENVIRONMENT` is set explicitly in that GitHub
Environment.

Every run creates a Terraform plan. Set `apply=true` only when that same run
should apply the generated plan file. Service applies execute the Cloud Run
migration job, promote the active worker image, and smoke the deployed API after
Terraform has applied the selected image digests.

Runner-pool administration can be split from worker authentication by creating
`${resource_prefix}-${environment}-runner-admin-token` in Secret Manager and
setting, or allowing CI to auto-resolve,
`BUCEPHALUS_RUNNER_ADMIN_TOKEN_SECRET_VERSION`. When that secret is configured,
set the GitHub environment secret `BUCEPHALUS_RUNNER_ADMIN_SMOKE` for the
post-deploy runner-pool smoke check. Without it, deploy keeps compatibility mode
where the API uses the worker token for runner-pool administration.

Cleanup is a separate workflow:

- `.github/workflows/bucephalus-gcp-cleanup.yml` with
  `cleanup_target=pool-controller` removes only the pool-controller service while
  preserving the API service.
- `cleanup_target=control-plane-services` removes Cloud Run control-plane
  services/jobs while preserving durable substrate resources such as Cloud SQL,
  Secret Manager containers, Artifact Registry, network, service accounts, and
  Terraform state.

When `deployment_stage=substrate` runs after Cloud Run services/jobs exist,
the generated Terraform plan automatically removes those service resources
while preserving durable substrate resources. Full substrate teardown remains a
manual Terraform destroy operation and is blocked by Cloud SQL deletion
protection unless that protection is intentionally changed.

Runner provisioning for Path 1 is GCE per-run worker VMs:

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
- runner VMs default to Google Container-Optimized OS
  (`projects/cos-cloud/global/images/family/cos-stable`) so Docker is already
  present at boot; the startup script only falls back to apt-based installation
  when an overridden boot image does not provide Docker
- when `modal_backend_enabled=true`, the same worker VM path fetches Modal and
  S3/R2 sync credentials from Secret Manager and advertises the `modal`
  executor; the worker image contains the packaged `bucephalus-modal-launcher`
  binary used by the Rust runner
