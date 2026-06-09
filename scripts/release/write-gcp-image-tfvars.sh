#!/usr/bin/env bash
set -euo pipefail

MANIFEST=""
OUT_PATH=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/write-gcp-image-tfvars.sh --image-manifest <cloud-image-build-manifest.json> --out <path>

Writes a Terraform tfvars fragment containing digest-addressed control-plane
image inputs for bucephalus-cloud/infra/gcp. The source image manifest must be
pushed and verified; local image-build manifests are inspection evidence only.
USAGE
}

public_image_manifest_input_ref() {
  printf '%s\n' "cloud-image-build-manifest://input"
}

public_tfvars_output_ref() {
  printf '%s\n' "tfvars://gcp-image-digests"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image-manifest)
      MANIFEST="${2:-}"
      shift 2
      ;;
    --out)
      OUT_PATH="${2:-}"
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

if [[ -z "${MANIFEST}" || -z "${OUT_PATH}" ]]; then
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

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

reject_symlinked_existing_components() {
  local path="$1"
  local current=""
  local part
  local -a parts
  if [[ -z "${path}" ]]; then
    echo "tfvars output directory is required" >&2
    echo "tfvars_ref: $(public_tfvars_output_ref)" >&2
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
      echo "tfvars output directory must be a stable path" >&2
      echo "tfvars_ref: $(public_tfvars_output_ref)" >&2
      exit 2
    fi
    if [[ -z "${current}" || "${current}" == "/" ]]; then
      current="${current}${part}"
    else
      current="${current}/${part}"
    fi
    if [[ -L "${current}" ]]; then
      echo "tfvars output directory must not contain symlinks" >&2
      echo "tfvars_ref: $(public_tfvars_output_ref)" >&2
      exit 2
    fi
  done
}

OUT_DIR="$(dirname "${OUT_PATH}")"
reject_symlinked_existing_components "${OUT_DIR}"
mkdir -p "${OUT_DIR}"
reject_symlinked_existing_components "${OUT_DIR}"
if [[ -L "${OUT_PATH}" ]]; then
  echo "tfvars output must not be a symlink" >&2
  echo "tfvars_ref: $(public_tfvars_output_ref)" >&2
  exit 2
fi
if [[ -e "${OUT_PATH}" && ! -f "${OUT_PATH}" ]]; then
  echo "tfvars output must be a regular file" >&2
  echo "tfvars_ref: $(public_tfvars_output_ref)" >&2
  exit 2
fi

"${ROOT_DIR}/scripts/release/verify-cloud-image-build-manifest.sh" "${MANIFEST}"

WORK_DIR="$(mktemp -d)"
OUT_TMP="$(mktemp "${OUT_DIR}/.gcp-image-digests.tfvars.XXXXXX")"
cleanup() {
  rm -rf "${WORK_DIR}"
  if [[ -n "${OUT_TMP:-}" && -e "${OUT_TMP}" ]]; then
    rm -f "${OUT_TMP}"
  fi
}
trap cleanup EXIT
WRITE_JS="${WORK_DIR}/write-gcp-image-tfvars.mjs"
cat > "${WRITE_JS}" <<'JS'
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
if (manifest.pushed !== true) {
  console.error("image manifest must be pushed=true to produce deploy tfvars");
  process.exit(1);
}

const byComponent = new Map(manifest.images.map((image) => [image.component, image]));
const required = {
  api_image_digest: "api",
  pool_controller_image_digest: "pool-controller",
  migration_image_digest: "migrations",
  worker_image_digest: "worker",
};
const garComponentRepo = /^([a-z0-9-]+-docker\.pkg\.dev\/[a-z0-9][a-z0-9-]*\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*)\/(api|pool-controller|migrations|worker)$/;

let deployRepositoryFamily = null;
for (const component of Object.values(required)) {
  const image = byComponent.get(component);
  const match = image?.image_repository?.match(garComponentRepo);
  if (!match || match[2] !== component) {
    fail(`${component}.image_repository must be a GCP Artifact Registry component repository`);
  }
  if (deployRepositoryFamily === null) {
    deployRepositoryFamily = match[1];
  } else if (match[1] !== deployRepositoryFamily) {
    fail("deploy tfvars image repositories must share one GCP Artifact Registry family");
  }
}

const lines = [
  "# Generated by scripts/release/write-gcp-image-tfvars.sh",
  "# Source: verified bucephalus_cloud_image_build_manifest_v1",
  `# Release version: ${manifest.release.version}`,
  `# Release git SHA: ${manifest.release.git_sha}`,
  `# Release manifest SHA256: ${manifest.release.manifest_sha256}`,
  "",
];

for (const [variable, component] of Object.entries(required)) {
  const image = byComponent.get(component);
  if (!image?.immutable_ref) {
    fail(`missing immutable image ref for ${component}`);
  }
  if (!image.immutable_ref.startsWith(`${image.image_repository}@`)) {
    fail(`${variable} must use the ${component} image_repository`);
  }
  lines.push(`${variable} = ${JSON.stringify(image.immutable_ref)}`);
}

lines.push("");
await Bun.write(process.env.OUT_PATH, lines.join("\n"));
console.log("tfvars=tfvars://gcp-image-digests");
JS
MANIFEST="${MANIFEST}" OUT_PATH="${OUT_TMP}" bun "${WRITE_JS}"
reject_symlinked_existing_components "${OUT_DIR}"
if [[ -L "${OUT_PATH}" ]]; then
  echo "tfvars output must not be a symlink" >&2
  echo "tfvars_ref: $(public_tfvars_output_ref)" >&2
  exit 2
fi
if [[ -e "${OUT_PATH}" && ! -f "${OUT_PATH}" ]]; then
  echo "tfvars output must be a regular file" >&2
  echo "tfvars_ref: $(public_tfvars_output_ref)" >&2
  exit 2
fi
mv -f "${OUT_TMP}" "${OUT_PATH}"
OUT_TMP=""
"${ROOT_DIR}/scripts/release/verify-gcp-image-tfvars.sh" \
  --image-manifest "${MANIFEST}" \
  --tfvars "${OUT_PATH}"
