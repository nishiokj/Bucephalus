# Distribution & Packaging

How Bucephalus is built, packaged, and released. The README covers user-facing
installation and first-run flow; this document is the packaging boundary.

## Public install

The public installer is the only command users should need:

```bash
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | sh
```

It detects macOS/Linux plus arm64/x86_64, downloads the matching GitHub Release
archive, verifies the archive checksum, and installs `bucephalus` into
`$HOME/.local/bin` unless `BUCEPHALUS_INSTALL_DIR` is set.
The sibling checksum file must contain exactly one record in the release format:
`<lowercase-sha256>  <archive-name>`.

Install a specific release with:

```bash
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | env BUCEPHALUS_VERSION=0.3.1 sh
```

From a checkout, the source install remains:

```bash
cargo install --path .
```

The legacy `lab` executable is still installed as a compatibility alias when
installing from Cargo. Public release archives ship only the `bucephalus`
binary.

## Build from source

```bash
cargo build --release --bin bucephalus
./target/release/bucephalus --help
```

Useful checks:

```bash
cargo check
cargo package --allow-dirty
```

`cargo package` is the registry boundary check: it verifies the crate can be
unpacked and built from only the files that would ship to crates.io.

## Distribution shape

Bucephalus ships as one publishable Rust crate:

```text
package: bucephalus-cli
binary:  bucephalus
alias:   lab
formula: bucephalus
```

The crate root is the repository root. The Rust implementation stays under
`rust/crates/*`, but those directories are a private source layout, not
separately published crates.

The analysis commands (`bucephalus views`, `bucephalus query`) read the account
SQLite database directly through the bundled `rusqlite` engine, so they are part
of the default build — there is no separate analytics feature or binary.

## Core release artifacts

GitHub tag releases attach prebuilt core CLI archives for macOS and Linux:

```text
bucephalus-aarch64-apple-darwin.tar.gz
bucephalus-x86_64-apple-darwin.tar.gz
bucephalus-x86_64-unknown-linux-gnu.tar.gz
bucephalus-aarch64-unknown-linux-gnu.tar.gz
<archive>.sha256
```

Each archive contains `bucephalus`, `README.md`, `LICENSE`,
`release-manifest.json`, and `SHA256SUMS`. Build one locally with:

```bash
scripts/release/build-core-release.sh --version 0.3.1 --target aarch64-apple-darwin
```

Verify the archive and write recorded provenance with:

```bash
scripts/release/verify-core-release.sh dist/releases/bucephalus-<target>.tar.gz
scripts/release/write-core-release-provenance.sh \
  --release dist/releases/bucephalus-<target>.tar.gz \
  --out dist/releases/bucephalus-<target>.provenance.json
scripts/release/verify-core-release-provenance.sh \
  dist/releases/bucephalus-<target>.provenance.json \
  --release dist/releases/bucephalus-<target>.tar.gz
```

The Homebrew formula can consume the same archive for its target, verify SHA256,
and install `bucephalus`.

## Cloud runner release artifacts

Bucephalus Cloud uses a larger release bundle because runner VMs need both the
Core binary and the Cloud worker/controller code. Build it with:

```bash
scripts/release/build-buc-release.sh --version 0.3.1
```

The Linux release workflow builds the provider-facing shape for
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`:

```text
dist/releases/bucephalus-<version>-<target>/
  bin/bucephalus
  release-inputs/
    Cargo.lock
  bucephalus-cloud/
    src/
    api/openapi/
    db/migrations/
    images/
    docker-compose.yml
    deploy/README.md
    infra/gcp/
    package.json
    bun.lock
    tsconfig.json
  release-manifest.json
  SHA256SUMS
```

The matching archive is:

```text
bucephalus-<version>-<target>.tar.gz
bucephalus-<version>-<target>.tar.gz.sha256
bucephalus-<version>-<target>.provenance.json
```

`release-manifest.json` records the git revision, dirty bit, target, core
binary checksum, root `Cargo.lock` checksum, Cloud `bun.lock` checksum, Cloud
`package.json` checksum, and tree digests for Cloud source, migrations, and
OpenAPI specs, and image definitions. It also records the release-root
`.dockerignore` used as the Cloud image build-context guard. `SHA256SUMS` covers
every bundled file except itself with exact `<lowercase-sha256>  <relative-path>`
records. Verify a bundle before publishing or promoting it with:

```bash
scripts/release/verify-buc-release.sh dist/releases/bucephalus-<version>-<target>.tar.gz
```

The verifier rejects checksum drift, malformed bundled checksum records,
incomplete bundled checksum coverage, malformed release metadata, non-Markdown
retired deploy payloads under `bucephalus-cloud/deploy/`, and
`.env`/`.env.example` files in the release artifact. This keeps the release
bundle as code and metadata, not a secret or local deployment store.

The release-root `.dockerignore` excludes generated Google Actions credential
files, env files, Terraform state, dependency folders, and image-build metadata
from Docker build contexts. The release verifier and image builder both require
those exclusions before an image build can run.

Build Cloud API, pool-controller, migration, and worker images from a verified
release bundle with:

```bash
scripts/release/build-cloud-images.sh \
  --release dist/releases/bucephalus-<version>-<target>.tar.gz \
  --repository <image-repository-prefix> \
  --base-image <bun-base-image@sha256:...>
```

The base image is required to be digest-addressed and must not use a `latest`
tag, even when a digest is present. Without `--push`, the images are loaded into
the local Docker daemon and inspected for release labels and forbidden baked
runtime configuration. Local image tags are not deploy inputs. When registry
publishing is wired, use `--push` and promote only the resulting
`image@sha256:<digest>` references recorded in
`cloud-image-build-manifest.json`. Pushed builds perform local image-boundary
inspection before publication, so a registry digest is not recorded unless the
same component build has passed the no-runtime-config/no-secret metadata check.

Base image approval is tracked in
`bucephalus-cloud/images/base-image-policy.json` and verified with:

```bash
scripts/release/verify-cloud-base-image-policy.sh \
  --base-image <bun-base-image@sha256:...> \
  --push
```

Local image inspection only requires a digest-addressed, non-`latest`, non-URL
base. Pushed image publication requires the base digest to be present in
`approved_base_images`. The first approved production base is:

```text
oven/bun@sha256:e10577f0db68676a7024391c6e5cb4b879ebd17188ab750cf10024a6d700e5c4
```

That digest was resolved from `oven/bun:1.3.14` through the Docker Hub Registry
API and the policy records the registry response plus Linux amd64/arm64 child
manifest evidence. Future base refreshes require a policy update and verifier
pass before pushed publication can use the new digest.

In the GitHub release workflow, `build_images: true` performs the verified
Linux x86_64 image build/inspection path. `push_images: true` adds registry
publication and is the only mode that produces promotion-ready image digest
tfvars. `push_images` requires `build_images`, and image builds require a
digest-addressed `bun_base_image`. The workflow defaults `bun_base_image` to
the approved `oven/bun@sha256:e10577f0db68676a7024391c6e5cb4b879ebd17188ab750cf10024a6d700e5c4`
base and defaults `image_repository` to
`us-central1-docker.pkg.dev/gen-lang-client-0255842044/buc-bucephalus-cloud/bucephalus-cloud`
for the first GCP environment. Registry authentication must already be
available to the workflow through GitHub OIDC and Google Workload Identity; the
workflow does not invent static registry credentials. Pushed publication also
requires `image_repository` to use the first-cloud GCP Artifact Registry prefix
shape:

```text
<location>-docker.pkg.dev/<project>/<repository>/<image-prefix>
```

Local image inspection may use a throwaway repository prefix, but pushed images
are rejected when the destination is Docker Hub, localhost, the default smoke
prefix, an example path, a URL, a tag, or a digest.

Pushed publication also has an explicit registry authentication preflight:

```bash
scripts/release/verify-cloud-registry-auth-boundary.sh \
  --repository <location>-docker.pkg.dev/<project>/<repository>/<image-prefix> \
  --push
```

The release workflow authenticates pushed image publication with
`google-github-actions/auth@v3`, installs gcloud with
`google-github-actions/setup-gcloud@v3`, and then runs:

```bash
scripts/release/configure-gcp-artifact-registry-auth.sh \
  --repository <location>-docker.pkg.dev/<project>/<repository>/<image-prefix>
```

That script rejects static credential surfaces. A temporary
`GOOGLE_APPLICATION_CREDENTIALS` file is allowed only when it is the
`gha-creds-*.json` file generated and marked by `google-github-actions/auth`;
manual service-account key paths are rejected. The script configures Docker with
the gcloud Artifact Registry credential helper for the repository hostname and
declares `BUCEPHALUS_GCP_REGISTRY_AUTH_READY=true`. The image build script
requires that ready marker before any pushed buildx invocation.

Validate an image build manifest with:

```bash
scripts/release/verify-cloud-image-build-manifest.sh \
  dist/releases/cloud-image-build-<target>/cloud-image-build-manifest.json
```

For `pushed: true` manifests, this verifier requires digest-addressed immutable
refs for API, pool-controller, migrations, and worker images under the declared
GCP Artifact Registry prefix, with component repositories ending in `/api`,
`/pool-controller`, `/migrations`, and `/worker`. A local `pushed: false`
manifest is inspection evidence only and must not be used as a deploy promotion
input. Every manifest entry must record
`boundary_verified: true` and the local inspection image evidence used before
publication. The manifest also records the release-root `.dockerignore` digest
and each component Dockerfile digest so a promoted image digest can be traced
back to the exact image build materials. Image metadata evidence uses
artifact-local filenames and Docker `sha256:<digest>` image IDs, not local
runner filesystem paths. The component repository, mutable tag used for the
build, local boundary-inspection tag, and immutable digest ref must all agree on
the same component repository. Builder identity follows the same rule as
release provenance: GitHub Actions records must carry complete run identity and
match the release git SHA, while local records must not carry GitHub identity
fields. When a release archive or directory is supplied with `--release`, the
manifest verifier also proves the release manifest digest, `.dockerignore`
digest, and per-component Dockerfile digests against that exact release
artifact. When the buildx metadata and IID files are present next to the image
manifest, the verifier also checks their recorded registry digest and Docker
image IDs against the manifest.

Write recorded release provenance with:

```bash
scripts/release/write-cloud-release-provenance.sh \
  --release dist/releases/bucephalus-<version>-<target>.tar.gz \
  --out dist/releases/bucephalus-<version>-<target>.provenance.json
```

When image publishing is wired, include the pushed image build manifest:

```bash
scripts/release/write-cloud-release-provenance.sh \
  --release dist/releases/bucephalus-<version>-<target>.tar.gz \
  --image-manifest dist/releases/cloud-image-build-<target>/cloud-image-build-manifest.json \
  --out dist/releases/cloud-image-build-<target>/cloud-image-build.provenance.json
```

The provenance verifier checks release and material digests, optional image
digests, image build-context and Dockerfile digests, image-boundary inspection
evidence, builder context, and that the record remains explicitly `unsigned`
until a registry or deploy-promotion signing boundary exists. Material paths and
optional image-build manifest paths in provenance are artifact-local paths, not
runner filesystem locations. GitHub Actions builder records must include the
run identity and match the release git SHA; local records must not carry GitHub
identity fields. When a release archive or directory is provided with
`--release`, the verifier also proves that the provenance archive digest,
manifest digest, release identity, and material metadata match that exact
artifact.

The signing boundary is tracked in
`docs/specs/CLOUD_PATH2_SIGNING_POLICY.json` and verified with:

```bash
scripts/release/verify-cloud-signing-policy.sh
```

Current records are allowed to be unsigned only for the documented Core release,
Cloud release, and release asset index schemas. A future change that sets
`signature.status` to a signed state must first declare the signing identity,
issuer/subject policy, signature material location, CI verifier, and deploy
promotion verification path.

For the GCP substrate, derive the image input tfvars from a pushed image build
manifest:

```bash
scripts/release/write-gcp-image-tfvars.sh \
  --image-manifest dist/releases/cloud-image-build-<target>/cloud-image-build-manifest.json \
  --out dist/releases/cloud-image-build-<target>/gcp-image-digests.tfvars
```

The generated file contains only real digest-addressed `api_image_digest`,
`pool_controller_image_digest`, and `migration_image_digest` values. It is a
promotion input, not an apply step, and it refuses local `pushed: false` image
manifests or all-zero placeholder digests. Those three refs must come from the
same GCP Artifact Registry repository family, and worker image refs are
intentionally excluded from the Terraform handoff.

Verify an existing tfvars handoff against its pushed image manifest with:

```bash
scripts/release/verify-gcp-image-tfvars.sh \
  --image-manifest dist/releases/cloud-image-build-<target>/cloud-image-build-manifest.json \
  --tfvars dist/releases/cloud-image-build-<target>/gcp-image-digests.tfvars
```

The verifier rejects missing, duplicate, unexpected, non-digest, mismatched,
cross-family, or worker image Terraform variables so deploy promotion does not
consume a stale or edited image input fragment.

Before handing image inputs to a deploy workflow, verify the complete promotion
evidence bundle:

```bash
scripts/release/verify-gcp-image-promotion-evidence.sh \
  --image-manifest dist/releases/cloud-image-build-<target>/cloud-image-build-manifest.json \
  --image-provenance dist/releases/cloud-image-build-<target>/cloud-image-build.provenance.json \
  --tfvars dist/releases/cloud-image-build-<target>/gcp-image-digests.tfvars
```

This ties the pushed image manifest, unsigned image-build provenance, and GCP
tfvars handoff together. It rejects local manifests, mismatched image digests,
stale tfvars, cross-family Terraform image refs, worker image promotion inputs,
missing boundary-inspection evidence, and provenance that claims signing before
the signing boundary exists.

Write and verify the pushed-image promotion evidence index with:

```bash
scripts/release/write-cloud-image-promotion-evidence-index.sh \
  --image-manifest dist/releases/cloud-image-build-<target>/cloud-image-build-manifest.json \
  --image-provenance dist/releases/cloud-image-build-<target>/cloud-image-build.provenance.json \
  --tfvars dist/releases/cloud-image-build-<target>/gcp-image-digests.tfvars \
  --out dist/releases/cloud-image-build-<target>/cloud-image-promotion-evidence.json
scripts/release/verify-cloud-image-promotion-evidence-index.sh \
  dist/releases/cloud-image-build-<target>/cloud-image-promotion-evidence.json
```

`cloud-image-promotion-evidence.json` records the SHA256 digests of the pushed
image manifest, unsigned image-build provenance, and generated tfvars handoff.
It records exactly those three evidence entries plus the single GCP Artifact
Registry repository family used by API, pool-controller, and migration image
refs. When those files are present, the verifier rechecks their digests and
reruns the complete promotion evidence verifier. The index is also unsigned
until the same signing boundary that covers release and image provenance exists.

The release workflow uploads pushed-image handoff files as one
`cloud-image-promotion-evidence-<target>` Actions artifact containing the image
manifest, image-build provenance, generated tfvars, and promotion evidence
index. Local image inspection artifacts do not include tfvars or promotion
inputs.

Deploy to the first GCP substrate through
`.github/workflows/bucephalus-gcp-deploy.yml`. The workflow supports a
substrate-only mode before real image digests exist:

```text
terraform_action=substrate-apply
```

That mode writes `deploy_control_plane_services = false`, applies the durable
substrate through a remote GCS backend, and creates prerequisites such as
Artifact Registry without accepting placeholder image digests. After the release
workflow pushes real images and uploads promotion evidence, run the deploy
workflow in digest promotion mode. It takes a release workflow run ID and
promotion evidence artifact name, downloads the pushed-image handoff, verifies
`cloud-image-promotion-evidence.json`, writes non-secret deploy tfvars with
`scripts/deploy/write-gcp-deploy-tfvars.sh`, and runs Terraform with a remote
GCS backend:

The deploy workflow defaults to project `gen-lang-client-0255842044`, region
`us-central1`, Terraform state bucket
`gen-lang-client-0255842044-bucephalus-tfstate`, state prefix
`bucephalus-cloud/bucephalus`, environment `bucephalus`, resource prefix `buc`,
and user OAuth client ID
`380690977483-3a2n2ttkbf352kmn3tl7qgr5ir1f34c3.apps.googleusercontent.com`.

```text
terraform plan \
  -var-file=<generated-deploy.tfvars> \
  -var-file=<downloaded-gcp-image-digests.tfvars>
```

The workflow does not accept `api_image_digest`,
`pool_controller_image_digest`, or `migration_image_digest` as manual inputs.
Those values must come from the verified `gcp-image-digests.tfvars` artifact.
On `apply`, the workflow applies the selected digest promotion, executes the
Cloud Run migration job, and smokes `/readyz`, a user-authenticated API request,
and a worker-authenticated API request. Missing Workload Identity, Terraform
backend access, smoke identity tokens, or GCP IAM policy should stop the deploy
cleanly.

Bootstrap the GitHub/GCP OIDC boundary before running either workflow:

```bash
scripts/deploy/bootstrap-gcp-github-oidc.sh \
  --project-id <gcp-project> \
  --repository <owner/repo>
```

The script is dry-run by default. With `--apply`, it enables the prerequisite
APIs, creates the Terraform state bucket, creates publisher/deployer service
accounts, creates the GitHub Workload Identity pool/provider, grants scoped IAM
roles, and writes the GitHub Actions secrets consumed by the release and deploy
workflows.

The GitHub Release publish job writes a release-wide asset index after
downloading the Core and Cloud artifacts:

```bash
scripts/release/write-release-asset-index.sh \
  --assets-dir dist/core-assets \
  --assets-dir dist/cloud-assets \
  --out dist/release-assets.json
scripts/release/verify-release-asset-index.sh dist/release-assets.json
```

`release-assets.json` records every attached archive, its sibling checksum, and
its unsigned provenance digest. The writer and verifier require each sibling
checksum file to contain exactly the archive's single checksum record. The
writer verifies each Core or Cloud archive through the existing archive and
provenance verifiers before emitting the index. In the tagged release workflow,
the writer also receives the required Core and Cloud target matrix and rejects a
partial or extra target set before `release-assets.json` is attached to the
GitHub Release alongside the archive assets. Indexed archive, checksum,
provenance, and asset-root paths must be artifact-local paths. Expanded release
directories are not publishable assets.

GitHub Actions upload artifacts in the release workflow are intermediate handoff
evidence and use explicit 30-day retention. Durable rollback and promotion
references must come from GitHub Release assets, registry immutable refs, and
recorded provenance, not from long-lived Actions artifact storage.

The first GCP Artifact Registry substrate configures cleanup only for untagged
image versions older than 30 days. Tagged/digest-promoted images are retained by
that policy so cleanup does not erase known-good rollback references. Promotion
must not consume untagged images.

Cloud deployments should consume immutable release artifacts and images, not a
live checkout. The old local-VM, SSH, systemd, startup-script, and handwritten
provider deployment materials are retired. The replacement target is described
in `docs/specs/CLOUD_DEPLOYMENT_GOAL_STATE.md`: infrastructure declares the
cloud substrate, CI publishes digest-addressable artifacts and images, deploy
promotes selected digests and runs migrations with a scoped identity, and runner
execution is mediated through the Cloud API.

## CI/CD

The Cloud/Core CI gate is:

```bash
scripts/ci/cloud-gates.sh
```

It checks Rust formatting and tests, Cloud typecheck and tests, OpenAPI YAML
parseability plus local `$ref` targets, and Postgres migrations when
`DATABASE_URL` is set. It also runs
`scripts/ci/verify-cloud-release-boundary.sh`, which fails if the release
workflow or image definitions drift back toward retired deploy payloads,
mutable `latest` inputs, direct Docker push/login commands, unchecked image
promotion, or baked runtime configuration. Release and Cloud CI setup pins Bun
to a concrete version instead of `latest`. The Cloud CI workflow also runs this
policy as a separate `Release boundary policy` job so artifact/image boundary
regressions remain visible even when broader Rust or Cloud tests fail.

Official release builds require a clean git worktree. Local smoke builds may set
`BUCEPHALUS_RELEASE_ALLOW_DIRTY=true`. The release workflow verifies the exact
Linux cloud release archive before uploading it as a GitHub Actions artifact.

GitHub Actions wires this into:

- `.github/workflows/bucephalus-cloud-ci.yml`: PR and main CI for Core + Cloud.
- `.github/workflows/bucephalus-release.yml`: manual/tagged Linux release bundle
  build, uploaded as a GitHub Actions artifact.

## Repository boundary

This repository is the Rust product. Python harnesses, demos, generated runs,
and local experiment scratch do not ship.

The package includes:

```text
Cargo.toml
README.md
LICENSE
schemas/
rust/crates/*/src/
rust/crates/lab-analysis/views/
```

Ignored or removed from the product surface:

```text
.lab/
demos/
Python scripts and package metadata
legacy docs and generated benchmark artifacts
```
