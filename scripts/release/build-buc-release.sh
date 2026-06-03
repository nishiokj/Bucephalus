#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="${BUCEPHALUS_RELEASE_VERSION:-}"
OUT_DIR="${BUCEPHALUS_RELEASE_OUT_DIR:-${ROOT_DIR}/dist/releases}"
TARGET="${BUCEPHALUS_RELEASE_TARGET:-}"
ARCHIVE_BASENAME=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/build-buc-release.sh --version <version> [--out <dir>] [--target <rust-target>]

Builds a Bucephalus release directory containing:
  - bin/bucephalus
  - bucephalus-cloud worker/controller/API bundle
  - migrations, OpenAPI specs, and deployment contracts
  - release-manifest.json
  - SHA256SUMS
  - a tar.gz archive
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
    --target)
      TARGET="${2:-}"
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

if [[ -z "${VERSION}" ]]; then
  echo "--version or BUCEPHALUS_RELEASE_VERSION is required" >&2
  exit 2
fi

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

require_command cargo
require_command bun
require_command git
require_command tar

CARGO_BUILD_SUBCOMMAND="${BUCEPHALUS_RELEASE_CARGO_BUILD_SUBCOMMAND:-build}"

GIT_SHA="$(git -C "${ROOT_DIR}" rev-parse HEAD)"
GIT_DIRTY="false"
if [[ -n "$(git -C "${ROOT_DIR}" status --porcelain)" ]]; then
  GIT_DIRTY="true"
fi
if [[ "${GIT_DIRTY}" == "true" && "${BUCEPHALUS_RELEASE_ALLOW_DIRTY:-false}" != "true" ]]; then
  echo "worktree is dirty; set BUCEPHALUS_RELEASE_ALLOW_DIRTY=true for local smoke builds" >&2
  exit 2
fi
BUILD_DATE="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
HOST_ARCH="$(uname -m)"
HOST_OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
TARGET_LABEL="${TARGET:-${HOST_ARCH}-${HOST_OS}}"
RELEASE_NAME="bucephalus-${VERSION}-${TARGET_LABEL}"
RELEASE_DIR="${OUT_DIR}/${RELEASE_NAME}"
ARCHIVE_BASENAME="${RELEASE_NAME}.tar.gz"
ARCHIVE_PATH="${OUT_DIR}/${ARCHIVE_BASENAME}"

rm -rf "${RELEASE_DIR}" "${ARCHIVE_PATH}"
mkdir -p "${RELEASE_DIR}/bin" "${RELEASE_DIR}/bucephalus-cloud"

echo "== Building bucephalus ${VERSION} =="
if [[ -n "${TARGET}" ]]; then
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" --release --bin bucephalus --target "${TARGET}"
  CORE_BIN="${ROOT_DIR}/target/${TARGET}/release/bucephalus"
else
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" --release --bin bucephalus
  CORE_BIN="${ROOT_DIR}/target/release/bucephalus"
fi

install -m 0755 "${CORE_BIN}" "${RELEASE_DIR}/bin/bucephalus"

echo "== Preparing cloud bundle =="
(
  cd "${ROOT_DIR}/bucephalus-cloud"
  bun install --frozen-lockfile
  bun run typecheck
  bun test
)

for path in \
  package.json \
  bun.lock \
  tsconfig.json \
  docker-compose.yml \
  scripts \
  src \
  api \
  db \
  deploy
do
  cp -R "${ROOT_DIR}/bucephalus-cloud/${path}" "${RELEASE_DIR}/bucephalus-cloud/${path}"
done

CORE_SHA="$(sha256_file "${RELEASE_DIR}/bin/bucephalus")"

cat > "${RELEASE_DIR}/release-manifest.json" <<EOF
{
  "schema_version": "bucephalus_release_v1",
  "version": "${VERSION}",
  "git_sha": "${GIT_SHA}",
  "git_dirty": ${GIT_DIRTY},
  "build_date": "${BUILD_DATE}",
  "target": "${TARGET_LABEL}",
  "artifacts": {
    "core_binary": {
      "path": "bin/bucephalus",
      "sha256": "${CORE_SHA}"
    },
    "cloud_bundle": {
      "path": "bucephalus-cloud",
      "runtime": "bun",
      "entrypoints": {
        "api": "bun run start",
        "worker": "bun run worker",
        "pool_controller": "bun run pool-controller",
        "migrations": "bun run db:migrate",
        "control_plane_installer": "deploy/control-plane/install-control-plane.sh",
        "control_plane_smoke": "deploy/control-plane/smoke-control-plane.sh"
      }
    },
    "runner_image_contract": {
      "path": "bucephalus-cloud/deploy/runner-image/runner-image.manifest.json"
    }
  },
  "schemas": {
    "sealed_package": "sealed_run_package_v2",
    "release": "bucephalus_release_v1"
  }
}
EOF

echo "== Checksums =="
(
  cd "${RELEASE_DIR}"
  find . -type f | sort | while read -r file; do
    digest="$(sha256_file "${file}")"
    printf "%s  %s\n" "${digest}" "${file#./}"
  done > SHA256SUMS
)

echo "== Archive =="
mkdir -p "${OUT_DIR}"
(
  cd "${OUT_DIR}"
  tar -czf "${ARCHIVE_BASENAME}" "${RELEASE_NAME}"
)

ARCHIVE_SHA="$(sha256_file "${ARCHIVE_PATH}")"
cat > "${OUT_DIR}/${ARCHIVE_BASENAME}.sha256" <<EOF
${ARCHIVE_SHA}  ${ARCHIVE_BASENAME}
EOF

echo "release_dir=${RELEASE_DIR}"
echo "archive=${ARCHIVE_PATH}"
echo "archive_sha256=${ARCHIVE_SHA}"
