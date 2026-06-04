#!/usr/bin/env bash
set -euo pipefail

PROVENANCE=""
RELEASE_INPUT=""
WORK_DIR=""
RELEASE_DIR=""
RELEASE_ARCHIVE_SHA=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-release-provenance.sh <cloud-release-provenance.json> [--release <release-dir-or-tar.gz>]

Validates unsigned Bucephalus Cloud release provenance. This checks structure,
release/material digests, optional image-build digest refs, and that the record
does not masquerade as a signed attestation. When --release is provided, also
verifies that the provenance describes that exact release artifact or directory.
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
      if [[ -z "${PROVENANCE}" ]]; then
        PROVENANCE="$1"
        shift
      else
        echo "unknown argument: $1" >&2
        usage >&2
        exit 2
      fi
      ;;
  esac
done

if [[ -z "${PROVENANCE}" ]]; then
  usage
  exit 2
fi

if [[ ! -f "${PROVENANCE}" ]]; then
  echo "provenance does not exist: ${PROVENANCE}" >&2
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
import { readFileSync } from "node:fs";
import { join } from "node:path";

const path = process.argv[1];
const provenance = JSON.parse(await Bun.file(path).text());
const releaseDir = process.env.RELEASE_DIR;
const releaseArchiveSha = process.env.RELEASE_ARCHIVE_SHA || null;
const sha256 = /^[a-f0-9]{64}$/;
const gitSha = /^[a-f0-9]{40}$/;
const digest = /^sha256:[a-f0-9]{64}$/;
const digestRef = /^.+@sha256:[a-f0-9]{64}$/;
const components = new Set(["api", "pool-controller", "migrations", "worker"]);
const garComponentRepo = /^[a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*\/(api|pool-controller|migrations|worker)$/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function checkSha(value, label) {
  if (typeof value !== "string" || !sha256.test(value)) {
    fail(`${label} must be a lowercase sha256 digest`);
  }
}

function checkArtifactPath(value, label) {
  if (typeof value !== "string" || value.trim() === "" || value.startsWith("/") || value.includes("..") || value.includes("\\")) {
    fail(`${label} must be a stable artifact-local path`);
  }
}

function recompute(record) {
  return createHash("sha256").update(JSON.stringify({
    ...record,
    provenance_sha256: null,
  })).digest("hex");
}
function hashFile(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
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
    fail("builder.github_workflow is required for GitHub Actions provenance");
  }
  if (typeof builder.github_ref !== "string" || !builder.github_ref.startsWith("refs/")) {
    fail("builder.github_ref must be a full refs/ value");
  }
  if (typeof builder.github_sha !== "string" || !gitSha.test(builder.github_sha)) {
    fail("builder.github_sha must be a 40-character lowercase git object id");
  }
  if (builder.github_sha !== release.git_sha) {
    fail("builder.github_sha must match release.git_sha");
  }
}

if (provenance.schema_version !== "bucephalus_cloud_release_provenance_v1") {
  fail("schema_version must be bucephalus_cloud_release_provenance_v1");
}
if (provenance.predicate_type !== "https://bucephalus.dev/provenance/cloud-release/v1") {
  fail("predicate_type is not recognized");
}
if (Number.isNaN(Date.parse(provenance.generated_at))) {
  fail("generated_at must be an ISO timestamp");
}
if (!provenance.release || typeof provenance.release !== "object") {
  fail("release object is required");
}
if (!["archive", "directory"].includes(provenance.release.input_kind)) {
  fail("release.input_kind must be archive or directory");
}
if (typeof provenance.release.version !== "string" || provenance.release.version.trim() === "") {
  fail("release.version is required");
}
if (typeof provenance.release.target !== "string" || provenance.release.target.trim() === "") {
  fail("release.target is required");
}
if (typeof provenance.release.git_sha !== "string" || !gitSha.test(provenance.release.git_sha)) {
  fail("release.git_sha must be a 40-character lowercase git object id");
}
if (typeof provenance.release.git_dirty !== "boolean") {
  fail("release.git_dirty must be boolean");
}
if (provenance.release.archive_sha256 !== null) {
  checkSha(provenance.release.archive_sha256, "release.archive_sha256");
}
checkSha(provenance.release.manifest_sha256, "release.manifest_sha256");

if (!provenance.materials || typeof provenance.materials !== "object") {
  fail("materials object is required");
}
for (const [name, entry] of Object.entries(provenance.materials.lockfiles ?? {})) {
  if (!entry || typeof entry.path !== "string") {
    fail(`materials.lockfiles.${name}.path is required`);
  }
  checkArtifactPath(entry.path, `materials.lockfiles.${name}.path`);
  checkSha(entry.sha256, `materials.lockfiles.${name}.sha256`);
}
for (const [name, entry] of Object.entries(provenance.materials.content_sets ?? {})) {
  if (!entry || typeof entry.path !== "string") {
    fail(`materials.content_sets.${name}.path is required`);
  }
  checkArtifactPath(entry.path, `materials.content_sets.${name}.path`);
  checkSha(entry.tree_sha256, `materials.content_sets.${name}.tree_sha256`);
}
if (provenance.materials.cloud_package !== null) {
  if (typeof provenance.materials.cloud_package.path !== "string") {
    fail("materials.cloud_package.path is required");
  }
  checkArtifactPath(provenance.materials.cloud_package.path, "materials.cloud_package.path");
  checkSha(provenance.materials.cloud_package.sha256, "materials.cloud_package.sha256");
}
if (provenance.materials.cloud_runtime_package !== null) {
  if (typeof provenance.materials.cloud_runtime_package.path !== "string") {
    fail("materials.cloud_runtime_package.path is required");
  }
  checkArtifactPath(provenance.materials.cloud_runtime_package.path, "materials.cloud_runtime_package.path");
  checkSha(provenance.materials.cloud_runtime_package.sha256, "materials.cloud_runtime_package.sha256");
}

if (releaseDir) {
  const manifestPath = join(releaseDir, "release-manifest.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (hashFile(manifestPath) !== provenance.release.manifest_sha256) {
    fail("release.manifest_sha256 does not match release manifest");
  }
  if (releaseArchiveSha && provenance.release.archive_sha256 !== releaseArchiveSha) {
    fail("release.archive_sha256 does not match release archive");
  }
  for (const field of ["version", "target", "git_sha", "git_dirty"]) {
    if (provenance.release[field] !== manifest[field]) {
      fail(`release.${field} does not match release manifest`);
    }
  }
  if (JSON.stringify(provenance.materials.lockfiles) !== JSON.stringify(manifest.source_inputs?.lockfiles ?? {})) {
    fail("materials.lockfiles do not match release manifest");
  }
  if (JSON.stringify(provenance.materials.cloud_package) !== JSON.stringify(manifest.source_inputs?.cloud_package ?? null)) {
    fail("materials.cloud_package does not match release manifest");
  }
  if (JSON.stringify(provenance.materials.cloud_runtime_package) !== JSON.stringify(manifest.source_inputs?.cloud_runtime_package ?? null)) {
    fail("materials.cloud_runtime_package does not match release manifest");
  }
  if (JSON.stringify(provenance.materials.content_sets) !== JSON.stringify(manifest.source_inputs?.content_sets ?? {})) {
    fail("materials.content_sets do not match release manifest");
  }
}

checkBuilder(provenance.builder, provenance.release);

if (provenance.image_build !== null) {
  checkArtifactPath(provenance.image_build.manifest_path, "image_build.manifest_path");
  checkSha(provenance.image_build.manifest_sha256, "image_build.manifest_sha256");
  if (typeof provenance.image_build.base_image !== "string" || !digestRef.test(provenance.image_build.base_image)) {
    fail("image_build.base_image must be digest-addressed");
  }
  if (provenance.image_build.image_context?.path !== ".dockerignore") {
    fail("image_build.image_context.path must be .dockerignore");
  }
  checkSha(provenance.image_build.image_context?.sha256, "image_build.image_context.sha256");
  if (typeof provenance.image_build.pushed !== "boolean") {
    fail("image_build.pushed must be boolean");
  }
  if (!Array.isArray(provenance.image_build.images) || provenance.image_build.images.length !== components.size) {
    fail("image_build.images must contain every deployable component");
  }
  const seen = new Set();
  for (const image of provenance.image_build.images) {
    if (!components.has(image.component)) {
      fail(`unknown image component: ${image.component}`);
    }
    if (seen.has(image.component)) {
      fail(`duplicate image component: ${image.component}`);
    }
    seen.add(image.component);
    if (image.boundary_verified !== true) {
      fail(`${image.component}.boundary_verified must be true`);
    }
    if (typeof image.boundary_image_ref !== "string" || image.boundary_image_ref.trim() === "" || image.boundary_image_ref.endsWith(":latest")) {
      fail(`${image.component}.boundary_image_ref must be a non-latest local inspection ref`);
    }
    if (typeof image.image_repository !== "string" || image.image_repository.includes("@sha256:") || image.image_repository.endsWith(":latest")) {
      fail(`${image.component}.image_repository must be an untagged repository`);
    }
    if (typeof image.tag_ref !== "string" || !image.tag_ref.startsWith(`${image.image_repository}:`) || image.tag_ref.includes("@sha256:") || image.tag_ref.endsWith(":latest")) {
      fail(`${image.component}.tag_ref must use image_repository`);
    }
    if (!image.boundary_image_ref.startsWith(`${image.image_repository}:`)) {
      fail(`${image.component}.boundary_image_ref must use image_repository`);
    }
    if (typeof image.image_id !== "string" || !digest.test(image.image_id)) {
      fail(`${image.component}.image_id must be a sha256 image id`);
    }
    if (typeof image.boundary_image_id !== "string" || !digest.test(image.boundary_image_id)) {
      fail(`${image.component}.boundary_image_id must be a sha256 image id`);
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
    checkSha(image.dockerfile?.sha256, `${image.component}.dockerfile.sha256`);
    if (image.timings_seconds !== null && image.timings_seconds !== undefined) {
      for (const field of ["build", "boundary_verify", "push", "total"]) {
        if (!Number.isFinite(image.timings_seconds[field]) || image.timings_seconds[field] < 0) {
          fail(`${image.component}.timings_seconds.${field} must be a non-negative number`);
        }
      }
    }
    if (provenance.image_build.pushed) {
      if (typeof image.image_repository !== "string" || !garComponentRepo.test(image.image_repository) || !image.image_repository.endsWith(`/${image.component}`)) {
        fail(`${image.component}.image_repository must be a GCP Artifact Registry component repository`);
      }
      if (typeof image.digest !== "string" || !digest.test(image.digest)) {
        fail(`${image.component}.digest is required for pushed image provenance`);
      }
      if (typeof image.immutable_ref !== "string" || !digestRef.test(image.immutable_ref)) {
        fail(`${image.component}.immutable_ref is required for pushed image provenance`);
      }
      if (!image.immutable_ref.startsWith(`${image.image_repository}@`)) {
        fail(`${image.component}.immutable_ref must use image_repository`);
      }
      if (image.boundary_image_ref !== `${image.tag_ref}-boundary-check`) {
        fail(`${image.component}.boundary_image_ref must be the pushed tag_ref boundary check`);
      }
    } else if (image.digest !== null || image.immutable_ref !== null) {
      fail(`${image.component} must not claim digest refs when image_build.pushed=false`);
    } else if (image.boundary_image_ref !== image.tag_ref) {
      fail(`${image.component}.boundary_image_ref must equal tag_ref for local image provenance`);
    }
  }
}

if (!provenance.signature || provenance.signature.status !== "unsigned") {
  fail("signature.status must be unsigned until a signing boundary exists");
}
checkSha(provenance.provenance_sha256, "provenance_sha256");
if (provenance.provenance_sha256 !== recompute(provenance)) {
  fail("provenance_sha256 does not match provenance content");
}
console.log(`verified release provenance ${path}`);
' "${PROVENANCE}"
