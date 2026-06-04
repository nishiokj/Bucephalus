#!/usr/bin/env bash
set -euo pipefail

MANIFEST=""
RELEASE_INPUT=""
WORK_DIR=""
RELEASE_DIR=""
RELEASE_ARCHIVE_SHA=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-image-build-manifest.sh <cloud-image-build-manifest.json> [--release <release-dir-or-tar.gz>]

Validates the image build manifest emitted by build-cloud-images.sh. Local
non-pushed manifests are allowed for image inspection evidence. Pushed manifests
must contain immutable digest refs for every deployable component. When
--release is provided, also verifies that the manifest describes that exact
release artifact or directory.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      RELEASE_INPUT="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [[ -z "${MANIFEST}" ]]; then
        MANIFEST="$1"
        shift
      else
        echo "unknown argument: $1" >&2
        usage >&2
        exit 2
      fi
      ;;
  esac
done

if [[ -z "${MANIFEST}" ]]; then
  usage
  exit 2
fi

if [[ ! -f "${MANIFEST}" ]]; then
  echo "manifest does not exist: ${MANIFEST}" >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${file}" | awk "{print \$1}"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${file}" | awk "{print \$1}"
  else
    echo "sha256sum or shasum is required" >&2
    exit 2
  fi
}

cleanup() {
  if [[ -n "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

if [[ -n "${RELEASE_INPUT}" ]]; then
  if [[ -d "${RELEASE_INPUT}" ]]; then
    RELEASE_DIR="${RELEASE_INPUT}"
  elif [[ -f "${RELEASE_INPUT}" ]]; then
    if ! command -v tar >/dev/null 2>&1; then
      echo "required command not found: tar" >&2
      exit 2
    fi
    RELEASE_ARCHIVE_SHA="$(sha256_file "${RELEASE_INPUT}")"
    WORK_DIR="$(mktemp -d)"
    tar -xzf "${RELEASE_INPUT}" -C "${WORK_DIR}"
    RELEASE_DIR="$(find "${WORK_DIR}" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
    if [[ -z "${RELEASE_DIR}" ]]; then
      echo "archive did not contain a release directory: ${RELEASE_INPUT}" >&2
      exit 1
    fi
  else
    echo "release input does not exist: ${RELEASE_INPUT}" >&2
    exit 2
  fi
  if [[ ! -f "${RELEASE_DIR}/release-manifest.json" ]]; then
    echo "release input is missing release-manifest.json: ${RELEASE_INPUT}" >&2
    exit 1
  fi
fi

RELEASE_DIR="${RELEASE_DIR}" RELEASE_ARCHIVE_SHA="${RELEASE_ARCHIVE_SHA}" bun -e '
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";

const path = process.argv[1];
const manifest = JSON.parse(await Bun.file(path).text());
const manifestDir = dirname(path);
const releaseDir = process.env.RELEASE_DIR;
const releaseArchiveSha = process.env.RELEASE_ARCHIVE_SHA || null;
const sha256 = /^[a-f0-9]{64}$/;
const digest = /^sha256:[a-f0-9]{64}$/;
const digestRef = /^.+@sha256:[a-f0-9]{64}$/;
const zeroDigest = /^sha256:0{64}$/;
const zeroDigestRef = /^.+@sha256:0{64}$/;
const components = new Set(["api", "pool-controller", "migrations", "worker"]);
const garComponentRepo = /^[a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*\/(api|pool-controller|migrations|worker)$/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function hashFile(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function readOptionalJson(path, label) {
  if (!existsSync(path)) {
    return null;
  }
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`${label} must be valid JSON: ${error.message}`);
  }
}

function readOptionalText(path) {
  if (!existsSync(path)) {
    return null;
  }
  return readFileSync(path, "utf8").trim();
}

function checkBuilder(builder, release) {
  if (!builder || typeof builder !== "object") {
    fail("builder object is required");
  }
  if (!["local", "github_actions"].includes(builder.kind)) {
    fail("builder.kind must be local or github_actions");
  }
  const githubFields = [
    "github_server_url",
    "github_repository",
    "github_run_id",
    "github_run_attempt",
    "github_workflow",
    "github_ref",
    "github_sha",
  ];
  if (builder.kind === "local") {
    for (const field of githubFields) {
      if (builder[field] !== null) {
        fail(`local builder must not claim ${field}`);
      }
    }
    return;
  }
  if (builder.github_server_url !== "https://github.com") {
    fail("builder.github_server_url must be https://github.com");
  }
  if (typeof builder.github_repository !== "string" || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(builder.github_repository)) {
    fail("builder.github_repository must be an owner/repo slug");
  }
  if (typeof builder.github_run_id !== "string" || !/^[0-9]+$/.test(builder.github_run_id)) {
    fail("builder.github_run_id must be numeric");
  }
  if (typeof builder.github_run_attempt !== "string" || !/^[0-9]+$/.test(builder.github_run_attempt)) {
    fail("builder.github_run_attempt must be numeric");
  }
  if (typeof builder.github_workflow !== "string" || builder.github_workflow.trim() === "") {
    fail("builder.github_workflow is required for GitHub Actions image manifests");
  }
  if (typeof builder.github_ref !== "string" || !builder.github_ref.startsWith("refs/")) {
    fail("builder.github_ref must be a full refs/ value");
  }
  if (typeof builder.github_sha !== "string" || !/^[a-f0-9]{40}$/.test(builder.github_sha)) {
    fail("builder.github_sha must be a 40-character lowercase git object id");
  }
  if (builder.github_sha !== release.git_sha) {
    fail("builder.github_sha must match release.git_sha");
  }
}

if (manifest.schema_version !== "bucephalus_cloud_image_build_manifest_v1") {
  fail("schema_version must be bucephalus_cloud_image_build_manifest_v1");
}
if (!manifest.release || typeof manifest.release !== "object") {
  fail("release object is required");
}
if (typeof manifest.release.version !== "string" || manifest.release.version.trim() === "") {
  fail("release.version is required");
}
if (typeof manifest.release.target !== "string" || !manifest.release.target.includes("linux")) {
  fail("release.target must be a Linux target");
}
if (typeof manifest.release.git_sha !== "string" || !/^[a-f0-9]{40}$/.test(manifest.release.git_sha)) {
  fail("release.git_sha must be a 40-character lowercase git object id");
}
if (typeof manifest.release.manifest_sha256 !== "string" || !sha256.test(manifest.release.manifest_sha256)) {
  fail("release.manifest_sha256 must be a lowercase sha256 digest");
}
if (manifest.release.archive_sha256 !== null && (typeof manifest.release.archive_sha256 !== "string" || !sha256.test(manifest.release.archive_sha256))) {
  fail("release.archive_sha256 must be null or a lowercase sha256 digest");
}
if (typeof manifest.base_image !== "string" || !digestRef.test(manifest.base_image) || zeroDigestRef.test(manifest.base_image)) {
  fail("base_image must be a real digest-addressed image");
}
if (manifest.image_context?.path !== ".dockerignore") {
  fail("image_context.path must be .dockerignore");
}
if (typeof manifest.image_context.sha256 !== "string" || !sha256.test(manifest.image_context.sha256)) {
  fail("image_context.sha256 must be a lowercase sha256 digest");
}
if (typeof manifest.pushed !== "boolean") {
  fail("pushed must be boolean");
}
checkBuilder(manifest.builder, manifest.release);
if (!Array.isArray(manifest.images)) {
  fail("images must be an array");
}
if (manifest.images.length !== components.size) {
  fail(`images must contain exactly ${components.size} entries`);
}
const seen = new Set();
for (const image of manifest.images) {
  if (!components.has(image.component)) {
    fail(`unknown component: ${image.component}`);
  }
  if (seen.has(image.component)) {
    fail(`duplicate component: ${image.component}`);
  }
  seen.add(image.component);
  if (typeof image.image_repository !== "string" || image.image_repository.includes("@sha256:") || image.image_repository.endsWith(":latest")) {
    fail(`${image.component}.image_repository must be an untagged repository`);
  }
  if (typeof image.tag_ref !== "string" || image.tag_ref.includes("@sha256:") || image.tag_ref.endsWith(":latest")) {
    fail(`${image.component}.tag_ref must be a non-latest tag ref`);
  }
  if (!image.tag_ref.startsWith(`${image.image_repository}:`)) {
    fail(`${image.component}.tag_ref must use image_repository`);
  }
  if (typeof image.image_id !== "string" || !digest.test(image.image_id) || zeroDigest.test(image.image_id)) {
    fail(`${image.component}.image_id must be a sha256 image id and not an all-zero placeholder`);
  }
  if (image.boundary_verified !== true) {
    fail(`${image.component}.boundary_verified must be true`);
  }
  if (typeof image.boundary_image_ref !== "string" || image.boundary_image_ref.trim() === "" || image.boundary_image_ref.endsWith(":latest")) {
    fail(`${image.component}.boundary_image_ref must be a non-latest local inspection ref`);
  }
  if (!image.boundary_image_ref.startsWith(`${image.image_repository}:`)) {
    fail(`${image.component}.boundary_image_ref must use image_repository`);
  }
  if (typeof image.boundary_image_id !== "string" || !digest.test(image.boundary_image_id) || zeroDigest.test(image.boundary_image_id)) {
    fail(`${image.component}.boundary_image_id must be a sha256 image id and not an all-zero placeholder`);
  }
  const expectedMetadataFile = `${image.component}.metadata.json`;
  if (image.metadata_file !== expectedMetadataFile) {
    fail(`${image.component}.metadata_file must be ${expectedMetadataFile}`);
  }
  const expectedBoundaryMetadataFile = `${image.component}.boundary.metadata.json`;
  if (image.boundary_metadata_file !== expectedBoundaryMetadataFile) {
    fail(`${image.component}.boundary_metadata_file must be ${expectedBoundaryMetadataFile}`);
  }
  const expectedDockerfile = `bucephalus-cloud/images/Dockerfile.${image.component}`;
  if (image.dockerfile?.path !== expectedDockerfile) {
    fail(`${image.component}.dockerfile.path must be ${expectedDockerfile}`);
  }
  if (typeof image.dockerfile.sha256 !== "string" || !sha256.test(image.dockerfile.sha256)) {
    fail(`${image.component}.dockerfile.sha256 must be a lowercase sha256 digest`);
  }
  if (manifest.pushed) {
    if (!garComponentRepo.test(image.image_repository) || !image.image_repository.endsWith(`/${image.component}`)) {
      fail(`${image.component}.image_repository must be a GCP Artifact Registry component repository`);
    }
    if (typeof image.digest !== "string" || !digest.test(image.digest) || zeroDigest.test(image.digest)) {
      fail(`${image.component}.digest is required and must be a real registry digest for pushed manifests`);
    }
    if (typeof image.immutable_ref !== "string" || !digestRef.test(image.immutable_ref) || zeroDigestRef.test(image.immutable_ref)) {
      fail(`${image.component}.immutable_ref is required and must be a real digest ref for pushed manifests`);
    }
    if (!image.immutable_ref.startsWith(`${image.image_repository}@`)) {
      fail(`${image.component}.immutable_ref must use image_repository`);
    }
    if (image.boundary_image_ref !== `${image.tag_ref}-boundary-check`) {
      fail(`${image.component}.boundary_image_ref must be the pushed tag_ref boundary check`);
    }
  } else {
    if (image.immutable_ref !== null || image.digest !== null) {
      fail(`${image.component} must not claim registry digest data when pushed=false`);
    }
    if (image.boundary_image_ref !== image.tag_ref) {
      fail(`${image.component}.boundary_image_ref must equal tag_ref for local manifests`);
    }
  }
  const metadata = readOptionalJson(join(manifestDir, image.metadata_file), `${image.component}.metadata_file`);
  if (metadata !== null && metadata["containerimage.digest"] !== undefined) {
    if (metadata["containerimage.digest"] !== image.digest) {
      fail(`${image.component}.metadata_file containerimage.digest does not match manifest`);
    }
  } else if (metadata !== null && manifest.pushed) {
    fail(`${image.component}.metadata_file must record containerimage.digest for pushed manifests`);
  }
  const boundaryMetadata = readOptionalJson(join(manifestDir, image.boundary_metadata_file), `${image.component}.boundary_metadata_file`);
  if (boundaryMetadata?.["containerimage.digest"] !== undefined) {
    if (typeof boundaryMetadata["containerimage.digest"] !== "string" || !digest.test(boundaryMetadata["containerimage.digest"]) || zeroDigest.test(boundaryMetadata["containerimage.digest"])) {
      fail(`${image.component}.boundary_metadata_file containerimage.digest must be a real image digest`);
    }
    if (boundaryMetadata["containerimage.digest"] !== image.boundary_image_id) {
      fail(`${image.component}.boundary_metadata_file containerimage.digest does not match boundary_image_id`);
    }
  }
  const iid = readOptionalText(join(manifestDir, `${image.component}.iid`));
  if (iid !== null && iid !== image.image_id) {
    fail(`${image.component}.iid does not match manifest image_id`);
  }
  const boundaryIid = readOptionalText(join(manifestDir, `${image.component}.boundary.iid`));
  if (boundaryIid !== null && boundaryIid !== image.boundary_image_id) {
    fail(`${image.component}.boundary.iid does not match manifest boundary_image_id`);
  }
}
for (const component of components) {
  if (!seen.has(component)) {
    fail(`missing component: ${component}`);
  }
}
if (releaseDir) {
  const releaseManifestPath = join(releaseDir, "release-manifest.json");
  const releaseManifest = JSON.parse(readFileSync(releaseManifestPath, "utf8"));
  if (hashFile(releaseManifestPath) !== manifest.release.manifest_sha256) {
    fail("release.manifest_sha256 does not match release manifest");
  }
  if (releaseArchiveSha && manifest.release.archive_sha256 !== releaseArchiveSha) {
    fail("release.archive_sha256 does not match release archive");
  }
  for (const field of ["version", "target", "git_sha"]) {
    if (manifest.release[field] !== releaseManifest[field]) {
      fail(`release.${field} does not match release manifest`);
    }
  }
  if (hashFile(join(releaseDir, ".dockerignore")) !== manifest.image_context.sha256) {
    fail("image_context.sha256 does not match release .dockerignore");
  }
  for (const image of manifest.images) {
    if (hashFile(join(releaseDir, image.dockerfile.path)) !== image.dockerfile.sha256) {
      fail(`${image.component}.dockerfile.sha256 does not match release Dockerfile`);
    }
  }
}
console.log(`verified image build manifest ${path}`);
' "${MANIFEST}"
