# GCP Path 1 Substrate

This Terraform directory declares the first production-shaped Bucephalus Cloud
substrate for Path 1 in `docs/specs/CLOUD_DEPLOYMENT_GOAL_STATE.md`.

It owns durable cloud resources only:

- required GCP APIs
- service identities for API, pool controller, and migrations
- Artifact Registry repository for digest-addressed images
- private VPC, subnet, service networking peering, and Serverless VPC connector
- private Cloud SQL for Postgres
- Secret Manager secret containers and IAM grants
- Cloud Run services for API and pool controller
- Cloud Run Job target for migrations

It does not create secret versions, inject plaintext credentials, publish
images, create DNS, create load balancer policy, or provision runner capacity.
Those are separate deployment, artifact, account, and runtime boundaries.

The first environment decisions are:

- region: `us-central1`
- environment: `bucephalus`
- image registry: GCP Artifact Registry
- deployment operator: GitHub Actions, with local operator runs using the same
  promotion contract
- database auth: Secret Manager database URL versions for now
- API ingress: public Cloud Run URL protected by app-layer Google OAuth

## Terraform State

This module declares a partial GCS backend. CI/CD and local operators must
provide backend configuration during initialization:

```bash
terraform init \
  -backend-config="bucket=<terraform-state-bucket>" \
  -backend-config="prefix=<environment>/gcp"
```

Do not apply this module from ephemeral local state. A deploy runner without
backend access should stop at `terraform init` rather than creating an
untracked environment.

## Artifact Registry Lifecycle

The Artifact Registry repository keeps tagged image versions available for
promotion and rollback. Terraform configures cleanup only for untagged image
versions older than 30 days:

```text
tag_state = UNTAGGED
older_than = 2592000s
```

Do not use untagged images as promotion inputs. Promotion and rollback must use
the digest refs recorded in the pushed image manifest, provenance, and tfvars
handoff. If a future registry policy deletes tagged versions, it must preserve
known-good rollback digests before production promotion is allowed.

## Inputs

The first substrate apply should set `deploy_control_plane_services = false`.
That creates durable prerequisites such as Artifact Registry, network, service
accounts, Cloud SQL, and Secret Manager containers without requiring image
digests that cannot exist until the registry exists.

Service promotion should set `deploy_control_plane_services = true`. In that
mode all image inputs must be digest-addressed:

```text
<registry>/<image>@sha256:<64 hex chars>
```

Mutable tags such as `latest` and all-zero placeholder digests are rejected by
Terraform variable validation. For GCP promotion, generate these inputs from
the pushed image manifest:

```bash
scripts/release/write-gcp-image-tfvars.sh \
  --image-manifest dist/releases/cloud-image-build-x86_64-unknown-linux-gnu/cloud-image-build-manifest.json \
  --out dist/releases/cloud-image-build-x86_64-unknown-linux-gnu/gcp-image-digests.tfvars
```

Do not hand-write image digest values into environment tfvars except as an
explicit rollback to a previously recorded promotion digest. The GitHub deploy
workflow downloads the `cloud-image-promotion-evidence-<target>` artifact from a
release workflow run, verifies `cloud-image-promotion-evidence.json`, and passes
the generated `gcp-image-digests.tfvars` as a separate Terraform var-file.

The deployment sequence is optimized around a fast development promotion and a
single service-stage production promotion:

0. Bootstrap GitHub/GCP OIDC wiring once with
   `scripts/deploy/bootstrap-gcp-github-oidc.sh --apply`. Review the dry-run
   output first; it creates the Terraform state bucket, publisher/deployer
   service accounts, Workload Identity provider, IAM grants, GitHub Actions
   secrets, and the non-secret GitHub repository/environment variables consumed
   by release, GCP deploy, GCP cleanup, and Cloudflare UI deploy workflows.
   Treat the GitHub UI as reconciled output, not the source of truth.
0.1. Verify the deploy boundary before running a deployment:
   `scripts/deploy/verify-gcp-cicd-readiness.sh --project-id <project>`.
   Add `--require-api-stage`, `--require-pool-stage`,
   `--require-cloudflare-ui-stage`, or `--require-all-deploy-stages` when those
   stages should be runnable now.
1. Run `.github/workflows/bucephalus-gcp-deploy.yml` with
   `deployment_stage=substrate` and `apply=true`.
2. For normal development, merge to `main`. After `Bucephalus Cloud CI` passes,
   `.github/workflows/bucephalus-cloud-candidate.yml` classifies the change. A
   runtime-only change builds and pushes the deployable x86_64 Cloud images,
   writes promotion evidence, and deploys that exact evidence to the
   `bucephalus-dev` GitHub Environment with `deployment_stage=services`.
   Deploy-boundary-only changes run plan-only against the latest evidence, and
   mixed runtime/deploy-boundary changes build images but stop at a plan against
   that candidate evidence. Runtime changes bundled with candidate/CI policy
   changes also stop at plan-only. Unless that environment overrides
   `BUCEPHALUS_DEPLOYMENT_ENVIRONMENT`, the deploy workflow maps it to the
   Terraform-safe environment label `dev`.
3. For production-style releases, run `.github/workflows/bucephalus-release.yml`
   with `version_override=<version>` against the created Artifact Registry
   repository. Production promotion uses `.github/workflows/bucephalus-gcp-deploy.yml`
   with `deployment_stage=services` and `apply=true`; the production GitHub
   Environment should provide the single human approval gate.
4. `deployment_stage=api` and `deployment_stage=pool` remain accepted for older
   runbooks, but new deployments should not require separate API and pool
   dispatches.

For a separate development substrate, bootstrap with an explicit short
deployment label, for example:

```bash
scripts/deploy/bootstrap-gcp-github-oidc.sh \
  --project-id <project> \
  --github-environment bucephalus-dev \
  --environment dev \
  --apply
```

Service cleanup is explicit. Use `.github/workflows/bucephalus-gcp-cleanup.yml`
with `cleanup_target=pool-controller` to remove only the pool-controller, or
`cleanup_target=control-plane-services` to remove Cloud Run services/jobs while
leaving durable substrate resources in place. Do not use
`deployment_stage=substrate` as teardown; the deploy workflow refuses that once
Cloud Run resources exist in Terraform state. Full substrate destroy is a
separate manual operation and Cloud SQL has deletion protection enabled.

## User OAuth Boundary

The API is configured as a Google OAuth resource server. Terraform requires the
user-facing OAuth client ID as `oauth_user_client_id` and injects it into the
API as `BUCEPHALUS_CLOUD_OAUTH_AUDIENCE`, because the verifier checks the JWT
`aud` claim. The default JWKS URL is Google's token cert endpoint:

```hcl
oauth_issuer         = "https://accounts.google.com"
oauth_user_client_id = "<google-oauth-client-id>.apps.googleusercontent.com"
oauth_jwks_url       = "https://www.googleapis.com/oauth2/v3/certs"
```

The OAuth client ID is not a secret. OAuth client secrets, if a browser or CLI
flow later needs one, must not be placed in Terraform variables or state.

## Secret Boundary

Terraform creates these Secret Manager containers:

- API database URL
- migrator database URL
- worker token
- runner admin token
- Modal token ID
- Modal token secret
- Modal S3-compatible access key ID
- Modal S3-compatible secret access key
- pool controller provision command JSON
- pool controller reap command JSON

Operators or CI/CD must create secret versions after database roles and tokens
exist. Do not put secret values in `.tfvars`, Terraform state, VM metadata,
startup scripts, image layers, or checked-in examples.

Cloud Run references explicit numeric secret versions. Rotation is a controlled
promotion: create a new Secret Manager version, update the corresponding
Terraform input, apply, smoke, and record the result.

`runner_admin_token_secret_version` is optional for compatibility. When unset,
the API uses the worker token for runner-pool administration. When set,
Terraform injects `BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN` into the API service
only; the pool controller and runner VMs continue to receive only the worker
token needed for worker registration, queue claims, package downloads, and
attempt updates.

The pool controller command JSON secrets are an interim accommodation for the
current application interface, which accepts provider commands through
configuration. They must point at provider code shipped in the selected image or
another declared artifact, not at retired SSH/startup-script deployment
materials. Path 3 should replace this with a first-class provider boundary.

## Modal Backend

Set `modal_backend_enabled=true` only after the Modal token and runtime sync
bucket are ready. The pool controller then advertises both `runner-docker` and
`modal`, and each per-run GCE worker VM fetches Modal credentials from Secret
Manager during startup before launching the worker container.

Required inputs when Modal is enabled:

- `modal_app_name`
- `modal_token_id_secret_version`
- `modal_token_secret_secret_version`
- `modal_s3_bucket`
- non-empty `modal_s3_prefix`
- either `modal_s3_secret_name` for an existing Modal Secret, or both
  `modal_s3_access_key_id_secret_version` and
  `modal_s3_secret_access_key_secret_version`

When Modal task images live in private GCP Artifact Registry, also set either
`modal_gcp_artifact_registry_secret_name` for an existing Modal Secret with
`SERVICE_ACCOUNT_JSON`, or
`modal_gcp_artifact_registry_service_account_json_secret_version` to inject the
JSON from Secret Manager into an ephemeral Modal Secret at launch time.

For Cloudflare R2, set `modal_s3_endpoint_url` to the account endpoint and
`modal_s3_region="auto"`. Keep `modal_s3_force_path_style=false`; Modal's Go SDK
CloudBucketMount API does not support path-style S3 mounts, so Terraform blocks
that configuration before a worker VM is provisioned.

## Database Ownership

The Cloud SQL instance is private-only. Terraform creates the database but does
not create password-bearing SQL users because provider-managed SQL user
passwords are persisted in Terraform state.

The default database profile is intentionally cost-conscious for early
`bucephalus` deployments: zonal `db-g1-small`, 10 GiB SSD, backups enabled, and
point-in-time recovery disabled. Production-like environments should override
the Cloud SQL variables explicitly rather than inheriting accidental HA/capacity.

Before promotion can run end to end, an admin migration identity must create the
runtime and migrator database credentials out of band, store them as Secret
Manager versions, and keep their permissions separated:

- runtime API/pool-controller credential: application runtime privileges
- migrator credential: schema migration privileges

If the project later supports Cloud SQL IAM auth end to end, this boundary can
move from database URL secrets to IAM-authenticated connections without changing
the resource ownership model. Do not enable that prematurely; the current path
is explicit URL secrets in Secret Manager.

## Pool Controller Ownership

Terraform does not create runner pool rows in Postgres. The runner pool ID is an
explicit input returned by the Cloud API after migrations and API deployment.
This keeps durable application state API-owned rather than hidden inside infra
provisioning.

## Deployment Contract

Deployment promotion is defined in
`../../deploy/contracts/path1-gcp-promotion.md`. Terraform applies substrate and
service image references; deployment orchestration is responsible for migration,
smoke, observation, and rollback to a previous digest.

## Validation

With credentials and a backend configured:

```bash
terraform init \
  -backend-config="bucket=<terraform-state-bucket>" \
  -backend-config="prefix=<environment>/gcp"
terraform validate
terraform plan -var-file=environments/dev.tfvars -var-file=/path/to/gcp-image-digests.tfvars
```

A clean block on missing project, credentials, secret versions, DNS, or account
policy is expected. Do not replace those blocks with local deployment shims.
