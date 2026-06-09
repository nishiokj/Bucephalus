#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INPUT="${1:-}"
WORK_DIR=""
RELEASE_DIR=""
RELEASE_ARCHIVE_INPUT=""
RELEASE_ROOT_NAME=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-buc-release.sh <release-dir-or-tar.gz>

Verifies a Bucephalus cloud release bundle:
  - archive .sha256, when a sibling checksum file exists
  - SHA256SUMS for every bundled file
  - release-manifest.json structure and source input digests
  - no local absolute path-shaped text in bundled release payloads
  - no retired deployment scripts, service units, or env examples leaked into deploy/
    outside the explicit Path 1 GCP provider surface
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

cloud_archive_member_ref() {
  local raw="$1"
  local lower public
  lower="$(printf '%s' "${raw}" | LC_ALL=C tr '[:upper:]' '[:lower:]')"
  case "${raw}" in
    ""|/*|*..*|*"\\"*)
      printf '%s' "archive-member://redacted"
      return 0
      ;;
  esac
  case "${lower}" in
    *secret*|*token*|*password*|*credential*|*api_key*|*private*|.env|*.env|*/.env|*/.env/*)
      printf '%s' "archive-member://redacted"
      return 0
      ;;
  esac
  public="$(printf '%s' "${raw}" | LC_ALL=C sed -e 's#[^A-Za-z0-9._/-]#_#g' -e 's#//*#/#g' -e 's#^/*##' -e 's#/*$##')"
  if [[ -z "${public}" ]]; then
    public="member"
  fi
  printf '%s' "archive-member://${public}"
}

verify_cloud_archive_members_before_extract() {
  local archive="$1"
  local members_file="${WORK_DIR}/cloud-archive-members.txt"
  local listing_file="${WORK_DIR}/cloud-archive-listing.txt"
  local duplicate member listing normalized entry_type top rel
  local root="" saw_root="false" saw_manifest="false" saw_checksums="false"

  tar -tzf "${archive}" > "${members_file}"
  tar -tvzf "${archive}" > "${listing_file}"

  if [[ ! -s "${members_file}" ]]; then
    echo "cloud release archive is empty" >&2
    exit 1
  fi

  duplicate="$(LC_ALL=C sort "${members_file}" | uniq -d | sed -n '1p')"
  if [[ -n "${duplicate}" ]]; then
    echo "cloud release archive contains duplicate tar entry" >&2
    echo "member_ref: $(cloud_archive_member_ref "${duplicate}")" >&2
    exit 1
  fi

  while IFS= read -r member && IFS= read -r listing <&3; do
    normalized="${member%/}"
    case "${member}" in
      ""|/*|*..*|*"\\"*)
        echo "unsafe cloud release archive member path" >&2
        echo "member_ref: $(cloud_archive_member_ref "${member}")" >&2
        exit 1
        ;;
    esac
    entry_type="${listing:0:1}"
    case "${entry_type}" in
      -|d) ;;
      *)
        echo "cloud release archive contains non-file tar entry" >&2
        echo "member_ref: $(cloud_archive_member_ref "${member}")" >&2
        exit 1
        ;;
    esac
    top="${normalized%%/*}"
    if [[ -z "${top}" ]]; then
      echo "unsafe cloud release archive member path" >&2
      echo "member_ref: $(cloud_archive_member_ref "${member}")" >&2
      exit 1
    fi
    if [[ -z "${root}" ]]; then
      root="${top}"
    fi
    if [[ "${top}" != "${root}" ]]; then
      echo "cloud release archive contains entry outside release directory" >&2
      echo "member_ref: $(cloud_archive_member_ref "${member}")" >&2
      exit 1
    fi
    if [[ "${normalized}" == "${root}" ]]; then
      if [[ "${entry_type}" != "d" ]]; then
        echo "cloud release archive root entry must be a directory" >&2
        echo "member_ref: $(cloud_archive_member_ref "${member}")" >&2
        exit 1
      fi
      saw_root="true"
      continue
    fi
    rel="${normalized#${root}/}"
    if [[ -z "${rel}" || "${rel}" == "${normalized}" ]]; then
      echo "cloud release archive contains entry outside release directory" >&2
      echo "member_ref: $(cloud_archive_member_ref "${member}")" >&2
      exit 1
    fi
    if [[ "${entry_type}" == "-" && "${rel}" == "release-manifest.json" ]]; then
      saw_manifest="true"
    fi
    if [[ "${entry_type}" == "-" && "${rel}" == "SHA256SUMS" ]]; then
      saw_checksums="true"
    fi
  done < "${members_file}" 3< "${listing_file}"

  if [[ "${saw_root}" != "true" ]]; then
    echo "cloud release archive is missing release root directory" >&2
    exit 1
  fi
  if [[ "${saw_manifest}" != "true" ]]; then
    echo "cloud release archive is missing release-manifest.json" >&2
    exit 1
  fi
  if [[ "${saw_checksums}" != "true" ]]; then
    echo "cloud release archive is missing SHA256SUMS" >&2
    exit 1
  fi

  printf '%s\n' "${root}"
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
  if [[ ! -e "${path}" || -L "${path}" || ! -f "${path}" ]]; then
    echo "checksum path must be a regular file: ${path}" >&2
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
require_command sed
require_command sort
require_command tar
require_command tr
require_command uniq

cleanup() {
  if [[ -n "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

if [[ -d "${INPUT}" ]]; then
  RELEASE_DIR="${INPUT}"
  RELEASE_ROOT_NAME="$(basename "${RELEASE_DIR}")"
elif [[ -f "${INPUT}" ]]; then
  RELEASE_ARCHIVE_INPUT="${INPUT}"
  if [[ -f "${INPUT}.sha256" ]]; then
    expected="$(read_archive_checksum "${INPUT}.sha256" "${INPUT}")"
    actual="$(sha256_file "${INPUT}")"
    if [[ "${expected}" != "${actual}" ]]; then
      echo "archive checksum mismatch for ${INPUT}: expected ${expected}, got ${actual}" >&2
      exit 1
    fi
  fi
  WORK_DIR="$(mktemp -d)"
  RELEASE_ROOT_NAME="$(verify_cloud_archive_members_before_extract "${INPUT}")"
  tar -xzf "${INPUT}" -C "${WORK_DIR}"
  RELEASE_DIR="${WORK_DIR}/${RELEASE_ROOT_NAME}"
  if [[ ! -d "${RELEASE_DIR}" ]]; then
    echo "archive did not contain the expected release directory" >&2
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
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { gunzipSync } from "node:zlib";

const releaseDir = process.env.RELEASE_DIR;
const releaseArchiveInput = process.env.RELEASE_ARCHIVE_INPUT || null;
const releaseRootName = process.env.RELEASE_ROOT_NAME || "";
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
    const stat = lstatSync(path);
    if (stat.isDirectory()) {
      out.push(...listFiles(path));
    } else if (stat.isFile()) {
      out.push(path);
    } else {
      fail(`cloud release contains non-regular file: ${relative(releaseDir, path).split("\\").join("/")}`);
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

function cString(block, start, length) {
  const bytes = block.subarray(start, start + length);
  const end = bytes.indexOf(0);
  return bytes.subarray(0, end === -1 ? bytes.length : end).toString("utf8");
}

function octal(block, start, length) {
  const text = cString(block, start, length).trim();
  return text === "" ? 0 : Number.parseInt(text, 8);
}

function blockIsZero(block) {
  return block.every((byte) => byte === 0);
}

function verifyArchiveHeaders(archivePath, rootName) {
  if (!rootName) {
    fail("cloud release archive root name is required");
  }
  const data = gunzipSync(readFileSync(archivePath));
  const seen = new Set();
  let sawRoot = false;
  let sawManifest = false;
  let sawChecksums = false;

  for (let offset = 0; offset + 512 <= data.length; offset += 512) {
    const header = data.subarray(offset, offset + 512);
    if (blockIsZero(header)) {
      break;
    }
    const name = cString(header, 0, 100);
    const prefix = cString(header, 345, 155);
    const path = prefix ? `${prefix}/${name}` : name;
    const normalizedPath = path.endsWith("/") ? path.slice(0, -1) : path;
    const type = cString(header, 156, 1) || "0";
    const uid = octal(header, 108, 8);
    const gid = octal(header, 116, 8);
    const size = octal(header, 124, 12);
    const mtime = octal(header, 136, 12);
    const uname = cString(header, 265, 32);
    const gname = cString(header, 297, 32);

    if (path === "" || path.startsWith("/") || path.includes("..") || path.includes("\\")) {
      fail(`cloud release archive contains unsafe tar entry: ${path}`);
    }
    if (seen.has(path)) {
      fail(`cloud release archive contains duplicate tar entry: ${path}`);
    }
    seen.add(path);
    if (normalizedPath !== rootName && !normalizedPath.startsWith(`${rootName}/`)) {
      fail(`cloud release archive contains entry outside release directory: ${path}`);
    }
    if (!["0", "5"].includes(type)) {
      fail(`cloud release archive contains non-file tar entry: ${path}`);
    }
    if (type === "5" && size !== 0) {
      fail(`cloud release archive directory entry has non-zero size: ${path}`);
    }
    if (uid !== 0 || gid !== 0) {
      fail(`cloud release archive tar entry ${path} must use uid/gid 0`);
    }
    if (!["", "root"].includes(uname) || !["", "root"].includes(gname)) {
      fail(`cloud release archive tar entry ${path} must not include local owner names`);
    }
    if (mtime !== 0) {
      fail(`cloud release archive tar entry ${path} must use normalized mtime 0`);
    }
    if (normalizedPath === rootName) {
      sawRoot = true;
    } else if (type === "0") {
      const relativePath = normalizedPath.slice(rootName.length + 1);
      if (relativePath === "release-manifest.json") {
        sawManifest = true;
      } else if (relativePath === "SHA256SUMS") {
        sawChecksums = true;
      }
    }
    offset += Math.ceil(size / 512) * 512;
  }

  if (!sawRoot) {
    fail(`cloud release archive is missing release root directory: ${rootName}`);
  }
  if (!sawManifest) {
    fail("cloud release archive is missing release-manifest.json");
  }
  if (!sawChecksums) {
    fail("cloud release archive is missing SHA256SUMS");
  }
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
  if (!lstatSync(path).isFile()) {
    fail(`${label}.path must be a regular file: ${entry.path}`);
  }
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

const localPathPatterns = [
  { label: "file URL local path", regex: /file:\/\/(?:\/(?:Users|home|private|tmp|var\/folders|Volumes|mnt\/[A-Za-z]\/Users)\/|\/[A-Za-z]:\/|[A-Za-z]:[\\/]|%[A-Z_]*(?:USERPROFILE|HOME|TEMP|TMP|APPDATA|LOCALAPPDATA)[A-Z_]*%[\\/]|~[\\/])/i },
  { label: "macOS home path", regex: /\/Users\/[^/\s"'`<>{}]+(?:\/[^\s"'`<>{}]*)?/ },
  { label: "Linux home path", regex: /\/home\/[^/\s"'`<>{}]+(?:\/[^\s"'`<>{}]*)?/ },
  { label: "Linux temp path", regex: /\/tmp\/[^\s"'`<>{}]+/ },
  { label: "macOS private temp path", regex: /\/private\/(?:tmp|var)\/[^\s"'`<>{}]+/ },
  { label: "macOS per-user temp path", regex: /\/var\/folders\/[^\s"'`<>{}]+/ },
  { label: "macOS mounted volume path", regex: /\/Volumes\/[^\s"'`<>{}]+/ },
  { label: "WSL mounted Windows user path", regex: /\/mnt\/[A-Za-z]\/Users\/[^\s"'`<>{}]+/ },
  { label: "Windows drive path", regex: /\b[A-Za-z]:[\\/][^\s"'`<>{}]+/ },
  { label: "Windows profile env path", regex: /%[A-Z_]*(?:USERPROFILE|HOME|TEMP|TMP|APPDATA|LOCALAPPDATA)[A-Z_]*%[\\/][^\s"'`<>{}]+/i },
  { label: "home-relative path", regex: /(^|[\s="'`<>{}\[\]()])~[\\/][^\s"'`<>{}]+/ },
  { label: "GitHub runner workspace path", regex: /\/__w\/[^\s"'`<>{}]+/ },
];

function isTextLike(data) {
  if (data.includes(0)) {
    return false;
  }
  const sample = data.subarray(0, Math.min(data.length, 8192)).toString("utf8");
  return !sample.includes("\uFFFD");
}

function verifyNoLocalPathContent() {
  for (const path of listFiles(releaseDir)) {
    const data = readFileSync(path);
    if (!isTextLike(data)) {
      continue;
    }
    const text = data.toString("utf8");
    const rel = relative(releaseDir, path).split("\\").join("/");
    for (const { label, regex } of localPathPatterns) {
      if (regex.test(text)) {
        fail(`${rel} contains ${label}; release text payloads must not embed local absolute paths`);
      }
    }
  }
}

if (releaseArchiveInput) {
  verifyArchiveHeaders(releaseArchiveInput, releaseRootName);
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
verifyNoLocalPathContent();

assertBundledFile(manifest.artifacts?.core_binary, "artifacts.core_binary");
assertBundledFile(manifest.artifacts?.worker_runner_binary, "artifacts.worker_runner_binary");
assertBundledFile(manifest.artifacts?.size_report, "artifacts.size_report");
assertBundledFile(manifest.source_inputs?.lockfiles?.cargo, "source_inputs.lockfiles.cargo");
assertBundledFile(manifest.source_inputs?.lockfiles?.cloud_bun, "source_inputs.lockfiles.cloud_bun");
assertBundledFile(manifest.source_inputs?.lockfiles?.cloud_runtime_bun, "source_inputs.lockfiles.cloud_runtime_bun");
assertBundledFile(manifest.source_inputs?.cloud_package, "source_inputs.cloud_package");
assertBundledFile(manifest.source_inputs?.cloud_runtime_package, "source_inputs.cloud_runtime_package");
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

const sizeReport = readJson(join(releaseDir, manifest.artifacts.size_report.path));
if (sizeReport.schema_version !== "bucephalus_release_size_report_v1") {
  fail("release-size-report.json schema_version must be bucephalus_release_size_report_v1");
}
if (!sizeReport.total || !Number.isSafeInteger(sizeReport.total.size_bytes) || sizeReport.total.size_bytes <= 0) {
  fail("release-size-report.json total.size_bytes must be a positive integer");
}
if (!Number.isSafeInteger(sizeReport.total.file_count) || sizeReport.total.file_count <= 0) {
  fail("release-size-report.json total.file_count must be a positive integer");
}
if (!Array.isArray(sizeReport.sections) || sizeReport.sections.length === 0) {
  fail("release-size-report.json sections must be a non-empty array");
}
const requiredSizeSections = new Set([
  "bin",
  "release-inputs",
  "bucephalus-cloud/runtime-dist",
  "bucephalus-cloud/db",
  "bucephalus-cloud/images",
  "bucephalus-cloud/src",
  "bucephalus-cloud/api",
  "bucephalus-cloud/deploy",
  "bucephalus-cloud/infra",
]);
let reportedBytes = 0;
let reportedFiles = 0;
for (const section of sizeReport.sections) {
  if (!requiredSizeSections.has(section.path)) {
    fail(`release-size-report.json contains unexpected section: ${section.path}`);
  }
  requiredSizeSections.delete(section.path);
  if (!Number.isSafeInteger(section.size_bytes) || section.size_bytes < 0) {
    fail(`release-size-report.json ${section.path}.size_bytes must be a non-negative integer`);
  }
  if (!Number.isSafeInteger(section.file_count) || section.file_count < 0) {
    fail(`release-size-report.json ${section.path}.file_count must be a non-negative integer`);
  }
  if (!Array.isArray(section.files) || section.files.length !== section.file_count) {
    fail(`release-size-report.json ${section.path}.files must match file_count`);
  }
  const sectionBytes = section.files.reduce((sum, file) => {
    if (typeof file.path !== "string" || file.path.startsWith("/") || file.path.includes("..") || file.path.includes("\\")) {
      fail(`release-size-report.json contains unsafe file path in ${section.path}`);
    }
    if (!file.path.startsWith(`${section.path}/`) && file.path !== section.path) {
      fail(`release-size-report.json file ${file.path} is outside section ${section.path}`);
    }
    if (!Number.isSafeInteger(file.size_bytes) || file.size_bytes < 0) {
      fail(`release-size-report.json ${file.path}.size_bytes must be a non-negative integer`);
    }
    assertSha(file.sha256, `release-size-report.json ${file.path}.sha256`);
    const realPath = join(releaseDir, file.path);
    const stat = lstatSync(realPath);
    if (!stat.isFile()) {
      fail(`release-size-report.json file does not exist: ${file.path}`);
    }
    if (stat.size !== file.size_bytes) {
      fail(`release-size-report.json size mismatch for ${file.path}`);
    }
    if (sha256File(realPath) !== file.sha256) {
      fail(`release-size-report.json digest mismatch for ${file.path}`);
    }
    return sum + file.size_bytes;
  }, 0);
  if (sectionBytes !== section.size_bytes) {
    fail(`release-size-report.json section byte total mismatch for ${section.path}`);
  }
  reportedBytes += section.size_bytes;
  reportedFiles += section.file_count;
}
if (requiredSizeSections.size > 0) {
  fail(`release-size-report.json is missing sections: ${[...requiredSizeSections].sort().join(", ")}`);
}
if (reportedBytes !== sizeReport.total.size_bytes || reportedFiles !== sizeReport.total.file_count) {
  fail("release-size-report.json total does not match section totals");
}

const allowedDeployProviderPayloads = new Set([
  "provider/gcp/gce-provider-common.js",
  "provider/gcp/provision-runner-vm.js",
  "provider/gcp/reap-runner-vm.js",
]);
const deployFiles = listFiles(join(releaseDir, "bucephalus-cloud", "deploy"))
  .map((path) => relative(join(releaseDir, "bucephalus-cloud", "deploy"), path).split("\\").join("/"));
const unexpectedDeployFiles = deployFiles.filter(
  (path) => !path.endsWith(".md") && !allowedDeployProviderPayloads.has(path),
);
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

RELEASE_DIR="${RELEASE_DIR}" ROOT_DIR="${ROOT_DIR}" RELEASE_ARCHIVE_INPUT="${RELEASE_ARCHIVE_INPUT}" RELEASE_ROOT_NAME="${RELEASE_ROOT_NAME}" bun "${VERIFY_JS}"
