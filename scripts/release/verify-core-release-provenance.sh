#!/usr/bin/env bash
set -euo pipefail

PROVENANCE=""
RELEASE_INPUT=""
WORK_DIR=""
RELEASE_DIR=""
RELEASE_ARCHIVE_SHA=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-core-release-provenance.sh <core-release-provenance.json> [--release <release-dir-or-tar.gz>]

Validates unsigned Bucephalus Core release provenance. When --release is
provided, also verifies that the provenance describes that exact release
artifact or directory.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      RELEASE_INPUT="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [[ -z "${PROVENANCE}" ]]; then
        PROVENANCE="$1"
        shift
      else
        echo "unknown argument: $1" >&2
        usage >&2
        exit 2
      fi
      ;;
  esac
done

if [[ -z "${PROVENANCE}" ]]; then
  usage
  exit 2
fi

if [[ ! -f "${PROVENANCE}" ]]; then
  echo "provenance does not exist: ${PROVENANCE}" >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${file}" | awk "{print \$1}"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${file}" | awk "{print \$1}"
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

if [[ -n "${RELEASE_INPUT}" ]]; then
  if [[ -d "${RELEASE_INPUT}" ]]; then
    RELEASE_DIR="${RELEASE_INPUT}"
  elif [[ -f "${RELEASE_INPUT}" ]]; then
    if ! command -v tar >/dev/null 2>&1; then
      echo "required command not found: tar" >&2
      exit 2
    fi
    RELEASE_ARCHIVE_SHA="$(sha256_file "${RELEASE_INPUT}")"
    WORK_DIR="$(mktemp -d)"
    tar -xzf "${RELEASE_INPUT}" -C "${WORK_DIR}"
    RELEASE_DIR="${WORK_DIR}"
  else
    echo "release input does not exist: ${RELEASE_INPUT}" >&2
    exit 2
  fi
  if [[ ! -f "${RELEASE_DIR}/release-manifest.json" ]]; then
    echo "release input is missing release-manifest.json: ${RELEASE_INPUT}" >&2
    exit 1
  fi
fi

RELEASE_DIR="${RELEASE_DIR}" RELEASE_ARCHIVE_SHA="${RELEASE_ARCHIVE_SHA}" bun -e '
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
const path = process.argv[1];
const provenance = JSON.parse(await Bun.file(path).text());
const sha256 = /^[a-f0-9]{64}$/;
const gitSha = /^[a-f0-9]{40}$/;
const releaseDir = process.env.RELEASE_DIR;
const releaseArchiveSha = process.env.RELEASE_ARCHIVE_SHA || null;

function fail(message) {
  console.error(message);
  process.exit(1);
}
function checkSha(value, label) {
  if (typeof value !== "string" || !sha256.test(value)) {
    fail(`${label} must be a lowercase sha256 digest`);
  }
}
function recompute(record) {
  return createHash("sha256").update(JSON.stringify({ ...record, provenance_sha256: null })).digest("hex");
}
function hashFile(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}
function checkBuilder(builder, release) {
  if (!builder || typeof builder !== "object") {
    fail("builder object is required");
  }
  if (!["local", "github_actions"].includes(builder.kind)) {
    fail("builder.kind must be local or github_actions");
  }

  const githubFields = [
    "github_server_url",
    "github_repository",
    "github_run_id",
    "github_run_attempt",
    "github_workflow",
    "github_ref",
    "github_sha",
  ];
  if (builder.kind === "local") {
    for (const field of githubFields) {
      if (builder[field] !== null) {
        fail(`local builder must not claim ${field}`);
      }
    }
    return;
  }

  if (builder.github_server_url !== "https://github.com") {
    fail("builder.github_server_url must be https://github.com");
  }
  if (typeof builder.github_repository !== "string" || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(builder.github_repository)) {
    fail("builder.github_repository must be an owner/repo slug");
  }
  if (typeof builder.github_run_id !== "string" || !/^[0-9]+$/.test(builder.github_run_id)) {
    fail("builder.github_run_id must be numeric");
  }
  if (typeof builder.github_run_attempt !== "string" || !/^[0-9]+$/.test(builder.github_run_attempt)) {
    fail("builder.github_run_attempt must be numeric");
  }
  if (typeof builder.github_workflow !== "string" || builder.github_workflow.trim() === "") {
    fail("builder.github_workflow is required for GitHub Actions provenance");
  }
  if (typeof builder.github_ref !== "string" || !builder.github_ref.startsWith("refs/")) {
    fail("builder.github_ref must be a full refs/ value");
  }
  if (typeof builder.github_sha !== "string" || !gitSha.test(builder.github_sha)) {
    fail("builder.github_sha must be a 40-character lowercase git object id");
  }
  if (builder.github_sha !== release.git_sha) {
    fail("builder.github_sha must match release.git_sha");
  }
}

if (provenance.schema_version !== "bucephalus_core_release_provenance_v1") {
  fail("schema_version must be bucephalus_core_release_provenance_v1");
}
if (provenance.predicate_type !== "https://bucephalus.dev/provenance/core-release/v1") {
  fail("predicate_type is not recognized");
}
if (Number.isNaN(Date.parse(provenance.generated_at))) {
  fail("generated_at must be an ISO timestamp");
}
if (!["archive", "directory"].includes(provenance.release?.input_kind)) {
  fail("release.input_kind must be archive or directory");
}
if (typeof provenance.release.version !== "string" || provenance.release.version.trim() === "") {
  fail("release.version is required");
}
if (typeof provenance.release.target !== "string" || provenance.release.target.trim() === "") {
  fail("release.target is required");
}
if (typeof provenance.release.git_sha !== "string" || !gitSha.test(provenance.release.git_sha)) {
  fail("release.git_sha must be a 40-character lowercase git object id");
}
if (typeof provenance.release.git_dirty !== "boolean") {
  fail("release.git_dirty must be boolean");
}
if (provenance.release.archive_sha256 !== null) {
  checkSha(provenance.release.archive_sha256, "release.archive_sha256");
}
checkSha(provenance.release.manifest_sha256, "release.manifest_sha256");
if (provenance.artifacts?.core_binary?.path !== "bucephalus") {
  fail("artifacts.core_binary.path must be bucephalus");
}
checkSha(provenance.artifacts.core_binary.sha256, "artifacts.core_binary.sha256");
if (provenance.artifacts?.hosted_cli_binary?.path !== "buc") {
  fail("artifacts.hosted_cli_binary.path must be buc");
}
checkSha(provenance.artifacts.hosted_cli_binary.sha256, "artifacts.hosted_cli_binary.sha256");
if (provenance.artifacts?.modal_launcher_binary?.path !== "bucephalus-modal-launcher") {
  fail("artifacts.modal_launcher_binary.path must be bucephalus-modal-launcher");
}
checkSha(provenance.artifacts.modal_launcher_binary.sha256, "artifacts.modal_launcher_binary.sha256");
checkBuilder(provenance.builder, provenance.release);
if (releaseDir) {
  const manifestPath = join(releaseDir, "release-manifest.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (hashFile(manifestPath) !== provenance.release.manifest_sha256) {
    fail("release.manifest_sha256 does not match release manifest");
  }
  if (releaseArchiveSha && provenance.release.archive_sha256 !== releaseArchiveSha) {
    fail("release.archive_sha256 does not match release archive");
  }
  for (const field of ["version", "target", "git_sha", "git_dirty"]) {
    if (provenance.release[field] !== manifest[field]) {
      fail(`release.${field} does not match release manifest`);
    }
  }
  if (JSON.stringify(provenance.artifacts.core_binary) !== JSON.stringify(manifest.artifacts?.core_binary)) {
    fail("artifacts.core_binary does not match release manifest");
  }
  if (JSON.stringify(provenance.artifacts.hosted_cli_binary) !== JSON.stringify(manifest.artifacts?.hosted_cli_binary)) {
    fail("artifacts.hosted_cli_binary does not match release manifest");
  }
  if (JSON.stringify(provenance.artifacts.modal_launcher_binary) !== JSON.stringify(manifest.artifacts?.modal_launcher_binary)) {
    fail("artifacts.modal_launcher_binary does not match release manifest");
  }
}
if (provenance.signature?.status !== "unsigned") {
  fail("signature.status must be unsigned until a signing boundary exists");
}
checkSha(provenance.provenance_sha256, "provenance_sha256");
if (provenance.provenance_sha256 !== recompute(provenance)) {
  fail("provenance_sha256 does not match provenance content");
}
console.log(`verified core release provenance ${path}`);
' "${PROVENANCE}"
