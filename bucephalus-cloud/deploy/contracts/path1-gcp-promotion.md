# Path 1 GCP Deploy Promotion Contract

This is the deploy boundary for the first GCP substrate. It promotes immutable
image digests into infrastructure that already exists or is being declared by
Terraform. It is not an SSH, startup-script, systemd, Mac Studio, or local VM
deployment path.

## Required Inputs

- GCP project and region
- Terraform state backend and workspace/environment name
- GitHub deploy environment with a GCP Workload Identity deploy service account
- pushed Cloud image promotion evidence:
  - `cloud-image-build-manifest.json`
  - `cloud-image-build.provenance.json`
  - `gcp-image-digests.tfvars`
  - `cloud-image-promotion-evidence.json`
- digest-addressed API image from verified promotion evidence
- digest-addressed pool controller image from verified promotion evidence
- digest-addressed migration image from verified promotion evidence
- digest-addressed worker image from verified promotion evidence for GCE
  per-run runner VMs
- Google OAuth issuer, user OAuth client ID, and JWKS URL
- Secret Manager versions for:
  - API database URL
  - migrator database URL
  - worker token
  - pool controller provision command JSON
  - pool controller reap command JSON
- database roles created with separated runtime and migration privileges
- API-created runner pool ID for the pool controller
- smoke target URL through the approved ingress path

## Promotion Sequence

1. Apply or refresh the declared substrate in `bucephalus-cloud/infra/gcp` with
   `deploy_control_plane_services=false`, `deploy_api_services=false`, and
   `deploy_pool_controller=false` when the Artifact Registry repository does
   not exist yet.
2. Confirm the Cloud SQL instance has no public IPv4 address.
3. Confirm required numeric Secret Manager versions exist and are readable only
   by their intended service identities.
4. Build and push Cloud images through the release workflow after the Artifact
   Registry repository exists. The pushed promotion evidence must include API,
   pool-controller, migrations, and worker images.
5. Verify pushed image promotion evidence and update Terraform image inputs
   only from the generated `gcp-image-digests.tfvars`.
6. Create or confirm the API-owned runner pool record and feed its ID into the
   services deploy input before enabling the pool-controller service.
7. Render services deploy tfvars from non-secret operator inputs, including the
   Google OAuth user client ID, explicit API/migrator/worker secret versions, the
   API-created runner pool ID, and explicit pool-controller provider command
   secret versions, with `deploy_api_services=true` and
   `deploy_pool_controller=true`.
8. Confirm the pool-controller provider command secrets point at the image-owned
   GCE provider scripts and that the pool controller has the declared GCE runner
   service account/IAM grants.
9. Run Terraform plan against the remote GCS backend with services deploy tfvars
   and generated image digest tfvars.
10. Apply the exact Terraform plan generated in the same workflow run, creating
   or updating the migration job, API service, pool-controller service, and
   worker-image-promotion job.
11. Run the migration Cloud Run Job using the migrator identity.
12. Promote the active worker image through the worker-image-promotion job.
13. Smoke the API through the approved ingress path:
   - `/readyz`
   - an authenticated user API request
   - an authenticated worker API request
14. Observe logs, metrics, Cloud Run revision health, Cloud SQL connectivity, and
   pool controller reconciliation errors.
15. Record the promoted digests, migration result, smoke result, Terraform output
   snapshot, and operator identity.

The active GitHub Actions path is `.github/workflows/bucephalus-gcp-deploy.yml`.
It consumes the `cloud-image-promotion-evidence-<target>` artifact from a
release workflow run rather than accepting handwritten image digest inputs. Use
`deployment_stage=services` for normal app promotion after the API-created pool
ID exists. `deployment_stage=api` and `deployment_stage=pool` remain accepted
compatibility aliases for older runbooks. Leave `apply=false` for plan-only
review, or set `apply=true` to apply the generated plan from the same workflow
run.

The active cleanup path is `.github/workflows/bucephalus-gcp-cleanup.yml`.
`cleanup_target=pool-controller` removes only the pool-controller service.
`cleanup_target=control-plane-services` removes Cloud Run services/jobs while
retaining durable substrate resources. Full substrate teardown is not a routine
promotion action. `deployment_stage=substrate` also plans and applies that
control-plane service cleanup automatically when services are present in
Terraform state.

## Rollback

Rollback changes only versioned references and controlled schema state:

- return API and pool controller image inputs to the previous known-good digests
- apply Terraform to create new Cloud Run revisions for those digests
- do not mutate hosts by hand
- do not expose Postgres publicly to bypass private network failures
- do not inject replacement secrets through VM metadata, startup scripts, or
  checked-in files

Schema rollback must be explicit. If a migration is not reversible, rollback is
blocked until a forward fix or declared recovery plan exists.

## Clean Blocks

The deploy should stop, loudly and early, when any of these are missing:

- GCP credentials or project access
- Terraform backend access
- Secret Manager write/read policy
- database admin/migration credentials
- approved DNS or ingress path
- artifact registry digests
- Google OAuth client ID for user tokens
- smoke identity tokens

A clean block is the expected production behavior. Do not replace it with local
stand-ins.
