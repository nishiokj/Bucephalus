# Cloud Path 2 Artifact, Image, And CI Readiness

This document tracks what remains before Path 2 in
`docs/specs/CLOUD_DEPLOYMENT_GOAL_STATE.md` is end-to-end ready. It is scoped to
artifacts, images, checksums, provenance, and digest-based publishing. It does
not attempt to solve cloud substrate, deploy promotion, runner capacity, or
user-secret policy.

## Current State

- Cloud release bundles are built from a clean git revision by
  `scripts/release/build-buc-release.sh`.
- Core CLI release archives are built by `scripts/release/build-core-release.sh`
  and verified by `scripts/release/verify-core-release.sh`.
- `scripts/release/write-core-release-provenance.sh` writes unsigned recorded
  provenance for Core CLI archives, and
  `scripts/release/verify-core-release-provenance.sh` verifies it. When the
  release archive or directory is provided, the verifier also proves the
  provenance archive digest, manifest digest, release identity, and Core binary
  metadata match that exact release artifact.
- The bundle contains the Core binary, Cloud source, migrations, OpenAPI specs,
  Cloud package metadata, Cloud lockfile, deployment-boundary documentation, and
  the root `Cargo.lock` as release input metadata.
- `release-manifest.json` records the git revision, dirty bit, target, core
  binary checksum, packaged Modal launcher checksum, lockfile checksums, Cloud
  `package.json` checksum, and tree digests for Cloud source, migrations,
  OpenAPI specs, and image definitions.
  It also records the release-root `.dockerignore` image context guard.
- `SHA256SUMS` covers every bundled file except itself using exact
  `<sha256>  <relative-path>` records, and the archive has a sibling `.sha256`
  file with exactly one `<sha256>  <archive-name>` record.
- `scripts/release/verify-buc-release.sh` verifies archive checksums, exact
  bundled file checksum records, complete `SHA256SUMS` coverage for every
  bundled file except itself, manifest structure, source input digests,
  content-set tree digests, and absence of non-Markdown retired deploy payloads
  or env files. `scripts/release/verify-core-release.sh` applies the same exact
  `SHA256SUMS` coverage rule to Core CLI archives. The Cloud verifier also
  requires `.dockerignore` exclusions for generated Google Actions credentials,
  env files, Terraform state, dependency folders, and image-build metadata.
- `.github/workflows/bucephalus-release.yml` verifies each Linux cloud release
  archive before uploading it, and uploads only archive, checksum, and
  provenance assets rather than expanded release directories.
- `scripts/release/write-release-asset-index.sh` writes
  `release-assets.json` for the GitHub Release asset set after verifying every
  Core and Cloud archive plus its checksum and unsigned provenance. The tagged
  release workflow passes the required Core and Cloud target matrix so partial
  release asset sets cannot be indexed as complete production releases.
- `scripts/release/verify-release-asset-index.sh` validates the release asset
  index schema, self-digest, per-asset digests, exact single-record checksum
  files, provenance metadata, required target matrix when recorded, and unsigned
  status. Indexed archive, checksum, provenance, and asset-root paths must be
  artifact-local rather than absolute or traversal paths.
- Cloud API, pool-controller, migration, and worker Dockerfiles live under
  `bucephalus-cloud/images/` and are copied into the release bundle.
- `scripts/release/build-cloud-images.sh` builds those images only from a
  verified release bundle, requires a digest-addressed Bun base image, and
  records local image-boundary verification evidence for every component. It
  refuses release bundles that do not carry the required image build-context
  `.dockerignore` guard.
- `bucephalus-cloud/images/base-image-policy.json` records the reviewed base
  image policy. The current approved production base is
  `oven/bun@sha256:e10577f0db68676a7024391c6e5cb4b879ebd17188ab750cf10024a6d700e5c4`,
  resolved from `oven/bun:1.3.14` with Docker Hub registry response evidence
  and Linux amd64/arm64 child manifest evidence.
- `scripts/release/verify-cloud-base-image-policy.sh` validates the base-image
  policy and requires pushed images to use an approved base digest while still
  allowing local image inspection with any digest-addressed, non-`latest`,
  non-URL base.
- `scripts/release/verify-cloud-image-boundary.sh` inspects built image labels
  and fails when environment-specific runtime configuration or secret-looking
  values are baked into image metadata.
- `scripts/release/verify-cloud-image-build-manifest.sh` validates local image
  inspection manifests and requires immutable `image@sha256:<digest>` refs for
  pushed manifests under GCP Artifact Registry component repositories. It also
  requires every image entry to prove local boundary inspection, records the
  release-root `.dockerignore` digest, and records each component Dockerfile
  digest before promotion evidence can be accepted. Image metadata evidence is
  recorded as artifact-local filenames with Docker `sha256:<digest>` image IDs
  rather than local runner filesystem paths. Tag refs, boundary-inspection refs,
  and immutable refs must all use the same component repository. GitHub Actions
  builder records must carry complete run identity and match the release git
  SHA; local manifests must not claim GitHub identity fields. When the release
  archive or directory is provided, the verifier also proves the image manifest
  release digest, `.dockerignore` digest, and per-component Dockerfile digests
  match that exact release artifact. When adjacent buildx metadata and IID
  files are present, the verifier ties their recorded registry digest and image
  IDs back to the manifest.
- `scripts/release/write-cloud-release-provenance.sh` writes unsigned recorded
  provenance for release archives and optional image build manifests.
- `scripts/release/verify-cloud-release-provenance.sh` verifies release,
  material, builder, and optional image digest metadata while requiring the
  signature status to remain explicitly `unsigned`. When image-build
  provenance is present, it must include local image-boundary verification
  evidence, Dockerfile digests, and image build-context digest evidence for
  every deployable component. Material paths and optional image-manifest paths
  are artifact-local, not absolute runner paths. GitHub Actions builder records
  must carry complete run identity and match the release git SHA; local builder
  records must not claim GitHub identity fields. When the release archive or
  directory is provided, the verifier also proves the provenance archive digest,
  manifest digest, release identity, and material metadata match that exact
  release artifact.
- `docs/specs/CLOUD_PATH2_SIGNING_POLICY.json` records the current signing
  policy: Core provenance, Cloud provenance, and the release asset index may be
  unsigned until a real signing boundary exists, and premature `signed`,
  `verified`, `keyless`, or `cosign` status claims are forbidden.
- `scripts/release/verify-cloud-signing-policy.sh` validates that signing
  policy and lists the concrete blockers that must be resolved before signed
  provenance can be accepted.
- `scripts/release/write-gcp-image-tfvars.sh` converts a verified pushed image
  build manifest into digest-addressed GCP Terraform image inputs for API,
  pool-controller, and migrations from one Artifact Registry repository family,
  then verifies the generated file.
- `scripts/release/verify-gcp-image-tfvars.sh` verifies that a GCP image tfvars
  handoff contains exactly the expected digest refs from a pushed image manifest
  and no extra Terraform variables. Worker images remain build evidence and are
  explicitly rejected as deploy tfvars inputs.
- `scripts/release/verify-gcp-image-promotion-evidence.sh` verifies that the
  pushed image manifest, image-build provenance, and GCP tfvars all describe the
  same digest-addressed promotion input set before handoff. It independently
  checks the Terraform handoff against both manifest and provenance immutable
  refs, requires one Artifact Registry repository family, and keeps worker
  images out of promotion tfvars.
- `scripts/release/write-cloud-image-promotion-evidence-index.sh` writes
  `cloud-image-promotion-evidence.json` for pushed image handoff evidence after
  verifying the complete manifest/provenance/tfvars bundle.
- `scripts/release/verify-cloud-image-promotion-evidence-index.sh` validates
  that image promotion evidence index, checks the manifest/provenance/tfvars
  file digests when present, re-runs the complete promotion verifier, and keeps
  the index explicitly unsigned until signing is configured. The index must
  contain exactly the pushed image manifest, image-build provenance, and tfvars
  evidence entries, and it records the single Artifact Registry repository
  family used by deployable image refs.
- `scripts/ci/verify-cloud-release-boundary.sh` is part of
  `scripts/ci/cloud-gates.sh` and checks that release workflow/image definitions
  do not regress to retired deploy payloads, mutable `latest` inputs, direct
  Docker push/login commands, unchecked promotion, or baked runtime
  configuration.
- `.github/workflows/bucephalus-cloud-ci.yml` also runs the release-boundary
  policy as an independent job so Path 2 regressions stay visible if unrelated
  Rust or Cloud tests fail first.
- Release and Cloud CI workflows pin Bun setup to `1.3.14` instead of floating
  on `latest`.
- The release workflow has an explicit manual image-build smoke option for the
  Linux x86_64 cloud bundle and defaults to the approved digest-pinned Bun base
  image. A separate `push_images` switch controls registry publication.
- Release input validation fails early when pushed image publication is
  requested without image builds or when image builds omit a digest-addressed
  Bun base image.
- `scripts/release/verify-cloud-image-publish-inputs.sh` validates image
  publication inputs and requires pushed images to target the declared
  first-cloud GCP Artifact Registry repository-prefix shape rather than Docker
  Hub, localhost, examples, tags, digests, URLs, or the local smoke default. The
  Bun base image input must be digest-addressed and must not use a `latest` tag
  even when a digest is present.
- The release workflow defaults pushed images to
  `us-central1-docker.pkg.dev/gen-lang-client-0255842044/buc-bucephalus-cloud/bucephalus-cloud`,
  matching the first GCP substrate's Terraform repository name.
- `.github/workflows/bucephalus-release.yml` authenticates pushed image
  publication through Google Workload Identity using
  `google-github-actions/auth@v3`, installs gcloud with
  `google-github-actions/setup-gcloud@v3`, and grants `id-token: write` only to
  the Linux release job that can publish images.
- `scripts/release/configure-gcp-artifact-registry-auth.sh` configures Docker
  for the target GCP Artifact Registry hostname with the gcloud credential
  helper, verifies the active gcloud account matches the declared publisher
  service account, rejects static credential surfaces, allows only the
  `gha-creds-*.json` ADC file generated by `google-github-actions/auth`, and
  sets `BUCEPHALUS_GCP_REGISTRY_AUTH_READY=true`.
- `scripts/release/verify-cloud-registry-auth-boundary.sh` validates the
  workload-identity provider, publisher service account, Artifact Registry
  repository prefix, and ready marker before pushed buildx publication. Local
  image inspection still requires no registry auth.
- GCP image tfvars are generated only for pushed image manifests with immutable
  digest refs from one Artifact Registry repository family, not for local image
  inspection manifests or worker image evidence.
- Pushed-image workflow artifacts use a dedicated
  `cloud-image-promotion-evidence-<target>` bundle containing the pushed image
  manifest, image-build provenance, generated tfvars, and
  `cloud-image-promotion-evidence.json` self-hashed checksum index before
  deploy-side promotion consumes them.
- The GCP deploy workflow supports a substrate-only stage with
  `deploy_control_plane_services=false`, allowing Artifact Registry and other
  durable substrate resources to be created before any real image digests exist.
  Full service promotion later sets `deploy_control_plane_services=true` and
  consumes only verified `gcp-image-digests.tfvars`.
- `scripts/deploy/bootstrap-gcp-github-oidc.sh` is the audited one-time
  bootstrap for release/deploy Workload Identity: it is dry-run by default and
  can enable prerequisite APIs, create the Terraform state bucket, create
  publisher/deployer service accounts, create the GitHub OIDC provider, grant
  IAM roles, and set the GitHub Actions secrets consumed by release/deploy
  workflows when run with `--apply`.
- The tagged GitHub Release publication path attaches `release-assets.json`
  alongside archive, checksum, and provenance assets.
- Release workflow Actions artifacts are explicitly retained for 30 days as
  intermediate handoff evidence. Durable references for rollback and promotion
  are GitHub Release assets, registry digests, and provenance records, not
  default-retained CI artifact storage.
- The first GCP Artifact Registry substrate configures cleanup only for
  untagged image versions older than 30 days, preserving tagged/digest-promoted
  rollback references.

## Still Needed For End-To-End Readiness

These items require user decisions, cloud provisioning, or another path's deploy
contract before Path 2 can be proven end to end:

- Provision the real Google Workload Identity provider, publisher service
  account, Artifact Registry IAM binding, and GitHub secrets consumed by the
  release workflow, then run the pushed path to prove Docker/GAR auth against
  the declared registry. `scripts/deploy/bootstrap-gcp-github-oidc.sh` now
  dry-runs the required setup. Needed input: permission to run it with `--apply`
  for the selected GCP project/repository.
- Add production runner-provider helper image content, if Path 3 requires
  provider-owned binaries beyond the Cloud worker and Core CLI. Needed input:
  whether Path 3 runner provisioning needs a separate provider/helper image or
  the current worker image is the only Path 2 image boundary.
- Run the pushed image build path against the declared registry and verify the
  resulting `cloud-image-build-manifest.json` with immutable refs for all four
  Cloud images. `latest` and mutable tags may exist for operator convenience,
  but must not be deploy inputs. Needed input: approval to run the
  `workflow_dispatch` release with `build_images=true`, `push_images=true`,
  the approved base image, and the declared GAR repository prefix.
- Sign release provenance and pushed image provenance at the registry or deploy
  promotion boundary. Current provenance is recorded and verified, but
  intentionally unsigned under `CLOUD_PATH2_SIGNING_POLICY.json`. Needed input:
  signing mechanism and identity policy, for example keyless OIDC issuer/subject
  constraints or managed key material location, plus where signatures and
  attestations are stored.
- Run the CI image-build smoke in a Linux environment with an approved
  digest-pinned base image so the existing image boundary verifier is exercised
  against real local images, not only scripts and fixtures. Needed input: the
  approved base ref and permission to run a Linux image-build workflow with
  Docker buildx available.
- Decide whether release archives are published only to GitHub Releases, only to
  cloud object storage, or to both. The chosen destination needs an immutable
  object key and checksum/provenance record. Needed input: release archive
  storage policy and, if object storage is used, bucket/path/IAM decisions.
- Provision and run the deploy-side Path 1 workflow with real credentials:
  `.github/workflows/bucephalus-gcp-deploy.yml` now supports substrate-only
  bootstrap, then consumes the pushed image promotion evidence artifact, renders
  non-secret deploy tfvars, applies via a remote GCS Terraform backend, runs the
  migration job, and smokes user plus worker auth. Needed input: deploy Workload
  Identity provider/service account, Terraform backend bucket/prefix,
  API-created runner pool ID, numeric Secret Manager versions, smoke identity
  tokens, and approval to run `substrate-apply` followed by full `apply`.
- Revisit registry-side retention after a real pushed image run and production
  rollback window are chosen. The current Terraform policy deletes only
  untagged images older than 30 days and intentionally preserves tagged
  promotion/rollback refs. Needed input: required rollback window and whether
  promotion tags are retained separately from immutable digest refs.
- Add human-readable release attestation review steps for production promotion:
  verify checksums, verify provenance identity, verify image digests, verify no
  secrets or environment-specific values are present, then approve promotion.
  Needed input: who approves production promotion and where that approval is
  recorded.

## Known Non-Path-2 Gate Failure

`scripts/ci/cloud-gates.sh` currently fails before Cloud typecheck/tests when
the Rust test suite runs. The failure is not in the Path 2 release/image
boundary scripts; focused Path 2 gates pass. The failing Rust tests are runtime
persistence/control-plane tests with stale shared state symptoms, such as
unexpected existing SQLite evidence rows, previously committed schedule slots,
and poisoned runtime-control test locks. This must be resolved before the full
release gate can be green, but it belongs to the runtime/control-plane test
surface rather than the artifact, image, checksum, or digest-publishing
boundary.

## Explicit Non-Readiness

The system is not yet ready for production-shaped end-to-end deployment because
the implemented registry and deploy OIDC/auth boundaries have not been
provisioned and proven against the real GCP registry/project, there are no
digest-published production Cloud or runner images, no signed provenance
attestation, and no successful deploy workflow run consuming a chosen pushed
image promotion evidence artifact. Those are real boundary gaps, not
local-workflow problems to hide with SSH, startup scripts, mutable tags, or
checked-in env examples.
