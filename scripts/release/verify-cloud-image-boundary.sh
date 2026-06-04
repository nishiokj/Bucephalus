#!/usr/bin/env bash
set -euo pipefail

IMAGE_REF="${1:-}"
COMPONENT=""
RELEASE_MANIFEST_SHA=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-image-boundary.sh <image-ref> --component <name> --release-manifest-sha256 <sha256>

Inspects a built Bucephalus Cloud image and verifies release labels plus the
absence of environment-specific runtime configuration in image metadata.
USAGE
}

shift || true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --component)
      COMPONENT="${2:-}"
      shift 2
      ;;
    --release-manifest-sha256)
      RELEASE_MANIFEST_SHA="${2:-}"
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

if [[ -z "${IMAGE_REF}" || -z "${COMPONENT}" || -z "${RELEASE_MANIFEST_SHA}" ]]; then
  usage >&2
  exit 2
fi

if [[ ! "${RELEASE_MANIFEST_SHA}" =~ ^[a-f0-9]{64}$ ]]; then
  echo "--release-manifest-sha256 must be a lowercase sha256 hex digest" >&2
  exit 2
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "required command not found: docker" >&2
  exit 2
fi
if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

WORK_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "${WORK_DIR}"
}
trap cleanup EXIT
VERIFY_JS="${WORK_DIR}/verify-cloud-image-boundary.mjs"
cat > "${VERIFY_JS}" <<'JS'
const images = JSON.parse(process.env.INSPECT_JSON);
if (!Array.isArray(images) || images.length !== 1) {
  console.error("docker image inspect must return exactly one image");
  process.exit(1);
}
const image = images[0];
const config = image.Config ?? {};
const labels = config.Labels ?? {};
const env = config.Env ?? [];
const component = process.env.COMPONENT;
const releaseManifestSha = process.env.RELEASE_MANIFEST_SHA;

function fail(message) {
  console.error(message);
  process.exit(1);
}

if (labels["org.bucephalus.image.component"] !== component) {
  fail(`image component label mismatch: expected ${component}, got ${labels["org.bucephalus.image.component"] ?? "<missing>"}`);
}
if (labels["org.bucephalus.image.contract"] !== "cloud-release-bundle-v1") {
  fail("image contract label must be cloud-release-bundle-v1");
}
if (labels["org.bucephalus.release.manifest.sha256"] !== releaseManifestSha) {
  fail("release manifest sha label does not match selected release bundle");
}

const forbiddenEnvNames = new Set([
  "DATABASE_URL",
  "BUCEPHALUS_WORKER_DATABASE_URL",
  "BUCEPHALUS_RUN_STORE_URL",
  "BUCEPHALUS_CLOUD_WORKER_TOKEN",
  "BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON",
  "BUCEPHALUS_CLOUD_API_URL",
  "GOOGLE_APPLICATION_CREDENTIALS",
  "GOOGLE_CLOUD_PROJECT",
  "GCLOUD_PROJECT",
  "TAILSCALE_AUTHKEY",
]);
const forbiddenValuePattern = /(postgres:\/\/|@sha256:[a-f0-9]{64}|ya29\.|ghp_|github_pat_|BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|tailscale)/i;
for (const entry of env) {
  const [name, ...rest] = String(entry).split("=");
  const value = rest.join("=");
  if (forbiddenEnvNames.has(name)) {
    fail(`forbidden runtime env baked into image: ${name}`);
  }
  if (forbiddenValuePattern.test(value)) {
    fail(`forbidden-looking runtime value baked into image env: ${name}`);
  }
}

for (const [key, value] of Object.entries(labels)) {
  if (/token|secret|password|database_url/i.test(key) || forbiddenValuePattern.test(String(value))) {
    fail(`forbidden-looking runtime value baked into image label: ${key}`);
  }
}

console.log(`verified image boundary for ${component}`);
JS

INSPECT_JSON="$(docker image inspect "${IMAGE_REF}")"
INSPECT_JSON="${INSPECT_JSON}" COMPONENT="${COMPONENT}" RELEASE_MANIFEST_SHA="${RELEASE_MANIFEST_SHA}" bun "${VERIFY_JS}"
