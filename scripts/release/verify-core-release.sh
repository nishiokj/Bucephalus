#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INPUT="${1:-}"
WORK_DIR=""
RELEASE_DIR=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-core-release.sh <release-dir-or-tar.gz>

Verifies a Bucephalus Core CLI release archive:
  - archive .sha256, when a sibling checksum file exists
  - SHA256SUMS for bundled files
  - release-manifest.json structure and packaged binary digests
  - no env files or deployment payloads in the public CLI archive
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
  local expected path
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

cleanup() {
  if [[ -n "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

require_command awk
require_command bun
require_command tar

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
  RELEASE_DIR="${WORK_DIR}"
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

VERIFY_WORK_DIR="$(mktemp -d)"
cleanup_verify() {
  rm -rf "${VERIFY_WORK_DIR}"
}
trap 'cleanup_verify; cleanup' EXIT
VERIFY_JS="${VERIFY_WORK_DIR}/verify-core-release.mjs"
cat > "${VERIFY_JS}" <<'JS'
import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const releaseDir = process.env.RELEASE_DIR;
const sha256 = /^[a-f0-9]{64}$/;
const gitSha = /^[a-f0-9]{40}$/;
const expectedFiles = new Set(["bucephalus", "bucephalus-cloud", "bucephalus-modal-launcher", "README.md", "LICENSE", "release-manifest.json", "SHA256SUMS"]);

function fail(message) {
  console.error(message);
  process.exit(1);
}

function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function listFiles(dir, base = dir) {
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    const stat = statSync(path);
    if (stat.isDirectory()) {
      out.push(...listFiles(path, base));
    } else if (stat.isFile()) {
      out.push(relative(base, path).split("\\").join("/"));
    }
  }
  return out;
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
    if (!sha256.test(digest) || path === "" || path.startsWith("/") || path.includes("..") || path.includes("\\")) {
      fail(`malformed bundled checksum line ${i + 1}`);
    }
    if (seen.has(path)) {
      fail(`duplicate bundled checksum path: ${path}`);
    }
    seen.add(path);
  }

  const expected = new Set(listFiles(releaseDir).filter((file) => file !== "SHA256SUMS"));
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

const files = listFiles(releaseDir);
for (const file of files) {
  if (!expectedFiles.has(file)) {
    fail(`unexpected file in core release archive: ${file}`);
  }
  if (/(^|\/)[^/]*\.env(\.example)?$/.test(file) || file.endsWith(".env.example")) {
    fail(`env file leaked into core release: ${file}`);
  }
}
for (const file of expectedFiles) {
  if (!files.includes(file)) {
    fail(`missing expected core release file: ${file}`);
  }
}
for (const file of ["bucephalus", "bucephalus-cloud", "bucephalus-modal-launcher"]) {
  if ((statSync(join(releaseDir, file)).mode & 0o111) === 0) {
    fail(`${file} must be executable`);
  }
}
verifyChecksumManifest();

const manifest = JSON.parse(readFileSync(join(releaseDir, "release-manifest.json"), "utf8"));
if (manifest.schema_version !== "bucephalus_core_release_v1") {
  fail("release-manifest.json schema_version must be bucephalus_core_release_v1");
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
const core = manifest.artifacts?.core_binary;
if (!core || core.path !== "bucephalus" || typeof core.sha256 !== "string" || !sha256.test(core.sha256)) {
  fail("artifacts.core_binary must point at bucephalus with a sha256 digest");
}
if (sha256File(join(releaseDir, "bucephalus")) !== core.sha256) {
  fail("core binary digest does not match release manifest");
}

const cloudCli = manifest.artifacts?.cloud_cli_binary;
if (!cloudCli || cloudCli.path !== "bucephalus-cloud" || typeof cloudCli.sha256 !== "string" || !sha256.test(cloudCli.sha256)) {
  fail("artifacts.cloud_cli_binary must point at bucephalus-cloud with a sha256 digest");
}
if (sha256File(join(releaseDir, "bucephalus-cloud")) !== cloudCli.sha256) {
  fail("Cloud CLI binary digest does not match release manifest");
}

const modalLauncher = manifest.artifacts?.modal_launcher_binary;
if (!modalLauncher || modalLauncher.path !== "bucephalus-modal-launcher" || typeof modalLauncher.sha256 !== "string" || !sha256.test(modalLauncher.sha256)) {
  fail("artifacts.modal_launcher_binary must point at bucephalus-modal-launcher with a sha256 digest");
}
if (sha256File(join(releaseDir, "bucephalus-modal-launcher")) !== modalLauncher.sha256) {
  fail("Modal launcher binary digest does not match release manifest");
}

console.log(`verified core release ${releaseDir}`);
JS

RELEASE_DIR="${RELEASE_DIR}" ROOT_DIR="${ROOT_DIR}" bun "${VERIFY_JS}"
