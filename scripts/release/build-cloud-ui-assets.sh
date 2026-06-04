#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION=""
OUT_DIR=""
SKIP_BUILD="false"

usage() {
  cat <<'USAGE'
Usage: scripts/release/build-cloud-ui-assets.sh --version <version> [--out <dir>] [--skip-build]

Builds the Bucephalus Cloud web UI and writes a versioned Cloudflare-ready
static asset handoff.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --out)
      OUT_DIR="${2:-}"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD="true"
      shift
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

if [[ -z "${VERSION}" ]]; then
  echo "--version is required" >&2
  exit 2
fi
if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi
if ! command -v git >/dev/null 2>&1; then
  echo "required command not found: git" >&2
  exit 2
fi

OUT_DIR="${OUT_DIR:-${ROOT_DIR}/dist/releases/cloud-ui-${VERSION}}"
WEB_DIST="${ROOT_DIR}/bucephalus-cloud/web/dist"

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

if [[ "${SKIP_BUILD}" != "true" ]]; then
  (
    cd "${ROOT_DIR}/bucephalus-cloud"
    bun run web:build
  )
fi

if [[ ! -f "${WEB_DIST}/index.html" ]]; then
  echo "web build did not produce ${WEB_DIST}/index.html" >&2
  exit 1
fi
if [[ ! -f "${WEB_DIST}/buc-config.js" ]]; then
  echo "web build did not produce ${WEB_DIST}/buc-config.js" >&2
  exit 1
fi

rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}"
cp -R "${WEB_DIST}" "${OUT_DIR}/dist"

write_checksums "${OUT_DIR}/dist" "${OUT_DIR}/SHA256SUMS"
DIST_TREE_SHA="$(sha256_file "${OUT_DIR}/SHA256SUMS")"
GIT_SHA="$(git -C "${ROOT_DIR}" rev-parse HEAD)"
if git -C "${ROOT_DIR}" diff --quiet && git -C "${ROOT_DIR}" diff --cached --quiet; then
  GIT_DIRTY="false"
else
  GIT_DIRTY="true"
fi
BUILD_DATE="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

VERSION="${VERSION}" \
GIT_SHA="${GIT_SHA}" \
GIT_DIRTY="${GIT_DIRTY}" \
BUILD_DATE="${BUILD_DATE}" \
DIST_TREE_SHA="${DIST_TREE_SHA}" \
bun -e '
const manifest = {
  schema_version: "bucephalus_cloud_ui_assets_v1",
  version: process.env.VERSION,
  git_sha: process.env.GIT_SHA,
  git_dirty: process.env.GIT_DIRTY === "true",
  build_date: process.env.BUILD_DATE,
  asset_root: "dist",
  checksum_file: "SHA256SUMS",
  dist_tree_sha256: process.env.DIST_TREE_SHA,
  deploy_target: {
    provider: "cloudflare_workers_static_assets",
    shell: "bucephalus-cloud/web/worker.ts",
    config: "bucephalus-cloud/wrangler.toml"
  }
};
console.log(JSON.stringify(manifest, null, 2));
' > "${OUT_DIR}/cloud-ui-assets.json"

echo "wrote Cloud UI assets to ${OUT_DIR}"
