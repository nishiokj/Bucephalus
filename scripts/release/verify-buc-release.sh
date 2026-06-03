#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INPUT="${1:-}"
WORK_DIR=""
RELEASE_DIR=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-buc-release.sh <release-dir-or-tar.gz>

Verifies a Bucephalus cloud release bundle:
  - archive .sha256, when a sibling checksum file exists
  - SHA256SUMS for every bundled file
  - release-manifest.json structure and source input digests
  - no retired deployment scripts, service units, or env examples leaked into deploy/
USAGE
}

if [[ -z "${INPUT}" || "${INPUT}" == "-h" || "${INPUT}" == "--help" ]]; then
  usage
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

read_archive_checksum() {
  local checksum_file="$1"
  local archive="$2"
  local expected_name line expected checksum_name extra current line_count
  expected_name="$(basename "${archive}")"
  line=""
  line_count=0
  while IFS= read -r current || [[ -n "${current}" ]]; do
    line_count=$((line_count + 1))
    if [[ ${line_count} -eq 1 ]]; then
      line="${current}"
    fi
  done < "${checksum_file}"
  if [[ ${line_count} -ne 1 ]]; then
    echo "archive checksum file must contain exactly one record: ${checksum_file}" >&2
    exit 1
  fi
  read -r expected checksum_name extra <<< "${line}"
  if [[ -n "${extra:-}" || ! "${expected}" =~ ^[a-f0-9]{64}$ || "${checksum_name}" != "${expected_name}" || "${line}" != "${expected}  ${expected_name}" ]]; then
    echo "malformed archive checksum file: ${checksum_file}" >&2
    exit 1
  fi
  printf '%s\n' "${expected}"
}

verify_checksum_record() {
  local line="$1"
  local line_number="$2"
  local expected path actual
  if [[ -z "${line}" || "${line}" != *"  "* ]]; then
    echo "malformed bundled checksum line ${line_number}" >&2
    exit 1
  fi
  expected="${line%%  *}"
  path="${line#*  }"
  if [[ ! "${expected}" =~ ^[a-f0-9]{64}$ || -z "${path}" || "${line}" != "${expected}  ${path}" || "${path}" == *"  "* ]]; then
    echo "malformed bundled checksum line ${line_number}" >&2
    exit 1
  fi
  if [[ "${path}" = /* || "${path}" == *".."* ]]; then
    echo "unsafe checksum path: ${path}" >&2
    exit 1
  fi
  if [[ ! -f "${path}" ]]; then
    echo "checksum path is missing: ${path}" >&2
    exit 1
  fi
  actual="$(sha256_file "${path}")"
  if [[ "${expected}" != "${actual}" ]]; then
    echo "checksum mismatch for ${path}: expected ${expected}, got ${actual}" >&2
    exit 1
  fi
}

require_command awk
require_command bun
require_command tar

cleanup() {
  if [[ -n "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

if [[ -d "${INPUT}" ]]; then
  RELEASE_DIR="${INPUT}"
elif [[ -f "${INPUT}" ]]; then
  if [[ -f "${INPUT}.sha256" ]]; then
    expected="$(read_archive_checksum "${INPUT}.sha256" "${INPUT}")"
    actual="$(sha256_file "${INPUT}")"
    if [[ "${expected}" != "${actual}" ]]; then
      echo "archive checksum mismatch for ${INPUT}: expected ${expected}, got ${actual}" >&2
      exit 1
    fi
  fi
  WORK_DIR="$(mktemp -d)"
  tar -xzf "${INPUT}" -C "${WORK_DIR}"
  RELEASE_DIR="$(find "${WORK_DIR}" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
  if [[ -z "${RELEASE_DIR}" ]]; then
    echo "archive did not contain a release directory: ${INPUT}" >&2
    exit 1
  fi
else
  echo "release input does not exist: ${INPUT}" >&2
  exit 2
fi

if [[ ! -f "${RELEASE_DIR}/SHA256SUMS" ]]; then
  echo "missing SHA256SUMS in ${RELEASE_DIR}" >&2
  exit 1
fi

(
  cd "${RELEASE_DIR}"
  line_number=0
  while IFS= read -r line || [[ -n "${line}" ]]; do
    line_number=$((line_number + 1))
    verify_checksum_record "${line}" "${line_number}"
  done < SHA256SUMS
)

if [[ -z "${WORK_DIR}" ]]; then
  WORK_DIR="$(mktemp -d)"
fi
VERIFY_JS="${WORK_DIR}/verify-buc-release.mjs"
cat > "${VERIFY_JS}" <<'JS'
import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const releaseDir = process.env.RELEASE_DIR;
const hex64 = /^[0-9a-f]{64}$/;
const gitSha = /^[0-9a-f]{40}$/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`failed to parse ${path}: ${error.message}`);
  }
}

function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function listFiles(dir) {
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    const stat = statSync(path);
    if (stat.isDirectory()) {
      out.push(...listFiles(path));
    } else if (stat.isFile()) {
      out.push(path);
    }
  }
  return out;
}

function sha256Tree(dir) {
  const lines = listFiles(dir).map((path) => {
    const rel = relative(dir, path).split("\\").join("/");
    return `${sha256File(path)}  ${rel}\n`;
  });
  return createHash("sha256").update(lines.join("")).digest("hex");
}

function assertSha(value, label) {
  if (typeof value !== "string" || !hex64.test(value)) {
    fail(`${label} must be a lowercase sha256 hex digest`);
  }
}

function assertBundledFile(entry, label) {
  if (!entry || typeof entry.path !== "string") {
    fail(`${label}.path is required`);
  }
  if (entry.path.startsWith("/") || entry.path.includes("..")) {
    fail(`${label}.path is unsafe: ${entry.path}`);
  }
  assertSha(entry.sha256, `${label}.sha256`);
  const path = join(releaseDir, entry.path);
  if (sha256File(path) !== entry.sha256) {
    fail(`${label} digest does not match ${entry.path}`);
  }
}

function verifyChecksumManifest() {
  const checksumText = readFileSync(join(releaseDir, "SHA256SUMS"), "utf8");
  const records = checksumText.split("\n").filter((line) => line.length > 0);
  const seen = new Set();
  for (const [i, line] of records.entries()) {
    const separator = line.indexOf("  ");
    if (separator !== 64 || line.indexOf("  ", separator + 2) !== -1) {
      fail(`malformed bundled checksum line ${i + 1}`);
    }
    const digest = line.slice(0, separator);
    const path = line.slice(separator + 2);
    if (!hex64.test(digest) || path === "" || path.startsWith("/") || path.includes("..") || path.includes("\\")) {
      fail(`malformed bundled checksum line ${i + 1}`);
    }
    if (seen.has(path)) {
      fail(`duplicate bundled checksum path: ${path}`);
    }
    seen.add(path);
  }

  const expected = new Set(
    listFiles(releaseDir)
      .map((path) => relative(releaseDir, path).split("\\").join("/"))
      .filter((path) => path !== "SHA256SUMS"),
  );
  for (const path of expected) {
    if (!seen.has(path)) {
      fail(`SHA256SUMS is missing bundled file: ${path}`);
    }
  }
  for (const path of seen) {
    if (!expected.has(path)) {
      fail(`SHA256SUMS references unexpected file: ${path}`);
    }
  }
}

const manifest = readJson(join(releaseDir, "release-manifest.json"));
if (manifest.schema_version !== "bucephalus_release_v1") {
  fail("release-manifest.json schema_version must be bucephalus_release_v1");
}
if (typeof manifest.version !== "string" || manifest.version.trim() === "") {
  fail("release-manifest.json version is required");
}
if (typeof manifest.git_sha !== "string" || !gitSha.test(manifest.git_sha)) {
  fail("git_sha must be a 40-character lowercase git object id");
}
if (typeof manifest.git_dirty !== "boolean") {
  fail("git_dirty must be boolean");
}
if (typeof manifest.target !== "string" || manifest.target.trim() === "") {
  fail("target is required");
}
verifyChecksumManifest();

assertBundledFile(manifest.artifacts?.core_binary, "artifacts.core_binary");
assertBundledFile(manifest.source_inputs?.lockfiles?.cargo, "source_inputs.lockfiles.cargo");
assertBundledFile(manifest.source_inputs?.lockfiles?.cloud_bun, "source_inputs.lockfiles.cloud_bun");
assertBundledFile(manifest.source_inputs?.cloud_package, "source_inputs.cloud_package");
assertBundledFile(manifest.source_inputs?.image_context_ignore, "source_inputs.image_context_ignore");

const dockerignoreText = readFileSync(join(releaseDir, ".dockerignore"), "utf8");
for (const requiredPattern of [
  "gha-creds-*.json",
  "**/gha-creds-*.json",
  "*.env",
  "**/*.env",
  "*.env.example",
  "**/*.env.example",
  "node_modules/",
  "**/node_modules/",
  ".terraform/",
  "**/.terraform/",
  "*.tfstate",
  "**/*.tfstate",
  "image-build/",
  "**/image-build/",
  "*.metadata.json",
  "*.iid",
]) {
  if (!dockerignoreText.split("\n").includes(requiredPattern)) {
    fail(`.dockerignore is missing required image context exclusion: ${requiredPattern}`);
  }
}

for (const [key, entry] of Object.entries(manifest.source_inputs?.content_sets ?? {})) {
  if (!entry || typeof entry.path !== "string") {
    fail(`source_inputs.content_sets.${key}.path is required`);
  }
  if (entry.path.startsWith("/") || entry.path.includes("..")) {
    fail(`source_inputs.content_sets.${key}.path is unsafe: ${entry.path}`);
  }
  assertSha(entry.tree_sha256, `source_inputs.content_sets.${key}.tree_sha256`);
  const dir = join(releaseDir, entry.path);
  if (sha256Tree(dir) !== entry.tree_sha256) {
    fail(`source_inputs.content_sets.${key} tree digest does not match ${entry.path}`);
  }
}

const deployFiles = listFiles(join(releaseDir, "bucephalus-cloud", "deploy"))
  .map((path) => relative(join(releaseDir, "bucephalus-cloud", "deploy"), path).split("\\").join("/"));
const unexpectedDeployFiles = deployFiles.filter((path) => !path.endsWith(".md"));
if (unexpectedDeployFiles.length > 0) {
  fail(`retired deploy payload leaked into release: ${unexpectedDeployFiles.join(", ")}`);
}

const checksumPaths = readFileSync(join(releaseDir, "SHA256SUMS"), "utf8")
  .split("\n")
  .filter(Boolean)
  .map((line) => line.slice(66));
const envExamples = checksumPaths.filter((path) => /(^|\/)[^/]*\.env(\.example)?$/.test(path) || path.endsWith(".env.example"));
if (envExamples.length > 0) {
  fail(`env example or env file leaked into release: ${envExamples.join(", ")}`);
}

console.log(`verified ${releaseDir}`);
JS

RELEASE_DIR="${RELEASE_DIR}" ROOT_DIR="${ROOT_DIR}" bun "${VERIFY_JS}"
