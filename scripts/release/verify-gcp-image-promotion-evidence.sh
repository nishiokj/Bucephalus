#!/usr/bin/env bash
set -euo pipefail

MANIFEST=""
PROVENANCE=""
TFVARS=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-gcp-image-promotion-evidence.sh \
  --image-manifest <cloud-image-build-manifest.json> \
  --image-provenance <cloud-image-build.provenance.json> \
  --tfvars <gcp-image-digests.tfvars>

Verifies that the pushed Cloud image manifest, image-build provenance, and GCP
image tfvars all describe the same digest-addressed promotion input set.
USAGE
}

public_image_manifest_input_ref() {
  printf '%s\n' "cloud-image-build-manifest://input"
}

public_image_provenance_input_ref() {
  printf '%s\n' "release-provenance://input"
}

public_tfvars_input_ref() {
  printf '%s\n' "tfvars://gcp-image-digests"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image-manifest)
      MANIFEST="${2:-}"
      shift 2
      ;;
    --image-provenance)
      PROVENANCE="${2:-}"
      shift 2
      ;;
    --tfvars)
      TFVARS="${2:-}"
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

if [[ -z "${MANIFEST}" || -z "${PROVENANCE}" || -z "${TFVARS}" ]]; then
  usage >&2
  exit 2
fi

verify_promotion_input_file() {
  local file="$1"
  local label="$2"
  local ref="$3"
  if [[ -L "${file}" ]]; then
    echo "${label} must not be a symlink" >&2
    echo "${ref}" >&2
    exit 2
  fi
  if [[ ! -f "${file}" ]]; then
    echo "${label} does not exist" >&2
    echo "${ref}" >&2
    exit 2
  fi
}

verify_promotion_input_file "${MANIFEST}" "image manifest" "image_manifest_ref: $(public_image_manifest_input_ref)"
verify_promotion_input_file "${PROVENANCE}" "image provenance" "image_provenance_ref: $(public_image_provenance_input_ref)"
verify_promotion_input_file "${TFVARS}" "promotion tfvars" "tfvars_ref: $(public_tfvars_input_ref)"

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
"${ROOT_DIR}/scripts/release/verify-cloud-image-build-manifest.sh" "${MANIFEST}"
"${ROOT_DIR}/scripts/release/verify-cloud-release-provenance.sh" "${PROVENANCE}"

MANIFEST="${MANIFEST}" PROVENANCE="${PROVENANCE}" TFVARS="${TFVARS}" bun -e '
import { createHash } from "node:crypto";
import { lstatSync } from "node:fs";

const manifestPath = process.env.MANIFEST;
const provenancePath = process.env.PROVENANCE;
const tfvarsPath = process.env.TFVARS;
const digestRef = /^.+@sha256:[a-f0-9]{64}$/;
const digest = /^sha256:[a-f0-9]{64}$/;
const zeroDigestRef = /^.+@sha256:0{64}$/;
const zeroDigest = /^sha256:0{64}$/;
const components = new Set(["api", "pool-controller", "migrations", "worker"]);
const deployVariables = new Map([
  ["api_image_digest", "api"],
  ["pool_controller_image_digest", "pool-controller"],
  ["migration_image_digest", "migrations"],
  ["worker_image_digest", "worker"],
]);
const garComponentRepo = /^([a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*)\/(api|pool-controller|migrations|worker)$/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function requireRegularFile(path, label) {
  let stat;
  try {
    stat = lstatSync(path);
  } catch {
    fail(`${label} does not exist or cannot be inspected`);
  }
  if (stat.isSymbolicLink()) {
    fail(`${label} must not be a symlink`);
  }
  if (!stat.isFile()) {
    fail(`${label} must be a regular file`);
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

const manifest = JSON.parse(await readTextFile(manifestPath, "image manifest"));
const provenance = JSON.parse(await readTextFile(provenancePath, "image provenance"));
const tfvarsText = await readTextFile(tfvarsPath, "promotion tfvars");

function parseTfvars(text) {
  const assignments = new Map();
  for (const [lineNumber, rawLine] of text.split(/\r?\n/).entries()) {
    const line = rawLine.trim();
    if (line === "" || line.startsWith("#")) {
      continue;
    }
    const match = line.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"([^"]+)"$/);
    if (!match) {
      fail(`unsupported promotion tfvars line ${lineNumber + 1}`);
    }
    const [, name, value] = match;
    if (!deployVariables.has(name)) {
      fail(`unexpected promotion tfvars variable: ${name}`);
    }
    if (assignments.has(name)) {
      fail(`duplicate promotion tfvars variable: ${name}`);
    }
    if (!digestRef.test(value) || zeroDigestRef.test(value)) {
      fail(`${name} must be a real image@sha256 digest ref`);
    }
    assignments.set(name, value);
  }
  return assignments;
}

function parseGarComponentRepository(repository, component) {
  if (typeof repository !== "string") {
    fail(`${component}.image_repository must be a GCP Artifact Registry component repository`);
  }
  const match = repository.match(garComponentRepo);
  if (!match || match[2] !== component) {
    fail(`${component}.image_repository must be a GCP Artifact Registry component repository`);
  }
  return {
    family: match[1],
    component: match[2],
  };
}

if (manifest.pushed !== true) {
  fail("promotion evidence requires a pushed image manifest");
}
if (provenance.image_build?.pushed !== true) {
  fail("promotion evidence requires pushed image provenance");
}
if (provenance.image_build.manifest_sha256 !== await hashFile(manifestPath, "image manifest")) {
  fail("image provenance manifest_sha256 does not match image manifest");
}
if (provenance.release?.version !== manifest.release.version) {
  fail("image provenance release.version does not match image manifest");
}
if (provenance.release?.target !== manifest.release.target) {
  fail("image provenance release.target does not match image manifest");
}
if (provenance.release?.git_sha !== manifest.release.git_sha) {
  fail("image provenance release.git_sha does not match image manifest");
}
if (provenance.release?.manifest_sha256 !== manifest.release.manifest_sha256) {
  fail("image provenance release manifest digest does not match image manifest");
}
if (provenance.image_build.base_image !== manifest.base_image) {
  fail("image provenance base_image does not match image manifest");
}
if (provenance.image_build.image_context?.path !== manifest.image_context?.path || provenance.image_build.image_context?.sha256 !== manifest.image_context?.sha256) {
  fail("image provenance image_context does not match image manifest");
}

const manifestImages = new Map(manifest.images.map((image) => [image.component, image]));
const provenanceImages = new Map(provenance.image_build.images.map((image) => [image.component, image]));
for (const component of components) {
  const manifestImage = manifestImages.get(component);
  const provenanceImage = provenanceImages.get(component);
  if (!manifestImage || !provenanceImage) {
    fail(`missing image evidence for ${component}`);
  }
  if (manifestImage.boundary_verified !== true || provenanceImage.boundary_verified !== true) {
    fail(`${component} boundary verification evidence is required`);
  }
  if (manifestImage.digest !== provenanceImage.digest || !digest.test(manifestImage.digest) || zeroDigest.test(manifestImage.digest)) {
    fail(`${component} digest does not match between manifest and provenance`);
  }
  if (manifestImage.immutable_ref !== provenanceImage.immutable_ref || !digestRef.test(manifestImage.immutable_ref) || zeroDigestRef.test(manifestImage.immutable_ref)) {
    fail(`${component} immutable_ref does not match between manifest and provenance`);
  }
  if (manifestImage.image_repository !== provenanceImage.image_repository) {
    fail(`${component} image_repository does not match between manifest and provenance`);
  }
  if (manifestImage.tag_ref !== provenanceImage.tag_ref) {
    fail(`${component} tag_ref does not match between manifest and provenance`);
  }
  if (manifestImage.image_id !== provenanceImage.image_id) {
    fail(`${component} image_id does not match between manifest and provenance`);
  }
  if (manifestImage.metadata_file !== provenanceImage.metadata_file) {
    fail(`${component} metadata_file does not match between manifest and provenance`);
  }
  if (manifestImage.dockerfile?.path !== provenanceImage.dockerfile?.path || manifestImage.dockerfile?.sha256 !== provenanceImage.dockerfile?.sha256) {
    fail(`${component} dockerfile does not match between manifest and provenance`);
  }
  if (manifestImage.boundary_image_ref !== provenanceImage.boundary_image_ref) {
    fail(`${component} boundary_image_ref does not match between manifest and provenance`);
  }
  if (manifestImage.boundary_image_id !== provenanceImage.boundary_image_id) {
    fail(`${component} boundary_image_id does not match between manifest and provenance`);
  }
  if (manifestImage.boundary_metadata_file !== provenanceImage.boundary_metadata_file) {
    fail(`${component} boundary_metadata_file does not match between manifest and provenance`);
  }
  if (JSON.stringify(manifestImage.timings_seconds ?? null) !== JSON.stringify(provenanceImage.timings_seconds ?? null)) {
    fail(`${component} timings_seconds does not match between manifest and provenance`);
  }
}

const tfvars = parseTfvars(tfvarsText);
let deployRepositoryFamily = null;
for (const [name, component] of deployVariables.entries()) {
  const manifestImage = manifestImages.get(component);
  const provenanceImage = provenanceImages.get(component);
  const repository = parseGarComponentRepository(manifestImage?.image_repository, component);
  if (deployRepositoryFamily === null) {
    deployRepositoryFamily = repository.family;
  } else if (repository.family !== deployRepositoryFamily) {
    fail("promotion tfvars image repositories must share one GCP Artifact Registry family");
  }
  if (!tfvars.has(name)) {
    fail(`missing promotion tfvars variable: ${name}`);
  }
  if (tfvars.get(name) !== manifestImage.immutable_ref) {
    fail(`${name} does not match image manifest immutable ref`);
  }
  if (tfvars.get(name) !== provenanceImage.immutable_ref) {
    fail(`${name} does not match image provenance immutable ref`);
  }
  if (!tfvars.get(name).startsWith(`${manifestImage.image_repository}@`)) {
    fail(`${name} must use the ${component} image_repository`);
  }
}
if (tfvars.size !== deployVariables.size) {
  fail("promotion tfvars contains unexpected image inputs");
}

if (provenance.signature?.status !== "unsigned") {
  fail("image build provenance must remain unsigned until signing is configured");
}

console.log("verified GCP image promotion evidence promotion-evidence://gcp-image-promotion");
'

"${ROOT_DIR}/scripts/release/verify-gcp-image-tfvars.sh" \
  --image-manifest "${MANIFEST}" \
  --tfvars "${TFVARS}"
