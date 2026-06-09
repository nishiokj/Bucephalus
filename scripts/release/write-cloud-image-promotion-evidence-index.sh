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
  if [[ -L "${file}" ]]; then
    echo "promotion evidence file must not be a symlink" >&2
    exit 2
  fi
  if [[ ! -f "${file}" ]]; then
    echo "promotion evidence file does not exist" >&2
    exit 2
  fi
done

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

reject_symlinked_existing_components() {
  local path="$1"
  local current=""
  local part
  local -a parts
  if [[ -z "${path}" ]]; then
    echo "promotion evidence output directory is required" >&2
    exit 2
  fi
  if [[ "${path}" == "/tmp" || "${path}" == /tmp/* ]]; then
    if [[ -L /tmp && -d /private/tmp ]]; then
      path="/private/tmp${path#/tmp}"
    fi
  elif [[ "${path}" == "/var" || "${path}" == /var/* ]]; then
    if [[ -L /var && -d /private/var ]]; then
      path="/private/var${path#/var}"
    fi
  fi
  if [[ "${path}" == /* ]]; then
    current="/"
    path="${path#/}"
  fi
  IFS='/' read -r -a parts <<< "${path}"
  for part in "${parts[@]}"; do
    if [[ -z "${part}" || "${part}" == "." ]]; then
      continue
    fi
    if [[ "${part}" == ".." ]]; then
      echo "promotion evidence output directory must be a stable path" >&2
      exit 2
    fi
    if [[ -z "${current}" || "${current}" == "/" ]]; then
      current="${current}${part}"
    else
      current="${current}/${part}"
    fi
    if [[ -L "${current}" ]]; then
      echo "promotion evidence output directory must not contain symlinks" >&2
      exit 2
    fi
  done
}

OUT_DIR="$(dirname "${OUT}")"
reject_symlinked_existing_components "${OUT_DIR}"
mkdir -p "${OUT_DIR}"
reject_symlinked_existing_components "${OUT_DIR}"
if [[ -L "${OUT}" ]]; then
  echo "promotion evidence output must not be a symlink" >&2
  exit 2
fi
if [[ -e "${OUT}" && ! -f "${OUT}" ]]; then
  echo "promotion evidence output must be a regular file" >&2
  exit 2
fi

"${ROOT_DIR}/scripts/release/verify-gcp-image-promotion-evidence.sh" \
  --image-manifest "${MANIFEST}" \
  --image-provenance "${PROVENANCE}" \
  --tfvars "${TFVARS}"

OUT_TMP="$(mktemp "${OUT_DIR}/.cloud-image-promotion-evidence.json.XXXXXX")"
cleanup() {
  if [[ -n "${OUT_TMP:-}" && -e "${OUT_TMP}" ]]; then
    rm -f "${OUT_TMP}"
  fi
}
trap cleanup EXIT

MANIFEST="${MANIFEST}" PROVENANCE="${PROVENANCE}" TFVARS="${TFVARS}" ROOT_DIR="${ROOT_DIR}" OUT="${OUT_TMP}" bun -e '
import { createHash } from "node:crypto";
import { lstatSync } from "node:fs";
import { basename, relative, resolve } from "node:path";

const manifestPath = process.env.MANIFEST;
const provenancePath = process.env.PROVENANCE;
const tfvarsPath = process.env.TFVARS;
const out = process.env.OUT;
const rootDir = resolve(process.env.ROOT_DIR);
const garComponentRepo = /^([a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*)\/(api|pool-controller|migrations|worker)$/;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function sha256Text(value) {
  return createHash("sha256").update(value).digest("hex");
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
  return normalized;
}

function looksLikeHostPath(path) {
  const normalized = path.replace(/^\.\//, "");
  return /^(Users|home|private|tmp|var\/folders|Volumes|Desktop|Documents|Downloads|runner\/work|github\/workspace)\//.test(normalized);
}

const manifest = JSON.parse(await readTextFile(manifestPath, "image manifest"));
const provenance = JSON.parse(await readTextFile(provenancePath, "image provenance"));

const deployComponents = ["api", "pool-controller", "migrations", "worker"];
const images = new Map(manifest.images.map((image) => [image.component, image]));
let repositoryFamily = null;
const deployImages = deployComponents.map((component) => {
  const image = images.get(component);
  const match = image.image_repository.match(garComponentRepo);
  if (!match || match[2] !== component) {
    fail(`${component}.image_repository must be a GCP Artifact Registry deploy component repository`);
  }
  if (repositoryFamily === null) {
    repositoryFamily = match[1];
  } else if (repositoryFamily !== match[1]) {
    fail("image promotion evidence index deploy images must share one GCP Artifact Registry family");
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
      path: artifactPath(manifestPath, "evidence.image_manifest.path"),
      sha256: await hashFile(manifestPath, "image manifest"),
      schema_version: manifest.schema_version,
    },
    image_provenance: {
      name: basename(provenancePath),
      path: artifactPath(provenancePath, "evidence.image_provenance.path"),
      sha256: await hashFile(provenancePath, "image provenance"),
      schema_version: provenance.schema_version,
      signature_status: provenance.signature?.status,
    },
    tfvars: {
      name: basename(tfvarsPath),
      path: artifactPath(tfvarsPath, "evidence.tfvars.path"),
      sha256: await hashFile(tfvarsPath, "promotion tfvars"),
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
console.log("wrote cloud image promotion evidence index promotion-evidence://cloud-image-promotion-evidence");
'

reject_symlinked_existing_components "${OUT_DIR}"
if [[ -L "${OUT}" ]]; then
  echo "promotion evidence output must not be a symlink" >&2
  exit 2
fi
if [[ -e "${OUT}" && ! -f "${OUT}" ]]; then
  echo "promotion evidence output must be a regular file" >&2
  exit 2
fi
mv -f "${OUT_TMP}" "${OUT}"
OUT_TMP=""
"${ROOT_DIR}/scripts/release/verify-cloud-image-promotion-evidence-index.sh" "${OUT}"
