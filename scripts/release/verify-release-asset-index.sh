#!/usr/bin/env bash
set -euo pipefail

INDEX="${1:-}"
INDEX_REF="release-asset-index://source"

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-release-asset-index.sh <release-assets.json>

Validates a Bucephalus release asset index and, when referenced files are
present relative to the current working directory, rechecks their digests.
USAGE
}

if [[ -z "${INDEX}" || "${INDEX}" == "-h" || "${INDEX}" == "--help" ]]; then
  usage
  exit 2
fi

if [[ -L "${INDEX}" ]]; then
  echo "release asset index must not be a symlink" >&2
  exit 2
fi

if [[ ! -f "${INDEX}" ]]; then
  echo "release asset index does not exist: ${INDEX_REF}" >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

INDEX="${INDEX}" bun -e '
import { createHash } from "node:crypto";
import { lstatSync } from "node:fs";

const indexPath = process.env.INDEX;
const sha256 = /^[a-f0-9]{64}$/;
const gitSha = /^[a-f0-9]{40}$/;
const validSchemas = new Map([
  ["core", "bucephalus_core_release_provenance_v1"],
  ["cloud", "bucephalus_cloud_release_provenance_v1"],
]);

function fail(message) {
  console.error(message);
  process.exit(1);
}

function existingRegularFile(path, label) {
  let stat;
  try {
    stat = lstatSync(path);
  } catch (error) {
    if (error?.code === "ENOENT") {
      return false;
    }
    fail(`${label} does not exist or cannot be inspected`);
  }
  if (stat.isSymbolicLink()) {
    fail(`${label} must not be a symlink`);
  }
  if (!stat.isFile()) {
    fail(`${label} must be a regular file`);
  }
  return true;
}

function requireRegularFile(path, label) {
  if (!existingRegularFile(path, label)) {
    fail(`${label} does not exist`);
  }
}

function checkSha(value, label) {
  if (typeof value !== "string" || !sha256.test(value)) {
    fail(`${label} must be a lowercase sha256 digest`);
  }
}

async function hashFile(path, label) {
  requireRegularFile(path, label);
  return createHash("sha256").update(Buffer.from(await Bun.file(path).arrayBuffer())).digest("hex");
}

async function readTextFile(path, label) {
  requireRegularFile(path, label);
  return Bun.file(path).text();
}

function checkArtifactPath(path, label) {
  if (typeof path !== "string" || path.trim() === "" || path.startsWith("/") || path.includes("..") || path.includes("\\")) {
    fail(`${label} must be a stable artifact-local path`);
  }
  if (looksLikeHostPath(path)) {
    fail(`${label} must not look like a host filesystem path`);
  }
  if (/(^|\/)\.env(\.example)?$/.test(path) || /(^|[-_.])latest([-_.]|$)/.test(path) || /(^|[-_.\/])(secret|token|password|credential|api[_-]?key|private)([-_.\/]|$)/i.test(path)) {
    fail(`${label} contains a forbidden release asset name`);
  }
}

function looksLikeHostPath(path) {
  const normalized = path.replace(/^\.\//, "");
  return /^(Users|home|private|tmp|var\/folders|Volumes|Desktop|Documents|Downloads|runner\/work|github\/workspace)\//.test(normalized);
}

const index = JSON.parse(await readTextFile(indexPath, "release asset index"));

if (index.schema_version !== "bucephalus_release_asset_index_v1") {
  fail("schema_version must be bucephalus_release_asset_index_v1");
}
if (index.predicate_type !== "https://bucephalus.dev/provenance/release-assets/v1") {
  fail("predicate_type is not recognized");
}
if (Number.isNaN(Date.parse(index.generated_at))) {
  fail("generated_at must be an ISO timestamp");
}
if (!Array.isArray(index.asset_roots) || index.asset_roots.length === 0) {
  fail("asset_roots must be a non-empty array");
}
for (const [i, root] of index.asset_roots.entries()) {
  checkArtifactPath(root, `asset_roots[${i}]`);
}
if (!Array.isArray(index.assets) || index.assets.length === 0) {
  fail("assets must be a non-empty array");
}
const requiredTargets = {
  core: index.required_targets?.core ?? [],
  cloud: index.required_targets?.cloud ?? [],
};
if (!Array.isArray(requiredTargets.core) || !Array.isArray(requiredTargets.cloud)) {
  fail("required_targets.core and required_targets.cloud must be arrays when present");
}
if (index.signature?.status !== "unsigned") {
  fail("signature.status must be unsigned until a signing boundary exists");
}
checkSha(index.index_sha256, "index_sha256");
const recomputed = createHash("sha256").update(JSON.stringify({ ...index, index_sha256: null })).digest("hex");
if (index.index_sha256 !== recomputed) {
  fail("index_sha256 does not match index content");
}

const seenPaths = new Set();
const targetsByKind = new Map([
  ["core", new Set()],
  ["cloud", new Set()],
]);
for (const [i, asset] of index.assets.entries()) {
  const label = `assets[${i}]`;
  if (!validSchemas.has(asset.kind)) {
    fail(`${label}.kind must be core or cloud`);
  }
  if (asset.provenance?.schema_version !== validSchemas.get(asset.kind)) {
    fail(`${label}.provenance.schema_version does not match asset kind`);
  }
  if (typeof asset.name !== "string" || !asset.name.endsWith(".tar.gz")) {
    fail(`${label}.name must be a release archive name`);
  }
  if (typeof asset.path !== "string" || !asset.path.endsWith(".tar.gz")) {
    fail(`${label}.path must point at a .tar.gz archive`);
  }
  checkArtifactPath(asset.path, `${label}.path`);
  if (asset.name !== asset.path.split("/").pop()) {
    fail(`${label}.name must match path basename`);
  }
  if (seenPaths.has(asset.path)) {
    fail(`duplicate asset path: ${asset.path}`);
  }
  seenPaths.add(asset.path);
  checkSha(asset.sha256, `${label}.sha256`);

  if (asset.checksum?.path !== `${asset.path}.sha256`) {
    fail(`${label}.checksum.path must be the archive sibling checksum`);
  }
  checkArtifactPath(asset.checksum.path, `${label}.checksum.path`);
  checkSha(asset.checksum.value, `${label}.checksum.value`);
  checkSha(asset.checksum.sha256, `${label}.checksum.sha256`);

  const expectedProvenancePath = asset.path.slice(0, -".tar.gz".length) + ".provenance.json";
  if (asset.provenance.path !== expectedProvenancePath) {
    fail(`${label}.provenance.path must be the archive sibling provenance`);
  }
  checkArtifactPath(asset.provenance.path, `${label}.provenance.path`);
  checkSha(asset.provenance.sha256, `${label}.provenance.sha256`);
  if (typeof asset.provenance.version !== "string" || asset.provenance.version.trim() === "") {
    fail(`${label}.provenance.version is required`);
  }
  if (typeof asset.provenance.target !== "string" || asset.provenance.target.trim() === "") {
    fail(`${label}.provenance.target is required`);
  }
  if (typeof asset.provenance.git_sha !== "string" || !gitSha.test(asset.provenance.git_sha)) {
    fail(`${label}.provenance.git_sha must be a lowercase git object id`);
  }
  if (typeof asset.provenance.git_dirty !== "boolean") {
    fail(`${label}.provenance.git_dirty must be boolean`);
  }
  if (asset.provenance.signature_status !== "unsigned") {
    fail(`${label}.provenance.signature_status must be unsigned`);
  }
  const targets = targetsByKind.get(asset.kind);
  if (targets.has(asset.provenance.target)) {
    fail(`duplicate ${asset.kind} release asset target: ${asset.provenance.target}`);
  }
  targets.add(asset.provenance.target);

  if (existingRegularFile(asset.path, `${label}.path file`)) {
    if (await hashFile(asset.path, `${label}.path file`) !== asset.sha256) {
      fail(`${label}.path file digest does not match index`);
    }
  }
  if (existingRegularFile(asset.checksum.path, `${label}.checksum.path file`)) {
    const checksumRaw = await readTextFile(asset.checksum.path, `${label}.checksum.path file`);
    if (!checksumRaw.endsWith("\n") || checksumRaw.slice(0, -1).includes("\n")) {
      fail(`${label}.checksum.path file must contain exactly one checksum record`);
    }
    const checksumText = checksumRaw.slice(0, -1);
    if (checksumText !== `${asset.checksum.value}  ${asset.name}`) {
      fail(`${label}.checksum.path file must contain exactly the indexed checksum record`);
    }
    if (await hashFile(asset.checksum.path, `${label}.checksum.path file`) !== asset.checksum.sha256) {
      fail(`${label}.checksum.path file digest does not match index`);
    }
  }
  if (existingRegularFile(asset.provenance.path, `${label}.provenance.path file`)) {
    if (await hashFile(asset.provenance.path, `${label}.provenance.path file`) !== asset.provenance.sha256) {
      fail(`${label}.provenance.path file digest does not match index`);
    }
  }
}

for (const kind of ["core", "cloud"]) {
  const required = requiredTargets[kind];
  const requiredSet = new Set(required);
  if (requiredSet.size !== required.length) {
    fail(`duplicate required ${kind} release target`);
  }
  if (required.length === 0) {
    continue;
  }
  const seen = targetsByKind.get(kind);
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

console.log("verified release asset index release-asset-index://source");
'
