#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RELEASE_INPUT=""
OUT_PATH=""
WORK_DIR=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/write-core-release-provenance.sh --release <release-dir-or-tar.gz> --out <path>

Writes unsigned, recorded provenance for a Bucephalus Core CLI release archive.
The output is not a signature; it records source revision, target, archive
digest, manifest digest, and core binary digest.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      RELEASE_INPUT="${2:-}"
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

if [[ -z "${RELEASE_INPUT}" || -z "${OUT_PATH}" ]]; then
  usage >&2
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

cleanup() {
  if [[ -n "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

require_command bun
require_command tar

"${ROOT_DIR}/scripts/release/verify-core-release.sh" "${RELEASE_INPUT}"

RELEASE_ARCHIVE_SHA=""
RELEASE_INPUT_KIND="directory"
if [[ -d "${RELEASE_INPUT}" ]]; then
  RELEASE_DIR="${RELEASE_INPUT}"
else
  RELEASE_INPUT_KIND="archive"
  RELEASE_ARCHIVE_SHA="$(sha256_file "${RELEASE_INPUT}")"
  WORK_DIR="$(mktemp -d)"
  tar -xzf "${RELEASE_INPUT}" -C "${WORK_DIR}"
  RELEASE_DIR="${WORK_DIR}"
fi

mkdir -p "$(dirname "${OUT_PATH}")"
if [[ -z "${WORK_DIR}" ]]; then
  WORK_DIR="$(mktemp -d)"
fi
WRITE_JS="${WORK_DIR}/write-core-release-provenance.mjs"
cat > "${WRITE_JS}" <<'JS'
import { createHash } from "node:crypto";

const manifest = JSON.parse(await Bun.file(`${process.env.RELEASE_DIR}/release-manifest.json`).text());
const isGithubActions = process.env.GITHUB_ACTIONS === "true";
const provenance = {
  schema_version: "bucephalus_core_release_provenance_v1",
  predicate_type: "https://bucephalus.dev/provenance/core-release/v1",
  generated_at: new Date().toISOString(),
  release: {
    input_kind: process.env.RELEASE_INPUT_KIND,
    version: manifest.version,
    target: manifest.target,
    git_sha: manifest.git_sha,
    git_dirty: manifest.git_dirty,
    archive_sha256: process.env.RELEASE_ARCHIVE_SHA || null,
    manifest_sha256: process.env.RELEASE_MANIFEST_SHA,
  },
  artifacts: {
    core_binary: manifest.artifacts.core_binary,
  },
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
  signature: {
    status: "unsigned",
    reason: "registry or release signing boundary is not configured",
  },
};
provenance.provenance_sha256 = createHash("sha256").update(JSON.stringify({
  ...provenance,
  provenance_sha256: null,
})).digest("hex");
await Bun.write(process.env.OUT_PATH, `${JSON.stringify(provenance, null, 2)}\n`);
JS

RELEASE_DIR="${RELEASE_DIR}" \
RELEASE_INPUT_KIND="${RELEASE_INPUT_KIND}" \
RELEASE_ARCHIVE_SHA="${RELEASE_ARCHIVE_SHA}" \
RELEASE_MANIFEST_SHA="$(sha256_file "${RELEASE_DIR}/release-manifest.json")" \
OUT_PATH="${OUT_PATH}" \
bun "${WRITE_JS}"

"${ROOT_DIR}/scripts/release/verify-core-release-provenance.sh" "${OUT_PATH}" --release "${RELEASE_INPUT}"
echo "provenance=${OUT_PATH}"
