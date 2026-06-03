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
- Active local GCP project discovered from `gcloud`: `gen-lang-client-0255842044`
  with project number `380690977483`; billing is enabled.
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
- Terraform state backend: `gs://gen-lang-client-0255842044-bucephalus-tfstate`
  with prefix `bucephalus/gcp`, versioning, uniform bucket-level
  access, and public access prevention.
- Release image repository default:
  `us-central1-docker.pkg.dev/gen-lang-client-0255842044/buc-bucephalus-cloud/bucephalus-cloud`.
- User OAuth client ID default:
  `380690977483-3a2n2ttkbf352kmn3tl7qgr5ir1f34c3.apps.googleusercontent.com`.
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
- Choose the first runner provisioning implementation and network enforcement
  model. The current front-runner is GCE runners in the private VPC, with a
  sidecar/proxy option evaluated for egress policy, traces, and token/accounting
  telemetry.
- Keep `deploy_control_plane_services=false` for the first substrate apply. Turn
  it on only after images exist, secret values have been added, and the
  API-created runner pool ID is known.

Terraform status:

- `terraform init` succeeds against the GCS backend when supplied
  `bucket=gen-lang-client-0255842044-bucephalus-tfstate` and
  `prefix=bucephalus/gcp`.
- `terraform fmt -recursive` succeeded.
- `terraform validate` succeeds.
- `scripts/deploy/write-gcp-deploy-tfvars.sh` can generate substrate-only
  tfvars with `deploy_control_plane_services=false`, null OAuth/pool/secret
  promotion inputs, and no image refs.
- Local substrate apply completed on 2026-06-03 against project
  `gen-lang-client-0255842044` and environment `bucephalus`.
- A follow-up `terraform plan -detailed-exitcode` returned no changes.
- Cloud Run services/jobs remain intentionally disabled with
  `deploy_control_plane_services=false` until real image digests, secret value
  revisions, and the runner pool ID exist.

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
- `scripts/deploy/bootstrap-gcp-github-oidc.sh --project-id
  gen-lang-client-0255842044 --project-number 380690977483 --repository
  nishiokj/Bucephalus --environment bucephalus --resource-prefix buc` dry-runs
  the required API, bucket, service-account, Workload Identity, IAM, and GitHub
  secret setup. Add `--apply` only when ready to mutate cloud and repository
  settings.
