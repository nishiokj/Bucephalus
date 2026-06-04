#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARTIFACT_DIR="${1:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-ui-assets.sh <cloud-ui-artifact-dir>

Validates a Bucephalus Cloud UI static asset handoff before Cloudflare deploy.
USAGE
}

if [[ "${ARTIFACT_DIR}" == "-h" || "${ARTIFACT_DIR}" == "--help" ]]; then
  usage
  exit 0
fi
if [[ -z "${ARTIFACT_DIR}" ]]; then
  usage >&2
  exit 2
fi
if [[ ! -d "${ARTIFACT_DIR}" ]]; then
  echo "artifact directory not found: ${ARTIFACT_DIR}" >&2
  exit 1
fi
if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "sha256sum or shasum is required" >&2
    exit 2
  fi
}

write_checksums() {
  local base_dir="$1"
  local out_file="$2"
  (
    cd "${base_dir}"
    while IFS= read -r file; do
      local rel="${file#./}"
      printf "%s  %s\n" "$(sha256_file "${file}")" "${rel}"
    done < <(find . -type f -print | LC_ALL=C sort)
  ) > "${out_file}"
}

MANIFEST="${ARTIFACT_DIR}/cloud-ui-assets.json"
DIST_DIR="${ARTIFACT_DIR}/dist"
CHECKSUMS="${ARTIFACT_DIR}/SHA256SUMS"

for required in "${MANIFEST}" "${DIST_DIR}/index.html" "${DIST_DIR}/buc-config.js" "${CHECKSUMS}"; do
  if [[ ! -e "${required}" ]]; then
    echo "Cloud UI artifact is missing ${required}" >&2
    exit 1
  fi
done

TMP_CHECKSUMS="$(mktemp)"
trap 'rm -f "${TMP_CHECKSUMS}"' EXIT
write_checksums "${DIST_DIR}" "${TMP_CHECKSUMS}"
if ! cmp -s "${CHECKSUMS}" "${TMP_CHECKSUMS}"; then
  echo "Cloud UI asset checksums do not match SHA256SUMS" >&2
  exit 1
fi

EXPECTED_TREE_SHA="$(sha256_file "${CHECKSUMS}")"
MANIFEST="${MANIFEST}" EXPECTED_TREE_SHA="${EXPECTED_TREE_SHA}" ROOT_DIR="${ROOT_DIR}" bun -e '
const manifest = JSON.parse(await Bun.file(process.env.MANIFEST).text());
const failures = [];
function fail(message) { failures.push(message); }
if (manifest.schema_version !== "bucephalus_cloud_ui_assets_v1") fail("schema_version must be bucephalus_cloud_ui_assets_v1");
if (typeof manifest.version !== "string" || manifest.version.trim() === "") fail("version is required");
if (typeof manifest.git_sha !== "string" || !/^[a-f0-9]{40}$/.test(manifest.git_sha)) fail("git_sha must be a 40-character lowercase object id");
if (typeof manifest.git_dirty !== "boolean") fail("git_dirty must be boolean");
if (manifest.asset_root !== "dist") fail("asset_root must be dist");
if (manifest.checksum_file !== "SHA256SUMS") fail("checksum_file must be SHA256SUMS");
if (manifest.dist_tree_sha256 !== process.env.EXPECTED_TREE_SHA) fail("dist_tree_sha256 does not match SHA256SUMS");
if (manifest.deploy_target?.provider !== "cloudflare_workers_static_assets") fail("deploy_target.provider must be cloudflare_workers_static_assets");
if (manifest.deploy_target?.shell !== "bucephalus-cloud/web/worker.ts") fail("deploy_target.shell must point at the checked Worker shell");
if (manifest.deploy_target?.config !== "bucephalus-cloud/wrangler.toml") fail("deploy_target.config must point at the checked Wrangler config");
if (failures.length) {
  for (const failure of failures) console.error(failure);
  process.exit(1);
}
'

echo "Cloud UI assets verified: ${ARTIFACT_DIR}"
