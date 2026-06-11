#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST=""
POOL_ID=""
EVIDENCE_URI=""
OUTPUT_PATH=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/write-worker-image-promotion-env.sh --manifest <cloud-image-build-manifest.json> --pool-id <runner-pool-id> (--out <path>|--github-env <path>) [--evidence-uri <uri>]

Writes BUCEPHALUS_PROMOTE_WORKER_* environment values for the Cloud Run worker
image promotion job from a pushed, verified image build manifest.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest)
      MANIFEST="${2:-}"
      shift 2
      ;;
    --pool-id)
      POOL_ID="${2:-}"
      shift 2
      ;;
    --evidence-uri)
      EVIDENCE_URI="${2:-}"
      shift 2
      ;;
    --out|--github-env)
      OUTPUT_PATH="${2:-}"
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

if [[ -z "${MANIFEST}" || -z "${POOL_ID}" || -z "${OUTPUT_PATH}" ]]; then
  usage >&2
  exit 2
fi

"${ROOT_DIR}/scripts/release/verify-cloud-image-build-manifest.sh" "${MANIFEST}" --allow-partial

if command -v sha256sum >/dev/null 2>&1; then
  MANIFEST_SHA="$(sha256sum "${MANIFEST}" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  MANIFEST_SHA="$(shasum -a 256 "${MANIFEST}" | awk '{print $1}')"
else
  echo "sha256sum or shasum is required" >&2
  exit 2
fi
if [[ -z "${EVIDENCE_URI}" ]]; then
  EVIDENCE_URI="file://${MANIFEST}"
fi

worker_json="$(bun -e '
const manifest = JSON.parse(await Bun.file(process.argv[1]).text());
if (manifest.pushed !== true) {
  console.error("manifest must be pushed=true to promote an active worker image");
  process.exit(1);
}
const worker = manifest.images.find((image) => image.component === "worker");
if (!worker?.immutable_ref) {
  console.error("manifest does not contain a pushed worker immutable_ref");
  process.exit(1);
}
console.log(JSON.stringify({
  image: worker.immutable_ref,
  release_version: manifest.release.version,
  release_git_sha: manifest.release.git_sha,
  boundary_verified_at: new Date().toISOString(),
}));
' "${MANIFEST}")"

image_ref="$(bun -e 'const data = JSON.parse(process.argv[1]); console.log(data.image);' "${worker_json}")"
release_version="$(bun -e 'const data = JSON.parse(process.argv[1]); console.log(data.release_version);' "${worker_json}")"
release_git_sha="$(bun -e 'const data = JSON.parse(process.argv[1]); console.log(data.release_git_sha);' "${worker_json}")"
boundary_verified_at="$(bun -e 'const data = JSON.parse(process.argv[1]); console.log(data.boundary_verified_at);' "${worker_json}")"

{
  echo "BUCEPHALUS_PROMOTE_WORKER_POOL_ID=${POOL_ID}"
  echo "BUCEPHALUS_PROMOTE_WORKER_IMAGE=${image_ref}"
  echo "BUCEPHALUS_PROMOTE_WORKER_RELEASE_VERSION=${release_version}"
  echo "BUCEPHALUS_PROMOTE_WORKER_RELEASE_GIT_SHA=${release_git_sha}"
  echo "BUCEPHALUS_PROMOTE_WORKER_EVIDENCE_URI=${EVIDENCE_URI}"
  echo "BUCEPHALUS_PROMOTE_WORKER_EVIDENCE_SHA256=sha256:${MANIFEST_SHA}"
  echo "BUCEPHALUS_PROMOTE_WORKER_BOUNDARY_VERIFIED_AT=${boundary_verified_at}"
  echo 'BUCEPHALUS_PROMOTE_WORKER_METADATA_JSON={"source":"cloud-image-build-manifest","trigger":"gcp-deploy-workflow"}'
} >> "${OUTPUT_PATH}"
