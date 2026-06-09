#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INPUT="${1:-}"
WORK_DIR=""
RELEASE_DIR=""
RELEASE_ARCHIVE_INPUT=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-core-release.sh <release-dir-or-tar.gz>

Verifies a Bucephalus Core CLI release archive:
  - archive .sha256, when a sibling checksum file exists
  - SHA256SUMS for bundled files
  - release-manifest.json structure and packaged binary digests
  - no local absolute path-shaped text in bundled release payloads
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

CORE_RELEASE_EXPECTED_FILES=(
  bucephalus
  bucephalus-cloud
  bucephalus-modal-launcher
  install.sh
  README.md
  LICENSE
  release-manifest.json
  SHA256SUMS
)

core_release_expected_file() {
  local candidate="$1"
  local expected
  for expected in "${CORE_RELEASE_EXPECTED_FILES[@]}"; do
    if [[ "${candidate}" == "${expected}" ]]; then
      return 0
    fi
  done
  return 1
}

core_archive_member_ref() {
  local raw="$1"
  local lower public
  lower="$(printf '%s' "${raw}" | LC_ALL=C tr '[:upper:]' '[:lower:]')"
  case "${raw}" in
    ""|/*|*"/../"*|../*|*"/.."|*"\\"*)
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

verify_core_archive_members_before_extract() {
  local archive="$1"
  local members_file="${WORK_DIR}/core-archive-members.txt"
  local listing_file="${WORK_DIR}/core-archive-listing.txt"
  local member listing expected member_count actual_count

  tar -tzf "${archive}" > "${members_file}"
  tar -tvzf "${archive}" > "${listing_file}"

  actual_count="$(sed -n '$=' "${members_file}")"
  if [[ "${actual_count:-0}" != "${#CORE_RELEASE_EXPECTED_FILES[@]}" ]]; then
    echo "core release archive must contain exactly ${#CORE_RELEASE_EXPECTED_FILES[@]} files; found ${actual_count:-0}" >&2
    exit 1
  fi

  while IFS= read -r member || [[ -n "${member}" ]]; do
    case "${member}" in
      ""|/*|*"/../"*|../*|*"/.."|*"\\"*)
        echo "unsafe core release archive member path" >&2
        echo "member_ref: $(core_archive_member_ref "${member}")" >&2
        exit 1
        ;;
    esac
    if ! core_release_expected_file "${member}"; then
      echo "core release archive contains unexpected tar entry" >&2
      echo "member_ref: $(core_archive_member_ref "${member}")" >&2
      exit 1
    fi
  done < "${members_file}"

  while IFS= read -r listing || [[ -n "${listing}" ]]; do
    case "${listing}" in
      -*) ;;
      *)
        echo "core release archive contains non-file tar entry" >&2
        exit 1
        ;;
    esac
  done < "${listing_file}"

  for expected in "${CORE_RELEASE_EXPECTED_FILES[@]}"; do
    member_count=0
    while IFS= read -r member || [[ -n "${member}" ]]; do
      if [[ "${member}" == "${expected}" ]]; then
        member_count=$((member_count + 1))
      fi
    done < "${members_file}"
    if [[ "${member_count}" -eq 0 ]]; then
      echo "core release archive is missing tar entry: ${expected}" >&2
      exit 1
    fi
    if [[ "${member_count}" -gt 1 ]]; then
      echo "core release archive contains duplicate tar entry: ${expected}" >&2
      exit 1
    fi
  done
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

cleanup() {
  if [[ -n "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

require_command awk
require_command bun
require_command sed
require_command tar
require_command tr

if [[ -d "${INPUT}" ]]; then
  RELEASE_DIR="${INPUT}"
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
  verify_core_archive_members_before_extract "${INPUT}"
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
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { gunzipSync } from "node:zlib";

const releaseDir = process.env.RELEASE_DIR;
const releaseArchiveInput = process.env.RELEASE_ARCHIVE_INPUT || null;
const sha256 = /^[a-f0-9]{64}$/;
const gitSha = /^[a-f0-9]{40}$/;
const expectedFiles = new Set(["bucephalus", "bucephalus-cloud", "bucephalus-modal-launcher", "install.sh", "README.md", "LICENSE", "release-manifest.json", "SHA256SUMS"]);

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
    const stat = lstatSync(path);
    if (stat.isDirectory()) {
      out.push(...listFiles(path, base));
    } else if (stat.isFile()) {
      out.push(relative(base, path).split("\\").join("/"));
    } else {
      fail(`core release contains non-regular file: ${relative(base, path).split("\\").join("/")}`);
    }
  }
  return out;
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

function verifyArchiveHeaders(archivePath) {
  const data = gunzipSync(readFileSync(archivePath));
  const seen = new Set();
  for (let offset = 0; offset + 512 <= data.length; offset += 512) {
    const header = data.subarray(offset, offset + 512);
    if (blockIsZero(header)) {
      break;
    }
    const name = cString(header, 0, 100);
    const prefix = cString(header, 345, 155);
    const path = prefix ? `${prefix}/${name}` : name;
    const type = cString(header, 156, 1) || "0";
    const uid = octal(header, 108, 8);
    const gid = octal(header, 116, 8);
    const size = octal(header, 124, 12);
    const mtime = octal(header, 136, 12);
    const uname = cString(header, 265, 32);
    const gname = cString(header, 297, 32);
    if (type !== "0") {
      fail(`core release archive contains non-file tar entry: ${path}`);
    }
    if (!expectedFiles.has(path)) {
      fail(`core release archive contains unexpected tar entry: ${path}`);
    }
    if (seen.has(path)) {
      fail(`core release archive contains duplicate tar entry: ${path}`);
    }
    seen.add(path);
    if (uid !== 0 || gid !== 0) {
      fail(`core release archive tar entry ${path} must use uid/gid 0`);
    }
    if (!["", "root"].includes(uname) || !["", "root"].includes(gname)) {
      fail(`core release archive tar entry ${path} must not include local owner names`);
    }
    if (mtime !== 0) {
      fail(`core release archive tar entry ${path} must use normalized mtime 0`);
    }
    offset += Math.ceil(size / 512) * 512;
  }
  for (const expected of expectedFiles) {
    if (!seen.has(expected)) {
      fail(`core release archive is missing tar entry: ${expected}`);
    }
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
  for (const file of listFiles(releaseDir)) {
    const path = join(releaseDir, file);
    const data = readFileSync(path);
    if (!isTextLike(data)) {
      continue;
    }
    const text = data.toString("utf8");
    for (const { label, regex } of localPathPatterns) {
      if (regex.test(text)) {
        fail(`${file} contains ${label}; release text payloads must not embed local absolute paths`);
      }
    }
  }
}

if (releaseArchiveInput) {
  verifyArchiveHeaders(releaseArchiveInput);
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
  if ((lstatSync(join(releaseDir, file)).mode & 0o111) === 0) {
    fail(`${file} must be executable`);
  }
}
verifyChecksumManifest();
verifyNoLocalPathContent();

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

const installerScript = manifest.artifacts?.installer_script;
if (!installerScript || installerScript.path !== "install.sh" || typeof installerScript.sha256 !== "string" || !sha256.test(installerScript.sha256)) {
  fail("artifacts.installer_script must point at install.sh with a sha256 digest");
}
if (sha256File(join(releaseDir, "install.sh")) !== installerScript.sha256) {
  fail("installer script digest does not match release manifest");
}

console.log(`verified core release ${releaseDir}`);
JS

RELEASE_DIR="${RELEASE_DIR}" ROOT_DIR="${ROOT_DIR}" RELEASE_ARCHIVE_INPUT="${RELEASE_ARCHIVE_INPUT}" bun "${VERIFY_JS}"
