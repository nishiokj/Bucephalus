#!/usr/bin/env bash
set -euo pipefail

INDEX="${1:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-image-promotion-evidence-index.sh <cloud-image-promotion-evidence.json>

Validates an unsigned pushed-image promotion evidence index and, when
referenced files are present relative to the current working directory, rechecks
their digests and the complete promotion evidence handoff.
USAGE
}

if [[ -z "${INDEX}" || "${INDEX}" == "-h" || "${INDEX}" == "--help" ]]; then
  usage
  exit 2
fi

if [[ -L "${INDEX}" ]]; then
  echo "cloud image promotion evidence index must not be a symlink" >&2
  exit 2
fi

if [[ ! -f "${INDEX}" ]]; then
  echo "cloud image promotion evidence index does not exist" >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

INDEX="${INDEX}" bun -e '
import { createHash } from "node:crypto";
import { lstatSync } from "node:fs";
import { dirname, join } from "node:path";

const indexPath = process.env.INDEX;
const indexDir = dirname(indexPath);
const sha256 = /^[a-f0-9]{64}$/;
const gitSha = /^[a-f0-9]{40}$/;
const digest = /^sha256:[a-f0-9]{64}$/;
const digestRef = /^.+@sha256:[a-f0-9]{64}$/;
const deployComponents = ["api", "pool-controller", "migrations", "worker"];
const evidenceKeys = ["image_manifest", "image_provenance", "tfvars"];
const garDeployComponentRepo = /^([a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*)\/(api|pool-controller|migrations|worker)$/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function checkSha(value, label) {
  if (typeof value !== "string" || !sha256.test(value)) {
    fail(`${label} must be a lowercase sha256 digest`);
  }
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

async function readTextFile(path, label) {
  requireRegularFile(path, label);
  return Bun.file(path).text();
}

async function hashFile(path, label) {
  requireRegularFile(path, label);
  return createHash("sha256").update(Buffer.from(await Bun.file(path).arrayBuffer())).digest("hex");
}

function resolveEvidencePath(entry, label) {
  if (existingRegularFile(entry.path, `${label}.path file`)) {
    return entry.path;
  }
  const sibling = join(indexDir, entry.name);
  return existingRegularFile(sibling, `${label}.sibling file`) ? sibling : null;
}

function looksLikeHostPath(path) {
  const normalized = path.replace(/^\.\//, "");
  return /^(Users|home|private|tmp|var\/folders|Volumes|Desktop|Documents|Downloads|runner\/work|github\/workspace)\//.test(normalized);
}

function checkArtifactPath(path, label) {
  if (typeof path !== "string" || path.trim() === "" || path.includes("..") || path.startsWith("/") || path.includes("\\")) {
    fail(`${label} must be a stable artifact-local path`);
  }
  if (looksLikeHostPath(path)) {
    fail(`${label} must not look like a host filesystem path`);
  }
  if (/(^|\/)\.env(\.example)?$/.test(path) || /(^|[-_.])latest([-_.]|$)/.test(path)) {
    fail(`${label} contains a forbidden promotion evidence name`);
  }
}

const index = JSON.parse(await readTextFile(indexPath, "cloud image promotion evidence index"));

if (index.schema_version !== "bucephalus_cloud_image_promotion_evidence_index_v1") {
  fail("schema_version must be bucephalus_cloud_image_promotion_evidence_index_v1");
}
if (index.predicate_type !== "https://bucephalus.dev/provenance/cloud-image-promotion-evidence/v1") {
  fail("predicate_type is not recognized");
}
if (Number.isNaN(Date.parse(index.generated_at))) {
  fail("generated_at must be an ISO timestamp");
}
if (index.signature?.status !== "unsigned") {
  fail("signature.status must be unsigned until a signing boundary exists");
}
checkSha(index.index_sha256, "index_sha256");
const recomputed = createHash("sha256").update(JSON.stringify({ ...index, index_sha256: null })).digest("hex");
if (index.index_sha256 !== recomputed) {
  fail("index_sha256 does not match index content");
}

if (typeof index.release?.version !== "string" || index.release.version.trim() === "") {
  fail("release.version is required");
}
if (typeof index.release?.target !== "string" || index.release.target.trim() === "") {
  fail("release.target is required");
}
if (typeof index.release?.git_sha !== "string" || !gitSha.test(index.release.git_sha)) {
  fail("release.git_sha must be a lowercase git object id");
}
checkSha(index.release?.manifest_sha256, "release.manifest_sha256");

const evidence = index.evidence ?? {};
const actualEvidenceKeys = Object.keys(evidence).sort();
if (actualEvidenceKeys.length !== evidenceKeys.length || actualEvidenceKeys.join(",") !== evidenceKeys.slice().sort().join(",")) {
  fail("evidence must contain exactly image_manifest, image_provenance, and tfvars");
}
if (evidence.image_manifest?.name !== "cloud-image-build-manifest.json") {
  fail("evidence.image_manifest.name must be cloud-image-build-manifest.json");
}
if (evidence.image_manifest?.schema_version !== "bucephalus_cloud_image_build_manifest_v1") {
  fail("evidence.image_manifest.schema_version must be bucephalus_cloud_image_build_manifest_v1");
}
if (evidence.image_provenance?.name !== "cloud-image-build.provenance.json") {
  fail("evidence.image_provenance.name must be cloud-image-build.provenance.json");
}
if (evidence.image_provenance?.schema_version !== "bucephalus_cloud_release_provenance_v1") {
  fail("evidence.image_provenance.schema_version must be bucephalus_cloud_release_provenance_v1");
}
if (evidence.image_provenance?.signature_status !== "unsigned") {
  fail("evidence.image_provenance.signature_status must be unsigned");
}
if (evidence.tfvars?.name !== "gcp-image-digests.tfvars") {
  fail("evidence.tfvars.name must be gcp-image-digests.tfvars");
}

for (const [name, entry] of Object.entries(evidence)) {
  checkArtifactPath(entry.path, `evidence.${name}.path`);
  if (entry.path.split("/").pop() !== entry.name) {
    fail(`evidence.${name}.name must match path basename`);
  }
  checkSha(entry.sha256, `evidence.${name}.sha256`);
  const resolvedPath = resolveEvidencePath(entry, `evidence.${name}`);
  if (resolvedPath !== null && await hashFile(resolvedPath, `evidence.${name} file`) !== entry.sha256) {
    fail(`evidence.${name}.sha256 does not match referenced file`);
  }
}

if (!Array.isArray(index.deploy_images) || index.deploy_images.length !== deployComponents.length) {
  fail("deploy_images must contain exactly api, pool-controller, migrations, and worker");
}
if (typeof index.repository_family !== "string" || !/^[a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*$/.test(index.repository_family)) {
  fail("repository_family must be a GCP Artifact Registry repository family");
}
const seen = new Set();
for (const [i, image] of index.deploy_images.entries()) {
  const label = `deploy_images[${i}]`;
  if (!deployComponents.includes(image.component)) {
    fail(`${label}.component must be api, pool-controller, migrations, or worker`);
  }
  if (seen.has(image.component)) {
    fail(`duplicate deploy image component: ${image.component}`);
  }
  seen.add(image.component);
  const repositoryMatch = typeof image.image_repository === "string" ? image.image_repository.match(garDeployComponentRepo) : null;
  if (!repositoryMatch || repositoryMatch[1] !== index.repository_family || repositoryMatch[2] !== image.component) {
    fail(`${label}.image_repository must use repository_family and end with the component repository`);
  }
  if (typeof image.immutable_ref !== "string" || !digestRef.test(image.immutable_ref) || !image.immutable_ref.startsWith(`${image.image_repository}@`)) {
    fail(`${label}.immutable_ref must use image_repository`);
  }
  if (typeof image.digest !== "string" || !digest.test(image.digest) || !image.immutable_ref.endsWith(image.digest)) {
    fail(`${label}.digest must match immutable_ref`);
  }
}
for (const component of deployComponents) {
  if (!seen.has(component)) {
    fail(`missing deploy image component: ${component}`);
  }
}

const resolvedImageManifest = resolveEvidencePath(evidence.image_manifest, "evidence.image_manifest");
const resolvedImageProvenance = resolveEvidencePath(evidence.image_provenance, "evidence.image_provenance");
const resolvedTfvars = resolveEvidencePath(evidence.tfvars, "evidence.tfvars");
if (resolvedImageManifest && resolvedImageProvenance && resolvedTfvars) {
  const manifest = JSON.parse(await readTextFile(resolvedImageManifest, "evidence.image_manifest file"));
  const provenance = JSON.parse(await readTextFile(resolvedImageProvenance, "evidence.image_provenance file"));
  if (manifest.release?.version !== index.release.version || provenance.release?.version !== index.release.version) {
    fail("promotion evidence release.version does not match index");
  }
  if (manifest.release?.target !== index.release.target || provenance.release?.target !== index.release.target) {
    fail("promotion evidence release.target does not match index");
  }
  if (manifest.release?.git_sha !== index.release.git_sha || provenance.release?.git_sha !== index.release.git_sha) {
    fail("promotion evidence release.git_sha does not match index");
  }
  const manifestImages = new Map(manifest.images.map((image) => [image.component, image]));
  for (const image of index.deploy_images) {
    const manifestImage = manifestImages.get(image.component);
    if (manifestImage?.immutable_ref !== image.immutable_ref || manifestImage?.digest !== image.digest || manifestImage?.image_repository !== image.image_repository) {
      fail(`${image.component} deploy image does not match indexed manifest`);
    }
    if (!manifestImage.image_repository.startsWith(`${index.repository_family}/`)) {
      fail(`${image.component} deploy image does not match repository_family`);
    }
  }
}

console.log("verified cloud image promotion evidence index promotion-evidence://cloud-image-promotion-evidence");
'

resolve_evidence_path() {
  local key="$1"
  INDEX="${INDEX}" KEY="${key}" bun -e '
import { lstatSync } from "node:fs";
import { dirname, join } from "node:path";

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

async function readTextFile(path, label) {
  requireRegularFile(path, label);
  return Bun.file(path).text();
}

const index = JSON.parse(await readTextFile(process.env.INDEX, "cloud image promotion evidence index"));
const key = process.env.KEY;
const entry = index.evidence?.[key];
if (!entry) {
  process.exit(0);
}
if (existingRegularFile(entry.path, `evidence.${key}.path file`)) {
  console.log(entry.path);
  process.exit(0);
}
const sibling = join(dirname(process.env.INDEX), entry.name);
if (existingRegularFile(sibling, `evidence.${key}.sibling file`)) {
  console.log(sibling);
}
'
}

resolved_image_manifest="$(resolve_evidence_path image_manifest)"
resolved_image_provenance="$(resolve_evidence_path image_provenance)"
resolved_tfvars="$(resolve_evidence_path tfvars)"
if [[ -n "${resolved_image_manifest}" && -n "${resolved_image_provenance}" && -n "${resolved_tfvars}" ]]; then
  "${BASH_SOURCE[0]%/*}/verify-gcp-image-promotion-evidence.sh" \
    --image-manifest "${resolved_image_manifest}" \
    --image-provenance "${resolved_image_provenance}" \
    --tfvars "${resolved_tfvars}"
fi
