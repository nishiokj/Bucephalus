#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OVERRIDE=""
OUTPUT_PATH=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/resolve-release-version.sh [--override <version>] [--github-output <path>]

Resolves the release version from, in order:
  1. a pushed Git tag named v<version>
  2. an explicit manual override
  3. the tracked package or workspace package version in Cargo.toml
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --override)
      OVERRIDE="${2:-}"
      shift 2
      ;;
    --github-output)
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

version=""
source=""
if [[ "${GITHUB_REF_TYPE:-}" == "tag" && "${GITHUB_REF_NAME:-}" == v* ]]; then
  version="${GITHUB_REF_NAME#v}"
  source="tag"
elif [[ -n "${OVERRIDE}" ]]; then
  version="${OVERRIDE}"
  source="override"
else
  version="$(
    awk '
      /^\[package\]$/ { in_package = 1; next }
      /^\[workspace[.]package\]$/ { in_workspace_package = 1; next }
      /^\[/ { in_package = 0 }
      /^\[/ { in_workspace_package = 0 }
      in_package && /^version[[:space:]]*=/ {
        gsub(/"/, "", $3)
        print $3
        exit
      }
      in_workspace_package && /^version[[:space:]]*=/ {
        gsub(/"/, "", $3)
        print $3
        exit
      }
    ' "${ROOT_DIR}/Cargo.toml"
  )"
  source="Cargo.toml"
fi

if [[ ! "${version}" =~ ^[0-9]+[.][0-9]+[.][0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "resolved release version is not semver-like: ${version}" >&2
  exit 2
fi

# The workspace Cargo.toml version is the single source of truth: it is what
# binaries self-report via CARGO_PKG_VERSION. A tag or override that disagrees
# would ship binaries that lie about their version, making release skew
# undiagnosable. Releasing requires bumping Cargo.toml.
if [[ "${source}" != "Cargo.toml" ]]; then
  cargo_version="$(
    awk '
      /^\[package\]$/ { in_package = 1; next }
      /^\[workspace[.]package\]$/ { in_workspace_package = 1; next }
      /^\[/ { in_package = 0 }
      /^\[/ { in_workspace_package = 0 }
      in_package && /^version[[:space:]]*=/ {
        gsub(/"/, "", $3)
        print $3
        exit
      }
      in_workspace_package && /^version[[:space:]]*=/ {
        gsub(/"/, "", $3)
        print $3
        exit
      }
    ' "${ROOT_DIR}/Cargo.toml"
  )"
  if [[ "${version}" != "${cargo_version}" ]]; then
    echo "release version ${version} (from ${source}) does not match workspace Cargo.toml version ${cargo_version}; bump Cargo.toml so released binaries report the version they were published as" >&2
    exit 2
  fi
fi

emit_outputs() {
  echo "version=${version}"
  echo "version_source=${source}"
}

if [[ -n "${OUTPUT_PATH}" ]]; then
  emit_outputs >> "${OUTPUT_PATH}"
else
  emit_outputs
fi
