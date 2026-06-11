#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST=""
POOL_ID=""
EVIDENCE_URI=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/promote-worker-image.sh --manifest <cloud-image-build-manifest.json> --pool-id <runner-pool-id> [--evidence-uri <uri>]

Promotes the worker image from a pushed cloud image build manifest into the
runner pool's active worker image state in Postgres. Requires DATABASE_URL.
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

if [[ -z "${MANIFEST}" || -z "${POOL_ID}" ]]; then
  usage >&2
  exit 2
fi
if [[ -z "${DATABASE_URL:-}" ]]; then
  echo "DATABASE_URL is required to promote worker image state" >&2
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
}));
' "${MANIFEST}")"

image_ref="$(bun -e 'const data = JSON.parse(process.argv[1]); console.log(data.image);' "${worker_json}")"
release_version="$(bun -e 'const data = JSON.parse(process.argv[1]); console.log(data.release_version);' "${worker_json}")"
release_git_sha="$(bun -e 'const data = JSON.parse(process.argv[1]); console.log(data.release_git_sha);' "${worker_json}")"
if [[ -z "${EVIDENCE_URI}" ]]; then
  EVIDENCE_URI="file://${MANIFEST}"
fi

cd "${ROOT_DIR}/bucephalus-cloud"
bun run scripts/promote-worker-image.ts \
  --pool-id "${POOL_ID}" \
  --image "${image_ref}" \
  --release-version "${release_version}" \
  --release-git-sha "${release_git_sha}" \
  --promotion-evidence-uri "${EVIDENCE_URI}" \
  --promotion-evidence-sha256 "sha256:${MANIFEST_SHA}" \
  --metadata-json "{\"source\":\"cloud-image-build-manifest\"}"
