#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST=""
PROVENANCE=""
TFVARS=""
OUT=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/write-cloud-image-promotion-evidence-index.sh \
  --image-manifest <cloud-image-build-manifest.json> \
  --image-provenance <cloud-image-build.provenance.json> \
  --tfvars <gcp-image-digests.tfvars> \
  --out <cloud-image-promotion-evidence.json>

Writes an unsigned index for the pushed-image promotion evidence handoff.
The index is not a signature. It records stable digests for the image build
manifest, image-build provenance, and generated GCP image tfvars after the
complete promotion evidence verifier has accepted them.
USAGE
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
    --out)
      OUT="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${MANIFEST}" || -z "${PROVENANCE}" || -z "${TFVARS}" || -z "${OUT}" ]]; then
  usage >&2
  exit 2
fi

for file in "${MANIFEST}" "${PROVENANCE}" "${TFVARS}"; do
  if [[ ! -f "${file}" ]]; then
    echo "promotion evidence file does not exist: ${file}" >&2
    exit 2
  fi
done

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

"${ROOT_DIR}/scripts/release/verify-gcp-image-promotion-evidence.sh" \
  --image-manifest "${MANIFEST}" \
  --image-provenance "${PROVENANCE}" \
  --tfvars "${TFVARS}"

mkdir -p "$(dirname "${OUT}")"

MANIFEST="${MANIFEST}" PROVENANCE="${PROVENANCE}" TFVARS="${TFVARS}" OUT="${OUT}" bun -e '
import { createHash } from "node:crypto";
import { basename, relative } from "node:path";

const manifestPath = process.env.MANIFEST;
const provenancePath = process.env.PROVENANCE;
const tfvarsPath = process.env.TFVARS;
const out = process.env.OUT;
const manifest = JSON.parse(await Bun.file(manifestPath).text());
const provenance = JSON.parse(await Bun.file(provenancePath).text());
const garComponentRepo = /^([a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*)\/(api|pool-controller|migrations)$/;

function sha256Text(value) {
  return createHash("sha256").update(value).digest("hex");
}

async function hashFile(path) {
  return createHash("sha256").update(Buffer.from(await Bun.file(path).arrayBuffer())).digest("hex");
}

function normalize(path) {
  return relative(process.cwd(), path).split("\\").join("/");
}

const deployComponents = ["api", "pool-controller", "migrations"];
const images = new Map(manifest.images.map((image) => [image.component, image]));
let repositoryFamily = null;
const deployImages = deployComponents.map((component) => {
  const image = images.get(component);
  const match = image.image_repository.match(garComponentRepo);
  if (!match || match[2] !== component) {
    console.error(`${component}.image_repository must be a GCP Artifact Registry deploy component repository`);
    process.exit(1);
  }
  if (repositoryFamily === null) {
    repositoryFamily = match[1];
  } else if (repositoryFamily !== match[1]) {
    console.error("image promotion evidence index deploy images must share one GCP Artifact Registry family");
    process.exit(1);
  }
  return {
    component,
    image_repository: image.image_repository,
    immutable_ref: image.immutable_ref,
    digest: image.digest,
  };
});

const record = {
  schema_version: "bucephalus_cloud_image_promotion_evidence_index_v1",
  predicate_type: "https://bucephalus.dev/provenance/cloud-image-promotion-evidence/v1",
  generated_at: new Date().toISOString(),
  release: {
    version: manifest.release.version,
    target: manifest.release.target,
    git_sha: manifest.release.git_sha,
    manifest_sha256: manifest.release.manifest_sha256,
  },
  evidence: {
    image_manifest: {
      name: basename(manifestPath),
      path: normalize(manifestPath),
      sha256: await hashFile(manifestPath),
      schema_version: manifest.schema_version,
    },
    image_provenance: {
      name: basename(provenancePath),
      path: normalize(provenancePath),
      sha256: await hashFile(provenancePath),
      schema_version: provenance.schema_version,
      signature_status: provenance.signature?.status,
    },
    tfvars: {
      name: basename(tfvarsPath),
      path: normalize(tfvarsPath),
      sha256: await hashFile(tfvarsPath),
    },
  },
  repository_family: repositoryFamily,
  deploy_images: deployImages,
  signature: {
    status: "unsigned",
  },
  index_sha256: null,
};

record.index_sha256 = sha256Text(JSON.stringify(record));
await Bun.write(out, `${JSON.stringify(record, null, 2)}\n`);
console.log(`wrote cloud image promotion evidence index ${out}`);
'

"${ROOT_DIR}/scripts/release/verify-cloud-image-promotion-evidence-index.sh" "${OUT}"
