#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RELEASE_INPUT=""
IMAGE_MANIFEST=""
OUT_PATH=""
WORK_DIR=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/write-cloud-release-provenance.sh --release <release-dir-or-tar.gz> --out <path> [--image-manifest <cloud-image-build-manifest.json>]

Writes unsigned, recorded provenance for a Bucephalus Cloud release artifact.
The output is not a signature. It is a verifiable provenance record that can be
signed by the registry/promotion system once that boundary exists.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      RELEASE_INPUT="${2:-}"
      shift 2
      ;;
    --image-manifest)
      IMAGE_MANIFEST="${2:-}"
      shift 2
      ;;
    --out)
      OUT_PATH="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${file}" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${file}" | awk '{print $1}'
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

if [[ -z "${RELEASE_INPUT}" || -z "${OUT_PATH}" ]]; then
  usage >&2
  exit 2
fi

require_command bun
require_command tar

"${ROOT_DIR}/scripts/release/verify-buc-release.sh" "${RELEASE_INPUT}"

RELEASE_ARCHIVE_SHA=""
RELEASE_INPUT_KIND="directory"
if [[ -d "${RELEASE_INPUT}" ]]; then
  RELEASE_DIR="${RELEASE_INPUT}"
else
  RELEASE_INPUT_KIND="archive"
  RELEASE_ARCHIVE_SHA="$(sha256_file "${RELEASE_INPUT}")"
  WORK_DIR="$(mktemp -d)"
  tar -xzf "${RELEASE_INPUT}" -C "${WORK_DIR}"
  RELEASE_DIR="$(find "${WORK_DIR}" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
fi

if [[ -z "${RELEASE_DIR}" || ! -f "${RELEASE_DIR}/release-manifest.json" ]]; then
  echo "could not resolve release directory from ${RELEASE_INPUT}" >&2
  exit 1
fi

IMAGE_MANIFEST_SHA=""
if [[ -n "${IMAGE_MANIFEST}" ]]; then
  "${ROOT_DIR}/scripts/release/verify-cloud-image-build-manifest.sh" "${IMAGE_MANIFEST}"
  IMAGE_MANIFEST_SHA="$(sha256_file "${IMAGE_MANIFEST}")"
fi

mkdir -p "$(dirname "${OUT_PATH}")"
if [[ -z "${WORK_DIR}" ]]; then
  WORK_DIR="$(mktemp -d)"
fi
WRITE_JS="${WORK_DIR}/write-cloud-release-provenance.mjs"
cat > "${WRITE_JS}" <<'JS'
import { createHash } from "node:crypto";
import { relative } from "node:path";

const releaseManifestPath = `${process.env.RELEASE_DIR}/release-manifest.json`;
const releaseManifest = JSON.parse(await Bun.file(releaseManifestPath).text());
const imageManifestPath = process.env.IMAGE_MANIFEST || "";
const imageManifest = imageManifestPath
  ? JSON.parse(await Bun.file(imageManifestPath).text())
  : null;
const sourceRelease = imageManifest?.source_release ?? null;
const isGithubActions = process.env.GITHUB_ACTIONS === "true";

function sha256Text(value) {
  return createHash("sha256").update(value).digest("hex");
}

function artifactPath(path, label) {
  if (path === "") {
    return "";
  }
  const normalized = relative(process.cwd(), path).split("\\").join("/");
  if (normalized === "" || normalized.startsWith("/") || normalized.includes("..") || normalized.includes("\\")) {
    console.error(`${label} must be a stable artifact-local path`);
    process.exit(1);
  }
  return normalized;
}

const provenance = {
  schema_version: "bucephalus_cloud_release_provenance_v1",
  predicate_type: "https://bucephalus.dev/provenance/cloud-release/v1",
  generated_at: new Date().toISOString(),
  release: {
    input_kind: process.env.RELEASE_INPUT_KIND,
    version: releaseManifest.version,
    target: releaseManifest.target,
    git_sha: releaseManifest.git_sha,
    git_dirty: releaseManifest.git_dirty,
    archive_sha256: process.env.RELEASE_ARCHIVE_SHA || null,
    manifest_sha256: process.env.RELEASE_MANIFEST_SHA,
  },
  source_release: sourceRelease,
  builder: {
    kind: isGithubActions ? "github_actions" : "local",
    github_server_url: isGithubActions ? process.env.GITHUB_SERVER_URL || null : null,
    github_repository: isGithubActions ? process.env.GITHUB_REPOSITORY || null : null,
    github_run_id: isGithubActions ? process.env.GITHUB_RUN_ID || null : null,
    github_run_attempt: isGithubActions ? process.env.GITHUB_RUN_ATTEMPT || null : null,
    github_workflow: isGithubActions ? process.env.GITHUB_WORKFLOW || null : null,
    github_ref: isGithubActions ? process.env.GITHUB_REF || null : null,
    github_sha: isGithubActions ? process.env.GITHUB_SHA || null : null,
  },
  materials: {
    lockfiles: releaseManifest.source_inputs?.lockfiles ?? {},
    cloud_package: releaseManifest.source_inputs?.cloud_package ?? null,
    cloud_runtime_package: releaseManifest.source_inputs?.cloud_runtime_package ?? null,
    content_sets: releaseManifest.source_inputs?.content_sets ?? {},
  },
  image_build: imageManifest
    ? {
        manifest_path: artifactPath(imageManifestPath, "image_build.manifest_path"),
        manifest_sha256: process.env.IMAGE_MANIFEST_SHA,
        pushed: imageManifest.pushed,
        base_image: imageManifest.base_image,
        image_context: imageManifest.image_context,
        images: imageManifest.images.map((image) => ({
          component: image.component,
          dockerfile: image.dockerfile,
          image_repository: image.image_repository,
          tag_ref: image.tag_ref,
          immutable_ref: image.immutable_ref,
          image_id: image.image_id,
          digest: image.digest,
          metadata_file: image.metadata_file,
          boundary_verified: image.boundary_verified,
          boundary_image_ref: image.boundary_image_ref,
          boundary_image_id: image.boundary_image_id,
          boundary_metadata_file: image.boundary_metadata_file,
          timings_seconds: image.timings_seconds ?? null,
        })),
      }
    : null,
  signature: {
    status: "unsigned",
    reason: "registry or promotion signing boundary is not configured",
  },
};

provenance.provenance_sha256 = sha256Text(JSON.stringify({
  ...provenance,
  provenance_sha256: null,
}));

await Bun.write(process.env.OUT_PATH, `${JSON.stringify(provenance, null, 2)}\n`);
JS

RELEASE_DIR="${RELEASE_DIR}" \
RELEASE_INPUT_KIND="${RELEASE_INPUT_KIND}" \
RELEASE_ARCHIVE_SHA="${RELEASE_ARCHIVE_SHA}" \
RELEASE_MANIFEST_SHA="$(sha256_file "${RELEASE_DIR}/release-manifest.json")" \
IMAGE_MANIFEST="${IMAGE_MANIFEST}" \
IMAGE_MANIFEST_SHA="${IMAGE_MANIFEST_SHA}" \
OUT_PATH="${OUT_PATH}" \
bun "${WRITE_JS}"

"${ROOT_DIR}/scripts/release/verify-cloud-release-provenance.sh" "${OUT_PATH}" --release "${RELEASE_INPUT}"
echo "provenance=${OUT_PATH}"
