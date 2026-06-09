#!/usr/bin/env bash
set -euo pipefail

MANIFEST=""
TFVARS=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-gcp-image-tfvars.sh --image-manifest <cloud-image-build-manifest.json> --tfvars <gcp-image-digests.tfvars>

Verifies that a GCP image tfvars fragment contains exactly the digest-addressed
control-plane image inputs derived from a pushed Cloud image build manifest.
USAGE
}

public_image_manifest_input_ref() {
  printf '%s\n' "cloud-image-build-manifest://input"
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

if [[ -z "${MANIFEST}" || -z "${TFVARS}" ]]; then
  usage >&2
  exit 2
fi

if [[ -L "${MANIFEST}" ]]; then
  echo "image manifest must not be a symlink" >&2
  echo "image_manifest_ref: $(public_image_manifest_input_ref)" >&2
  exit 2
fi

if [[ ! -f "${MANIFEST}" ]]; then
  echo "image manifest does not exist" >&2
  echo "image_manifest_ref: $(public_image_manifest_input_ref)" >&2
  exit 2
fi

if [[ -L "${TFVARS}" ]]; then
  echo "tfvars file must not be a symlink" >&2
  echo "tfvars_ref: $(public_tfvars_input_ref)" >&2
  exit 2
fi

if [[ ! -f "${TFVARS}" ]]; then
  echo "tfvars file does not exist" >&2
  echo "tfvars_ref: $(public_tfvars_input_ref)" >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
"${ROOT_DIR}/scripts/release/verify-cloud-image-build-manifest.sh" "${MANIFEST}"

MANIFEST="${MANIFEST}" TFVARS="${TFVARS}" bun -e '
import { lstatSync } from "node:fs";

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

const manifest = JSON.parse(await readTextFile(process.env.MANIFEST, "image manifest"));
const text = await readTextFile(process.env.TFVARS, "tfvars file");
const digestRef = /^.+@sha256:[a-f0-9]{64}$/;
const zeroDigestRef = /^.+@sha256:0{64}$/;
const garComponentRepo = /^([a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*)\/(api|pool-controller|migrations|worker)$/;

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
  fail("image manifest must be pushed=true to verify deploy tfvars");
}

const byComponent = new Map(manifest.images.map((image) => [image.component, image]));
const expectedComponents = new Map([
  ["api_image_digest", "api"],
  ["pool_controller_image_digest", "pool-controller"],
  ["migration_image_digest", "migrations"],
  ["worker_image_digest", "worker"],
]);
const expected = new Map();
let deployRepositoryFamily = null;

for (const [name, component] of expectedComponents.entries()) {
  const image = byComponent.get(component);
  const repository = parseGarComponentRepository(image?.image_repository, component);
  if (deployRepositoryFamily === null) {
    deployRepositoryFamily = repository.family;
  } else if (repository.family !== deployRepositoryFamily) {
    fail("deploy tfvars image repositories must share one GCP Artifact Registry family");
  }
  if (typeof image.immutable_ref !== "string" || !image.immutable_ref.startsWith(`${image.image_repository}@`)) {
    fail(`image manifest is missing immutable ref for ${name}`);
  }
  expected.set(name, image.immutable_ref);
}

const assignments = new Map();
for (const [lineNumber, rawLine] of text.split(/\r?\n/).entries()) {
  const line = rawLine.trim();
  if (line === "" || line.startsWith("#")) {
    continue;
  }
  const match = line.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"([^"]+)"$/);
  if (!match) {
    fail(`unsupported tfvars line ${lineNumber + 1}`);
  }
  const [, name, value] = match;
  if (!expected.has(name)) {
    fail(`unexpected tfvars variable: ${name}`);
  }
  if (assignments.has(name)) {
    fail(`duplicate tfvars variable: ${name}`);
  }
  if (!digestRef.test(value) || zeroDigestRef.test(value)) {
    fail(`${name} must be a real image@sha256 digest ref`);
  }
  assignments.set(name, value);
}

for (const [name, value] of expected.entries()) {
  const component = expectedComponents.get(name);
  const image = byComponent.get(component);
  if (typeof value !== "string" || !digestRef.test(value) || zeroDigestRef.test(value)) {
    fail(`image manifest is missing a real immutable ref for ${name}`);
  }
  if (!assignments.has(name)) {
    fail(`missing tfvars variable: ${name}`);
  }
  if (assignments.get(name) !== value) {
    fail(`${name} does not match image manifest immutable ref`);
  }
  if (!assignments.get(name).startsWith(`${image.image_repository}@`)) {
    fail(`${name} must use the ${component} image_repository`);
  }
}

if (assignments.size !== expected.size) {
  fail("tfvars contains unexpected image inputs");
}

console.log("verified GCP image tfvars tfvars://gcp-image-digests");
'
