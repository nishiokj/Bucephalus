#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
POLICY="${ROOT_DIR}/bucephalus-cloud/images/base-image-policy.json"
BASE_IMAGE=""
PUSH="false"

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-base-image-policy.sh --base-image <image@sha256:digest> [--push] [--policy <path>]

Validates the Bucephalus Cloud base-image policy. Local image inspection only
requires a digest-addressed base image. Pushed image publication requires the
base image to appear in approved_base_images.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-image)
      BASE_IMAGE="${2:-}"
      shift 2
      ;;
    --push)
      PUSH="true"
      shift
      ;;
    --policy)
      POLICY="${2:-}"
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

if [[ -z "${BASE_IMAGE}" ]]; then
  usage >&2
  exit 2
fi

if [[ ! -f "${POLICY}" ]]; then
  echo "base image policy does not exist: ${POLICY}" >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

POLICY="${POLICY}" BASE_IMAGE="${BASE_IMAGE}" PUSH="${PUSH}" bun -e '
const policyPath = process.env.POLICY;
const baseImage = process.env.BASE_IMAGE;
const push = process.env.PUSH === "true";
const policy = JSON.parse(await Bun.file(policyPath).text());
const digestRef = /^[^\s]+@sha256:[a-f0-9]{64}$/;
const forbidden = /(DATABASE_URL|BUCEPHALUS_CLOUD_WORKER_TOKEN|GOOGLE_APPLICATION_CREDENTIALS|TAILSCALE_AUTHKEY|ghp_|github_pat_|ya29\\.)/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function checkEntry(entry, listName, index) {
  const label = `${listName}[${index}]`;
  if (!entry || typeof entry !== "object") {
    fail(`${label} must be an object`);
  }
  if (typeof entry.image !== "string" || !digestRef.test(entry.image)) {
    fail(`${label}.image must be digest-addressed`);
  }
  if (entry.image.includes(":latest") || entry.image.startsWith("http://") || entry.image.startsWith("https://")) {
    fail(`${label}.image must not be a tag, URL, or latest reference`);
  }
  if (forbidden.test(entry.image)) {
    fail(`${label}.image contains forbidden-looking runtime/secret material`);
  }
  if (typeof entry.reviewed_by !== "string" || entry.reviewed_by.trim() === "") {
    fail(`${label}.reviewed_by is required`);
  }
  if (Number.isNaN(Date.parse(entry.reviewed_at))) {
    fail(`${label}.reviewed_at must be an ISO timestamp`);
  }
  if (typeof entry.reason !== "string" || entry.reason.trim() === "") {
    fail(`${label}.reason is required`);
  }
  if (typeof entry.source_tag !== "string" || entry.source_tag.trim() === "" || entry.source_tag.includes("@sha256:") || /\blatest\b/.test(entry.source_tag)) {
    fail(`${label}.source_tag must be a non-latest source tag`);
  }
  if (Number.isNaN(Date.parse(entry.resolved_at))) {
    fail(`${label}.resolved_at must be an ISO timestamp`);
  }
  if (typeof entry.resolved_by !== "string" || entry.resolved_by.trim() === "") {
    fail(`${label}.resolved_by is required`);
  }
  if (entry.registry_response?.docker_content_digest !== entry.image.split("@")[1]) {
    fail(`${label}.registry_response.docker_content_digest must match the approved image digest`);
  }
  if (typeof entry.registry_response?.content_type !== "string" || !entry.registry_response.content_type.includes("image.index")) {
    fail(`${label}.registry_response.content_type must record an OCI/Docker image index`);
  }
  if (!Array.isArray(entry.platforms) || entry.platforms.length === 0) {
    fail(`${label}.platforms must record resolved Linux platform manifests`);
  }
  const platformDigests = new Set();
  for (const [platformIndex, platform] of entry.platforms.entries()) {
    const platformLabel = `${label}.platforms[${platformIndex}]`;
    if (platform?.os !== "linux") {
      fail(`${platformLabel}.os must be linux`);
    }
    if (typeof platform.architecture !== "string" || platform.architecture.trim() === "") {
      fail(`${platformLabel}.architecture is required`);
    }
    if (typeof platform.digest !== "string" || !/^sha256:[a-f0-9]{64}$/.test(platform.digest)) {
      fail(`${platformLabel}.digest must be a sha256 manifest digest`);
    }
    platformDigests.add(`${platform.os}/${platform.architecture}`);
  }
  for (const requiredPlatform of ["linux/amd64", "linux/arm64"]) {
    if (!platformDigests.has(requiredPlatform)) {
      fail(`${label}.platforms must include ${requiredPlatform}`);
    }
  }
}

if (!digestRef.test(baseImage)) {
  fail("--base-image must be digest-addressed");
}
if (baseImage.includes(":latest") || baseImage.startsWith("http://") || baseImage.startsWith("https://")) {
  fail("--base-image must not be a tag, URL, or latest reference");
}
if (forbidden.test(baseImage)) {
  fail("--base-image contains forbidden-looking runtime/secret material");
}

if (policy.schema_version !== "bucephalus_cloud_base_image_policy_v1") {
  fail("schema_version must be bucephalus_cloud_base_image_policy_v1");
}
if (!policy.requirements?.pushed_images_require_approved_base) {
  fail("policy must require approved bases for pushed images");
}
if (!Array.isArray(policy.approved_base_images)) {
  fail("approved_base_images must be an array");
}
if (!Array.isArray(policy.candidate_base_images)) {
  fail("candidate_base_images must be an array");
}

for (const [i, entry] of policy.approved_base_images.entries()) {
  checkEntry(entry, "approved_base_images", i);
}
for (const [i, entry] of policy.candidate_base_images.entries()) {
  checkEntry(entry, "candidate_base_images", i);
}

const approved = new Set(policy.approved_base_images.map((entry) => entry.image));
if (push && !approved.has(baseImage)) {
  fail(`pushed images require an approved base image policy entry: ${baseImage}`);
}

console.log(push ? "verified approved pushed base image" : "verified digest-addressed local base image");
'
