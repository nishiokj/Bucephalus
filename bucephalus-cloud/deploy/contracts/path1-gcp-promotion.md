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
   `deploy_control_plane_services=false` when the Artifact Registry repository
   does not exist yet.
2. Confirm the Cloud SQL instance has no public IPv4 address.
3. Confirm required numeric Secret Manager versions exist and are readable only
   by their intended service identities.
4. Build and push Cloud images through the release workflow after the Artifact
   Registry repository exists.
5. Verify pushed image promotion evidence and update Terraform image inputs
   only from the generated `gcp-image-digests.tfvars`.
6. Render deploy tfvars from non-secret operator inputs, including the Google
   OAuth user client ID, explicit Secret Manager versions, and API-created
   runner pool ID, with `deploy_control_plane_services=true`.
7. Run Terraform plan against the remote GCS backend with both deploy tfvars and
   generated image digest tfvars.
8. Apply the migration Cloud Run Job revision for the selected migration image.
9. Run the migration Cloud Run Job using the migrator identity.
10. Apply Terraform to create Cloud Run service revisions for the selected API
   and pool-controller image digests.
11. Create or confirm the API-owned runner pool record and feed its ID into the
   pool controller Terraform input.
12. Smoke the API through the approved ingress path:
   - `/readyz`
   - an authenticated user API request
   - an authenticated worker API request
13. Observe logs, metrics, Cloud Run revision health, Cloud SQL connectivity, and
   pool controller reconciliation errors.
14. Record the promoted digests, migration result, smoke result, Terraform output
   snapshot, and operator identity.

The active GitHub Actions path is `.github/workflows/bucephalus-gcp-deploy.yml`.
It consumes the `cloud-image-promotion-evidence-<target>` artifact from a
release workflow run rather than accepting handwritten image digest inputs.

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
