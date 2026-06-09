# Path 1 Readiness Notes

Completed in this slice:

- Added a GCP Terraform substrate for durable control-plane resources.
- Split API, pool controller, and migrator service identities.
- Declared private Postgres, Artifact Registry, Secret Manager containers,
  Cloud Run services, and a migration job.
- Enforced digest-addressed image inputs in Terraform.
- Rejected all-zero image placeholders in Terraform and promotion evidence.
- Approved the first production Bun base image digest for pushed Cloud image
  publication:
  `oven/bun@sha256:e10577f0db68676a7024391c6e5cb4b879ebd17188ab750cf10024a6d700e5c4`.
- Promoted the user Google OAuth client ID and JWKS URL to first-class
  Terraform inputs.
- Added a GCP deploy workflow that consumes pushed image promotion evidence,
  writes non-secret deploy tfvars, uses a remote GCS Terraform backend, applies
  selected image digests, runs the migration job, and smokes user plus worker
  API authentication.
- Split Terraform deployment into substrate-only and service-promotion stages so
  Artifact Registry can be created before any real pushed image digests exist.
- Added a deploy promotion contract that separates substrate, migration, smoke,
  observation, and rollback.

Operator decisions now captured:

- Cloud target: GCP.
- Active GCP project: supplied by the operator; pass `--project-id` and, when
  needed, `--project-number` to the bootstrap/readiness scripts.
- Local Terraform auth: Application Default Credentials are configured and can
  reach the project.
- Region: `us-central1` unless a later constraint forces a different central
  region.
- Environment name: `bucephalus`.
- Database authentication: Secret Manager database URL versions for now, not
  Cloud SQL IAM auth.
- Deploy operations: GitHub Actions, with local operator deployment still
  allowed as the same promotion contract using local ADC/gcloud credentials.
- Registry: GCP Artifact Registry is the default because it matches Cloud Run,
  Workload Identity, and digest promotion. Docker Hub is not needed for the
  first GCP path.
- Terraform state backend: `gs://<project-id>-bucephalus-tfstate`
  with prefix `bucephalus/gcp`, versioning, uniform bucket-level
  access, and public access prevention.
- Release image repository:
  `us-central1-docker.pkg.dev/<project-id>/buc-bucephalus-cloud/bucephalus-cloud`.
- User OAuth client ID:
  `<google-oauth-client-id>.apps.googleusercontent.com`.
- Initial API ingress: public Cloud Run URL protected by app-layer Google OAuth.
  Custom DNS and external load balancer policy can be promoted later.
- GUI auth direction: Google OAuth.
- CLI auth direction: simple browser/device-style OAuth flow against the same
  API user OAuth client ID.

Still needed before end-to-end production readiness:

- Publish real API, pool controller, and migration images by digest, then use
  only the generated `gcp-image-digests.tfvars` as Terraform image input.
- Create database runtime and migrator roles without storing passwords in
  Terraform state.
- Add actual values to the Terraform-created Secret Manager secret names:
  database URLs, worker token, and pool controller provision/reap command JSON.
  Each added value receives an integer version such as `1`; service deployment
  pins those integers.
- Pool-controller provider command versions have been written:
  `buc-bucephalus-pool-provision-cmd-json` version `1` and
  `buc-bucephalus-pool-reap-cmd-json` version `1`. These values point at the
  image-owned GCE per-run provider scripts.
- Wire GitHub Actions secrets/environments to GCP Workload Identity for Artifact
  Registry publishing, Terraform apply, migration job execution, and smoke
  checks. Use `scripts/deploy/bootstrap-gcp-github-oidc.sh` as the audited,
  dry-run-by-default bootstrap path for APIs, state bucket, service accounts,
  Workload Identity, IAM grants, and GitHub Actions secrets.
- Create smoke identities and store the user and worker smoke tokens as GitHub
  environment secrets for `.github/workflows/bucephalus-gcp-deploy.yml`.
- Keep the pool controller as an interim control-plane database client while
  runner VMs remain API clients. Moving pool-controller reconciliation fully
  behind API-owned endpoints is a later cleanup, not a runner boundary blocker.
- First runner provisioning implementation is GCE per-run Docker runner VMs:
  pool controller invokes image-owned provider scripts, creates no-public-IP
  VMs in the private VPC, pulls the digest-addressed worker image, starts the
  worker container against the host Docker daemon, and reaps the VM after the
  provision request is complete. Sidecar/proxy egress policy remains a later
  network enforcement implementation; v1 provider refuses runs that declare
  egress-host policy instead of silently granting ambient access.
- Keep `deploy_control_plane_services=false` for the first substrate apply. Turn
  it on only after images exist, secret values have been added, and the
  API-created runner pool ID is known.

Terraform status:

- `terraform init` succeeds against the GCS backend when supplied
  `bucket=<project-id>-bucephalus-tfstate` and
  `prefix=bucephalus/gcp`.
- `terraform fmt -recursive` succeeded.
- `terraform validate` succeeds.
- `scripts/deploy/write-gcp-deploy-tfvars.sh` can generate substrate-only
  tfvars with `deploy_control_plane_services=false`, null OAuth/pool/secret
  promotion inputs, and no image refs.
- Local substrate apply completed on 2026-06-03 against the selected GCP project
  and environment `bucephalus`.
- A follow-up `terraform plan -detailed-exitcode` returned no changes.
- Cloud Run services/jobs remain intentionally disabled with
  `deploy_control_plane_services=false` until real image digests and secret
  value revisions exist.
- Service promotion is split into API and pool-controller phases. The API phase
  uses `deploy_api_services=true` and `deploy_pool_controller=false`, creates
  the migration job plus API service, and does not require a runner pool ID. The
  pool phase uses `deploy_api_services=true` and `deploy_pool_controller=true`
  after an API-created runner pool ID exists.
- `gcp-image-digests.tfvars` now includes API, pool-controller, migrations, and
  worker image digest refs. The worker digest is injected into the pool
  controller as the GCE runner image; it is not copied by hand into metadata or
  Secret Manager.
- `.github/workflows/bucephalus-gcp-deploy.yml` now exposes
  `deployment_stage=substrate|api|pool` plus an explicit `apply` switch.
- `.github/workflows/bucephalus-gcp-cleanup.yml` now exposes explicit
  `cleanup_target=pool-controller|control-plane-services` service teardown.
- `scripts/deploy/bootstrap-gcp-github-oidc.sh` now reconciles the non-secret
  GitHub repository/environment variables consumed by release image
  publication, GCP deploy, GCP cleanup, and Cloudflare UI deploy workflows.
  `scripts/deploy/verify-gcp-cicd-readiness.sh` audits those values and can
  require stage-specific API, pool-controller, and Cloudflare UI deploy
  readiness.
- Terraform substrate apply for the runner additions completed on 2026-06-03.
  A follow-up plan returned no changes.

Observed external state on 2026-06-03:

- GitHub Actions secrets for this repository are empty, so release image
  publication and GCP deploy Workload Identity cannot run yet.
- The release workflow has no recent runs, so no pushed
  `cloud-image-promotion-evidence-*` artifact exists yet.
- GCP Artifact Registry in `us-central1` now contains
  `buc-bucephalus-cloud`.
- Terraform created private Postgres instance `buc-bucephalus-postgres`,
  database `bucephalus_cloud`, and private database IP `10.251.32.2`.
- Terraform created VPC `buc-bucephalus-control-plane`, subnet
  `buc-bucephalus-control-plane`, dedicated `/28` Serverless VPC connector
  subnet `buc-bucephalus-run-vpc-connector`, and VPC connector
  `buc-bucephalus-run-vpc`.
- Terraform created Secret Manager entries
  `buc-bucephalus-api-database-url`,
  `buc-bucephalus-migrator-database-url`,
  `buc-bucephalus-worker-token`,
  `buc-bucephalus-pool-provision-cmd-json`, and
  `buc-bucephalus-pool-reap-cmd-json`. These are secret names/containers; they
  still need actual values.
- Terraform created API, migrator, and pool-controller service accounts with
  least-scoped access to the relevant secret names.
- Terraform now declares a GCE runner service account, pool-controller
  permissions to create/delete per-run VMs using that identity, Artifact
  Registry reader access for the runner identity, worker-token access for the
  runner identity, and Cloud NAT for private runner VM egress without public VM
  IP addresses.
- Runner provider defaults are zone `us-central1-a`, machine type
  `e2-standard-2`, boot image
  `projects/ubuntu-os-cloud/global/images/family/ubuntu-2404-lts-amd64`, boot
  disk `100` GiB, subnet `buc-bucephalus-control-plane`, and service account
  `buc-bucephalus-runner@<project-id>.iam.gserviceaccount.com`.
- `scripts/deploy/bootstrap-gcp-github-oidc.sh --project-id
  <project-id> --project-number <project-number> --repository
  <owner>/<repo> --environment bucephalus --resource-prefix buc
  --google-oauth-client-id <google-oauth-client-id>.apps.googleusercontent.com`
  dry-runs
  the required API, bucket, service-account, Workload Identity, IAM, GitHub
  secret, repository variable, and environment variable setup. Add `--apply`
  only when ready to mutate cloud and repository settings.
