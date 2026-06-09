#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT=""
ASSET_DIRS=()
REQUIRED_CORE_TARGETS=()
REQUIRED_CLOUD_TARGETS=()

usage() {
  cat <<'USAGE'
Usage: scripts/release/write-release-asset-index.sh \
  --assets-dir <dir> [--assets-dir <dir> ...] \
  [--required-core-target <target> ...] \
  [--required-cloud-target <target> ...] \
  --out <release-assets.json>

Writes a verified index for GitHub Release archive assets. Every discovered
.tar.gz asset must have a sibling .tar.gz.sha256 and .provenance.json file.
Each asset is verified through the Core or Cloud release verifier before the
index is written. When required target arguments are provided, the indexed
release must contain exactly that target matrix for each asset kind.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --assets-dir)
      ASSET_DIRS+=("${2:-}")
      shift 2
      ;;
    --required-core-target)
      REQUIRED_CORE_TARGETS+=("${2:-}")
      shift 2
      ;;
    --required-cloud-target)
      REQUIRED_CLOUD_TARGETS+=("${2:-}")
      shift 2
      ;;
    --out)
      OUT="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ${#ASSET_DIRS[@]} -eq 0 || -z "${OUT}" ]]; then
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

require_command bun

for dir in "${ASSET_DIRS[@]}"; do
  if [[ -L "${dir}" ]]; then
    echo "asset directory must not be a symlink" >&2
    exit 2
  fi
  if [[ -z "${dir}" || ! -d "${dir}" ]]; then
    echo "asset directory does not exist" >&2
    echo "asset_root_ref: release-assets://asset-root" >&2
    exit 2
  fi
done

for dir in "${ASSET_DIRS[@]}"; do
  if find "${dir}" -type l -print -quit | grep -q .; then
    echo "release asset directories must not contain symlinks" >&2
    exit 1
  fi
done

ARCHIVES=()
while IFS= read -r -d '' archive; do
  ARCHIVES+=("${archive}")
done < <(find "${ASSET_DIRS[@]}" -type f -name '*.tar.gz' -print0 | sort -z)
if [[ ${#ARCHIVES[@]} -eq 0 ]]; then
  echo "no release archives found under asset directories" >&2
  exit 1
fi

for dir in "${ASSET_DIRS[@]}"; do
  while IFS= read -r -d '' file; do
    case "${file}" in
      *.tar.gz|*.tar.gz.sha256|*.provenance.json)
        ;;
      *)
        echo "unexpected release asset file" >&2
        echo "asset_file_ref: release-assets://unexpected-file" >&2
        exit 1
        ;;
    esac
    if [[ "${file}" =~ (^|/)\.env(\.example)?$ ]] || [[ "${file}" =~ (^|[-_.])latest([-_.]|$) ]] || [[ "${file}" =~ (^|[-_./])(secret|token|password|credential|api[_-]?key|private)([-_./]|$) ]]; then
      echo "forbidden release asset name" >&2
      echo "asset_file_ref: release-assets://forbidden-name" >&2
      exit 1
    fi
  done < <(find "${dir}" -type f -print0)
done

for archive in "${ARCHIVES[@]}"; do
  checksum="${archive}.sha256"
  provenance="${archive%.tar.gz}.provenance.json"
  if [[ -L "${archive}" ]]; then
    echo "release archive must not be a symlink: $(basename "${archive}")" >&2
    exit 1
  fi
  if [[ ! -f "${checksum}" ]]; then
    echo "release archive is missing checksum: $(basename "${checksum}")" >&2
    exit 1
  fi
  if [[ -L "${checksum}" ]]; then
    echo "release archive checksum must not be a symlink: $(basename "${checksum}")" >&2
    exit 1
  fi
  if [[ ! -f "${provenance}" ]]; then
    echo "release archive is missing provenance: $(basename "${provenance}")" >&2
    exit 1
  fi
  if [[ -L "${provenance}" ]]; then
    echo "release archive provenance must not be a symlink: $(basename "${provenance}")" >&2
    exit 1
  fi

  schema="$(
    PROVENANCE="${provenance}" bun -e '
      const record = JSON.parse(await Bun.file(process.env.PROVENANCE).text());
      console.log(record.schema_version ?? "");
    '
  )"
  case "${schema}" in
    bucephalus_core_release_provenance_v1)
      "${ROOT_DIR}/scripts/release/verify-core-release.sh" "${archive}"
      "${ROOT_DIR}/scripts/release/verify-core-release-provenance.sh" "${provenance}" --release "${archive}"
      ;;
    bucephalus_cloud_release_provenance_v1)
      "${ROOT_DIR}/scripts/release/verify-buc-release.sh" "${archive}"
      "${ROOT_DIR}/scripts/release/verify-cloud-release-provenance.sh" "${provenance}" --release "${archive}"
      ;;
    *)
      echo "unknown provenance schema for release provenance" >&2
      exit 1
      ;;
  esac
done

OUT_DIR="$(dirname "${OUT}")"
mkdir -p "${OUT_DIR}"

ASSET_DIRS_JSON="$(
  ASSET_DIRS_JOINED="$(printf '%s\n' "${ASSET_DIRS[@]}")" bun -e '
    console.log(JSON.stringify(process.env.ASSET_DIRS_JOINED.split("\n").filter(Boolean)));
  '
)"

REQUIRED_CORE_TARGETS_JSON="$(
  REQUIRED_CORE_TARGETS_JOINED="$(printf '%s\n' "${REQUIRED_CORE_TARGETS[@]}")" bun -e '
    console.log(JSON.stringify(process.env.REQUIRED_CORE_TARGETS_JOINED.split("\n").filter(Boolean)));
  '
)"

REQUIRED_CLOUD_TARGETS_JSON="$(
  REQUIRED_CLOUD_TARGETS_JOINED="$(printf '%s\n' "${REQUIRED_CLOUD_TARGETS[@]}")" bun -e '
    console.log(JSON.stringify(process.env.REQUIRED_CLOUD_TARGETS_JOINED.split("\n").filter(Boolean)));
  '
)"

ASSET_DIRS_JSON="${ASSET_DIRS_JSON}" \
REQUIRED_CORE_TARGETS_JSON="${REQUIRED_CORE_TARGETS_JSON}" \
REQUIRED_CLOUD_TARGETS_JSON="${REQUIRED_CLOUD_TARGETS_JSON}" \
ROOT_DIR="${ROOT_DIR}" \
OUT="${OUT}" bun -e '
import { createHash } from "node:crypto";
import { lstatSync, readdirSync } from "node:fs";
import { basename, relative, resolve } from "node:path";

const assetDirs = JSON.parse(process.env.ASSET_DIRS_JSON);
const requiredTargets = {
  core: JSON.parse(process.env.REQUIRED_CORE_TARGETS_JSON),
  cloud: JSON.parse(process.env.REQUIRED_CLOUD_TARGETS_JSON),
};
const out = process.env.OUT;
const rootDir = resolve(process.env.ROOT_DIR);
const sha256 = /^[a-f0-9]{64}$/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

async function hashFile(path, label) {
  requireRegularFile(path, label);
  return createHash("sha256").update(Buffer.from(await Bun.file(path).arrayBuffer())).digest("hex");
}

async function readTextFile(path, label) {
  requireRegularFile(path, label);
  return Bun.file(path).text();
}

function normalize(path) {
  return relative(rootDir, resolve(path)).split("\\").join("/");
}

function artifactPath(path, label) {
  const normalized = normalize(path);
  if (normalized === "" || normalized.startsWith("/") || normalized.includes("..") || normalized.includes("\\")) {
    fail(`${label} must be a stable artifact-local path`);
  }
  if (looksLikeHostPath(normalized)) {
    fail(`${label} must not look like a host filesystem path`);
  }
  if (/(^|\/)\.env(\.example)?$/.test(normalized) || /(^|[-_.])latest([-_.]|$)/.test(normalized) || /(^|[-_.\/])(secret|token|password|credential|api[_-]?key|private)([-_.\/]|$)/i.test(normalized)) {
    fail(`${label} contains a forbidden release asset name`);
  }
  return normalized;
}

function looksLikeHostPath(path) {
  const normalized = path.replace(/^\.\//, "");
  return /^(Users|home|private|tmp|var\/folders|Volumes|Desktop|Documents|Downloads|runner\/work|github\/workspace)\//.test(normalized);
}

function inspectNoSymlink(path, label) {
  let stat;
  try {
    stat = lstatSync(path);
  } catch {
    fail(`${label} does not exist or cannot be inspected`);
  }
  if (stat.isSymbolicLink()) {
    fail(`${label} must not be a symlink`);
  }
  return stat;
}

function requireRegularFile(path, label) {
  const stat = inspectNoSymlink(path, label);
  if (!stat.isFile()) {
    fail(`${label} must be a regular file`);
  }
}

function requireAssetDirectory(path, label) {
  const stat = inspectNoSymlink(path, label);
  if (!stat.isDirectory()) {
    fail(`${label} must be a directory`);
  }
}

function walk(dir) {
  const files = [];
  function visit(path) {
    for (const name of readdirSync(path)) {
      const child = `${path}/${name}`;
      const childLabel = artifactPath(child, `${name} path`);
      const stat = inspectNoSymlink(child, childLabel);
      if (stat.isDirectory()) {
        visit(child);
      } else if (stat.isFile()) {
        files.push(child);
      }
    }
  }
  visit(dir);
  return files.sort();
}

for (const dir of assetDirs) {
  artifactPath(dir, `${basename(dir)} asset root`);
  requireAssetDirectory(dir, `${basename(dir)} asset root`);
}

const archives = assetDirs.flatMap((dir) => walk(dir).filter((file) => file.endsWith(".tar.gz"))).sort();
if (archives.length === 0) {
  fail("release asset index requires at least one archive");
}

const assets = [];
for (const archive of archives) {
  const checksumPath = `${archive}.sha256`;
  const provenancePath = `${archive.slice(0, -".tar.gz".length)}.provenance.json`;
  const checksumRaw = await readTextFile(checksumPath, `${basename(checksumPath)} checksum`);
  if (!checksumRaw.endsWith("\n") || checksumRaw.slice(0, -1).includes("\n")) {
    fail(`${basename(checksumPath)} checksum must contain exactly one checksum record`);
  }
  const checksumText = checksumRaw.slice(0, -1);
  const checksumValue = checksumText.slice(0, 64);
  if (!sha256.test(checksumValue)) {
    fail(`${basename(checksumPath)} checksum does not start with a lowercase sha256 digest`);
  }
  if (checksumText !== `${checksumValue}  ${basename(archive)}`) {
    fail(`${basename(checksumPath)} checksum must contain exactly one checksum record for ${basename(archive)}`);
  }
  const archiveSha = await hashFile(archive, `${basename(archive)} archive`);
  if (archiveSha !== checksumValue) {
    fail(`${basename(archive)} archive sha256 does not match sibling checksum`);
  }
  const provenance = JSON.parse(await readTextFile(provenancePath, `${basename(provenancePath)} provenance`));
  let kind;
  if (provenance.schema_version === "bucephalus_core_release_provenance_v1") {
    kind = "core";
  } else if (provenance.schema_version === "bucephalus_cloud_release_provenance_v1") {
    kind = "cloud";
  } else {
    fail(`${basename(provenancePath)} provenance has unknown schema_version`);
  }
  if (provenance.release?.archive_sha256 !== archiveSha) {
    fail(`${basename(provenancePath)} provenance release.archive_sha256 does not match archive`);
  }
  assets.push({
    kind,
    name: basename(archive),
    path: artifactPath(archive, `${basename(archive)} path`),
    sha256: archiveSha,
    checksum: {
      path: artifactPath(checksumPath, `${basename(checksumPath)} path`),
      value: checksumValue,
      sha256: await hashFile(checksumPath, `${basename(checksumPath)} checksum`),
    },
    provenance: {
      path: artifactPath(provenancePath, `${basename(provenancePath)} path`),
      sha256: await hashFile(provenancePath, `${basename(provenancePath)} provenance`),
      schema_version: provenance.schema_version,
      predicate_type: provenance.predicate_type,
      version: provenance.release.version,
      target: provenance.release.target,
      git_sha: provenance.release.git_sha,
      git_dirty: provenance.release.git_dirty,
      signature_status: provenance.signature?.status,
    },
  });
}

assets.sort((left, right) => left.path.localeCompare(right.path));

function validateTargetMatrix(kind) {
  const seen = new Set();
  for (const asset of assets.filter((entry) => entry.kind === kind)) {
    const target = asset.provenance.target;
    if (seen.has(target)) {
      fail(`duplicate ${kind} release asset target: ${target}`);
    }
    seen.add(target);
  }
  const required = requiredTargets[kind];
  if (required.length === 0) {
    return;
  }
  const requiredSet = new Set(required);
  if (requiredSet.size !== required.length) {
    fail(`duplicate required ${kind} release target`);
  }
  for (const target of requiredSet) {
    if (!seen.has(target)) {
      fail(`missing required ${kind} release asset target: ${target}`);
    }
  }
  for (const target of seen) {
    if (!requiredSet.has(target)) {
      fail(`unexpected ${kind} release asset target: ${target}`);
    }
  }
}

validateTargetMatrix("core");
validateTargetMatrix("cloud");

const record = {
  schema_version: "bucephalus_release_asset_index_v1",
  predicate_type: "https://bucephalus.dev/provenance/release-assets/v1",
  generated_at: new Date().toISOString(),
  asset_roots: assetDirs.map((dir) => artifactPath(dir, `${basename(dir)} asset root`)).sort(),
  required_targets: {
    core: requiredTargets.core.slice().sort(),
    cloud: requiredTargets.cloud.slice().sort(),
  },
  assets,
  signature: {
    status: "unsigned",
  },
  index_sha256: null,
};
record.index_sha256 = createHash("sha256").update(JSON.stringify(record)).digest("hex");
await Bun.write(out, `${JSON.stringify(record, null, 2)}\n`);
console.log("wrote release asset index release-asset-index://output");
'

"${ROOT_DIR}/scripts/release/verify-release-asset-index.sh" "${OUT}"
