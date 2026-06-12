#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="${BUCEPHALUS_RELEASE_VERSION:-}"
OUT_DIR="${BUCEPHALUS_RELEASE_OUT_DIR:-${ROOT_DIR}/dist/releases}"
TARGET="${BUCEPHALUS_RELEASE_TARGET:-}"
CORE_BIN_INPUT="${BUCEPHALUS_RELEASE_CORE_BIN:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/release/build-core-release.sh --version <version> [--out <dir>] [--target <rust-target>] [--core-bin <path>]

Builds the public Bucephalus CLI release archive consumed by scripts/install.sh.
The archive is named bucephalus-<target>.tar.gz and contains:
  - bucephalus
  - buc
  - bucephalus-cloud
  - bucephalus-modal-launcher
  - README.md
  - LICENSE
  - release-manifest.json
  - SHA256SUMS

Use --core-bin only with a prebuilt bucephalus binary from a verified matching
target build in the same workflow.
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
    --core-bin)
      CORE_BIN_INPUT="${2:-}"
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
  local digest
  if command -v sha256sum >/dev/null 2>&1; then
    read -r digest _ < <(sha256sum "${file}")
  elif command -v shasum >/dev/null 2>&1; then
    read -r digest _ < <(shasum -a 256 "${file}")
  else
    echo "sha256sum or shasum is required" >&2
    exit 2
  fi
  printf '%s' "${digest}"
}

require_command cargo
require_command git
require_command go
require_command rustc
require_command tar
require_command install

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
if [[ -n "${TARGET}" ]]; then
  TARGET_LABEL="${TARGET}"
else
  TARGET_LABEL="$(rustc -vV | sed -n 's/^host: //p')"
fi

go_target_env() {
  case "$1" in
    x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl)
      printf 'GOOS=linux GOARCH=amd64'
      ;;
    aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
      printf 'GOOS=linux GOARCH=arm64'
      ;;
    x86_64-apple-darwin)
      printf 'GOOS=darwin GOARCH=amd64'
      ;;
    aarch64-apple-darwin)
      printf 'GOOS=darwin GOARCH=arm64'
      ;;
    *)
      echo "unsupported Go release target mapping for ${1}" >&2
      exit 2
      ;;
  esac
}

RELEASE_NAME="bucephalus-${VERSION}-${TARGET_LABEL}"
RELEASE_DIR="${OUT_DIR}/${RELEASE_NAME}"
ARCHIVE_BASENAME="bucephalus-${TARGET_LABEL}.tar.gz"
ARCHIVE_PATH="${OUT_DIR}/${ARCHIVE_BASENAME}"

rm -rf "${RELEASE_DIR}" "${ARCHIVE_PATH}" "${ARCHIVE_PATH}.sha256"
mkdir -p "${RELEASE_DIR}" "${OUT_DIR}"

if [[ -n "${CORE_BIN_INPUT}" ]]; then
  echo "== Using prebuilt bucephalus ${VERSION} for ${TARGET_LABEL} =="
  if [[ ! -f "${CORE_BIN_INPUT}" ]]; then
    echo "--core-bin does not exist: ${CORE_BIN_INPUT}" >&2
    exit 2
  fi
  CORE_BIN="${CORE_BIN_INPUT}"
else
  echo "== Building bucephalus ${VERSION} for ${TARGET_LABEL} =="
  if [[ -n "${TARGET}" ]]; then
    cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" -p bucephalus-cli --release --no-default-features --features core-cli --bin bucephalus --target "${TARGET}"
    CORE_BIN="${ROOT_DIR}/target/${TARGET}/release/bucephalus"
  else
    cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" -p bucephalus-cli --release --no-default-features --features core-cli --bin bucephalus
    CORE_BIN="${ROOT_DIR}/target/release/bucephalus"
  fi
fi

install -m 0755 "${CORE_BIN}" "${RELEASE_DIR}/bucephalus"
echo "== Building buc ${VERSION} for ${TARGET_LABEL} =="
if [[ -n "${TARGET}" ]]; then
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" -p bucephalus-cli --release --no-default-features --features hosted-cli --bin buc --target "${TARGET}"
  BUC_BIN="${ROOT_DIR}/target/${TARGET}/release/buc"
else
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" -p bucephalus-cli --release --no-default-features --features hosted-cli --bin buc
  BUC_BIN="${ROOT_DIR}/target/release/buc"
fi
install -m 0755 "${BUC_BIN}" "${RELEASE_DIR}/buc"
echo "== Building bucephalus-cloud ${VERSION} for ${TARGET_LABEL} =="
if [[ -n "${TARGET}" ]]; then
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" -p bucephalus-cli --release --no-default-features --features cloud-operator --bin bucephalus-cloud --target "${TARGET}"
  CLOUD_CLI_BIN="${ROOT_DIR}/target/${TARGET}/release/bucephalus-cloud"
else
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" -p bucephalus-cli --release --no-default-features --features cloud-operator --bin bucephalus-cloud
  CLOUD_CLI_BIN="${ROOT_DIR}/target/release/bucephalus-cloud"
fi
install -m 0755 "${CLOUD_CLI_BIN}" "${RELEASE_DIR}/bucephalus-cloud"
echo "== Building Modal launcher ${VERSION} for ${TARGET_LABEL} =="
read -r GOOS_VALUE GOARCH_VALUE <<< "$(go_target_env "${TARGET_LABEL}" | sed 's/GOOS=//; s/ GOARCH=/ /')"
(
  cd "${ROOT_DIR}/modal-launcher"
  GOOS="${GOOS_VALUE}" GOARCH="${GOARCH_VALUE}" CGO_ENABLED=0 go build -mod=readonly -trimpath -o "${RELEASE_DIR}/bucephalus-modal-launcher" .
)
install -m 0644 "${ROOT_DIR}/README.md" "${RELEASE_DIR}/README.md"
install -m 0644 "${ROOT_DIR}/LICENSE" "${RELEASE_DIR}/LICENSE"

CORE_SHA="$(sha256_file "${RELEASE_DIR}/bucephalus")"
BUC_SHA="$(sha256_file "${RELEASE_DIR}/buc")"
CLOUD_CLI_SHA="$(sha256_file "${RELEASE_DIR}/bucephalus-cloud")"
MODAL_LAUNCHER_SHA="$(sha256_file "${RELEASE_DIR}/bucephalus-modal-launcher")"
cat > "${RELEASE_DIR}/release-manifest.json" <<EOF
{
  "schema_version": "bucephalus_core_release_v1",
  "version": "${VERSION}",
  "git_sha": "${GIT_SHA}",
  "git_dirty": ${GIT_DIRTY},
  "build_date": "${BUILD_DATE}",
  "target": "${TARGET_LABEL}",
  "artifacts": {
    "core_binary": {
      "path": "bucephalus",
      "sha256": "${CORE_SHA}"
    },
    "hosted_cli_binary": {
      "path": "buc",
      "sha256": "${BUC_SHA}"
    },
    "cloud_cli_binary": {
      "path": "bucephalus-cloud",
      "sha256": "${CLOUD_CLI_SHA}"
    },
    "modal_launcher_binary": {
      "path": "bucephalus-modal-launcher",
      "sha256": "${MODAL_LAUNCHER_SHA}"
    }
  }
}
EOF

(
  cd "${RELEASE_DIR}"
  : > SHA256SUMS
  for file in bucephalus buc bucephalus-cloud bucephalus-modal-launcher README.md LICENSE release-manifest.json; do
    digest="$(sha256_file "${file}")"
    printf "%s  %s\n" "${digest}" "${file}" >> SHA256SUMS
  done
)

echo "== Archive =="
(
  cd "${RELEASE_DIR}"
  tar -czf "${ARCHIVE_PATH}" bucephalus buc bucephalus-cloud bucephalus-modal-launcher README.md LICENSE release-manifest.json SHA256SUMS
)

ARCHIVE_SHA="$(sha256_file "${ARCHIVE_PATH}")"
printf "%s  %s\n" "${ARCHIVE_SHA}" "${ARCHIVE_BASENAME}" > "${ARCHIVE_PATH}.sha256"

echo "release_dir=${RELEASE_DIR}"
echo "archive=${ARCHIVE_PATH}"
echo "archive_sha256=${ARCHIVE_SHA}"
