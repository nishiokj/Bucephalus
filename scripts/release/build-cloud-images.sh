#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RELEASE_INPUT=""
REPOSITORY=""
BASE_IMAGE=""
OUT_DIR=""
PUSH="false"
WORK_DIR=""
RELEASE_ARCHIVE_SHA=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/build-cloud-images.sh --release <release-dir-or-tar.gz> --repository <repo> --base-image <image@sha256:digest> [--out <dir>] [--push]

Builds Bucephalus Cloud images from a verified release bundle:
  - api
  - pool-controller
  - migrations
  - worker

The base image must be digest-addressed. Without --push, images are loaded into
the local Docker daemon and are not valid deploy inputs. With --push, the output
manifest records the registry digest returned by docker buildx metadata after a
local image-boundary inspection pass.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      RELEASE_INPUT="${2:-}"
      shift 2
      ;;
    --repository)
      REPOSITORY="${2:-}"
      shift 2
      ;;
    --base-image)
      BASE_IMAGE="${2:-}"
      shift 2
      ;;
    --out)
      OUT_DIR="${2:-}"
      shift 2
      ;;
    --push)
      PUSH="true"
      shift
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

json_get() {
  local file="$1"
  local expr="$2"
  bun -e "const data = JSON.parse(await Bun.file(process.argv[1]).text()); const value = ${expr}; if (value === undefined || value === null) process.exit(1); console.log(value);" "${file}"
}

copy_context_path() {
  local src="$1"
  local dst="$2"
  mkdir -p "$(dirname "${dst}")"
  cp -R "${src}" "${dst}"
}

prepare_image_context() {
  local component="$1"
  local context_dir="${OUT_DIR}/contexts/${component}"
  rm -rf "${context_dir}"
  mkdir -p "${context_dir}/bucephalus-cloud/images"

  cp "${RELEASE_DIR}/.dockerignore" "${context_dir}/.dockerignore"
  copy_context_path "${RELEASE_DIR}/bucephalus-cloud/images/Dockerfile.${component}" "${context_dir}/bucephalus-cloud/images/Dockerfile.${component}"

  case "${component}" in
    api)
      copy_context_path "${RELEASE_DIR}/bucephalus-cloud/runtime-dist/server.js" "${context_dir}/bucephalus-cloud/runtime-dist/server.js"
      ;;
    migrations)
      copy_context_path "${RELEASE_DIR}/bucephalus-cloud/runtime-dist/db/migrate.js" "${context_dir}/bucephalus-cloud/runtime-dist/db/migrate.js"
      copy_context_path "${RELEASE_DIR}/bucephalus-cloud/db" "${context_dir}/bucephalus-cloud/db"
      ;;
    pool-controller)
      copy_context_path "${RELEASE_DIR}/bucephalus-cloud/runtime-dist/poolController.js" "${context_dir}/bucephalus-cloud/runtime-dist/poolController.js"
      ;;
    worker)
      copy_context_path "${RELEASE_DIR}/bucephalus-cloud/runtime-dist/worker.js" "${context_dir}/bucephalus-cloud/runtime-dist/worker.js"
      copy_context_path "${RELEASE_DIR}/bucephalus-cloud/runtime-dist/secretResolver.js" "${context_dir}/bucephalus-cloud/runtime-dist/secretResolver.js"
      copy_context_path "${RELEASE_DIR}/bin/bucephalus" "${context_dir}/bin/bucephalus"
      ;;
    *)
      echo "unsupported image component: ${component}" >&2
      exit 2
      ;;
  esac

  printf '%s\n' "${context_dir}"
}

cleanup() {
  if [[ -n "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

if [[ -z "${RELEASE_INPUT}" || -z "${REPOSITORY}" || -z "${BASE_IMAGE}" ]]; then
  usage >&2
  exit 2
fi

publish_input_args=(
  --repository "${REPOSITORY}"
  --base-image "${BASE_IMAGE}"
)
if [[ "${PUSH}" == "true" ]]; then
  publish_input_args+=(--push)
fi
"${ROOT_DIR}/scripts/release/verify-cloud-image-publish-inputs.sh" "${publish_input_args[@]}"
base_policy_args=(--base-image "${BASE_IMAGE}")
if [[ "${PUSH}" == "true" ]]; then
  base_policy_args+=(--push)
fi
"${ROOT_DIR}/scripts/release/verify-cloud-base-image-policy.sh" "${base_policy_args[@]}"
registry_auth_args=(--repository "${REPOSITORY}")
if [[ "${PUSH}" == "true" ]]; then
  registry_auth_args+=(--push --require-ready)
fi
"${ROOT_DIR}/scripts/release/verify-cloud-registry-auth-boundary.sh" "${registry_auth_args[@]}"

require_command bun
require_command docker
require_command tar

"${ROOT_DIR}/scripts/release/verify-buc-release.sh" "${RELEASE_INPUT}"

if [[ -d "${RELEASE_INPUT}" ]]; then
  RELEASE_DIR="${RELEASE_INPUT}"
else
  RELEASE_ARCHIVE_SHA="$(sha256_file "${RELEASE_INPUT}")"
  WORK_DIR="$(mktemp -d)"
  tar -xzf "${RELEASE_INPUT}" -C "${WORK_DIR}"
  RELEASE_DIR="$(find "${WORK_DIR}" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
fi

if [[ -z "${RELEASE_DIR}" || ! -f "${RELEASE_DIR}/release-manifest.json" ]]; then
  echo "could not resolve release directory from ${RELEASE_INPUT}" >&2
  exit 1
fi
if [[ ! -f "${RELEASE_DIR}/.dockerignore" ]]; then
  echo "release bundle is missing .dockerignore image context guard" >&2
  exit 1
fi
for required_pattern in 'gha-creds-*.json' '**/gha-creds-*.json' '*.env' '**/*.env' '*.env.example' '**/*.env.example' 'image-build/' '**/image-build/' '*.metadata.json' '*.iid'; do
  if ! grep -Fxq "${required_pattern}" "${RELEASE_DIR}/.dockerignore"; then
    echo "release .dockerignore is missing required image context exclusion: ${required_pattern}" >&2
    exit 1
  fi
done

VERSION="$(json_get "${RELEASE_DIR}/release-manifest.json" "data.version")"
TARGET="$(json_get "${RELEASE_DIR}/release-manifest.json" "data.target")"
GIT_SHA="$(json_get "${RELEASE_DIR}/release-manifest.json" "data.git_sha")"
if [[ "${TARGET}" != *"linux"* ]]; then
  echo "Cloud images require a Linux release bundle; got target ${TARGET}" >&2
  exit 2
fi
RELEASE_MANIFEST_SHA="$(sha256_file "${RELEASE_DIR}/release-manifest.json")"
DOCKERIGNORE_SHA="$(sha256_file "${RELEASE_DIR}/.dockerignore")"
TAG_SUFFIX="$(printf "%s-%s-%s" "${VERSION}" "${TARGET}" "${GIT_SHA:0:12}" | tr -c 'A-Za-z0-9_.-' '-')"
OUT_DIR="${OUT_DIR:-${RELEASE_DIR}/image-build}"
mkdir -p "${OUT_DIR}"

COMPONENTS=(api pool-controller migrations worker)
ENTRIES_JSONL="${OUT_DIR}/image-entries.jsonl"
: > "${ENTRIES_JSONL}"

for component in "${COMPONENTS[@]}"; do
  release_dockerfile="${RELEASE_DIR}/bucephalus-cloud/images/Dockerfile.${component}"
  if [[ ! -f "${release_dockerfile}" ]]; then
    echo "missing image Dockerfile for ${component}: ${release_dockerfile}" >&2
    exit 1
  fi
  context_dir="$(prepare_image_context "${component}")"
  dockerfile="${context_dir}/bucephalus-cloud/images/Dockerfile.${component}"
  dockerfile_path="bucephalus-cloud/images/Dockerfile.${component}"
  dockerfile_sha="$(sha256_file "${release_dockerfile}")"

  image_repository="${REPOSITORY}/${component}"
  image_ref="${image_repository}:${TAG_SUFFIX}"
  boundary_ref="${image_ref}-boundary-check"
  cache_ref="${image_repository}:buildcache"
  metadata_file="${OUT_DIR}/${component}.metadata.json"
  boundary_metadata_file="${OUT_DIR}/${component}.boundary.metadata.json"
  metadata_name="${component}.metadata.json"
  boundary_metadata_name="${component}.boundary.metadata.json"
  iid_file="${OUT_DIR}/${component}.iid"
  boundary_iid_file="${OUT_DIR}/${component}.boundary.iid"
  inspected_ref="${image_ref}"
  echo "== Building ${component} image =="
  component_started_at="${SECONDS}"
  build_seconds=0
  boundary_verify_seconds=0
  push_seconds=0
  common_build_args=(
    buildx build
    --file "${dockerfile}"
    --build-arg "BUCEPHALUS_BUN_BASE_IMAGE=${BASE_IMAGE}"
    --build-arg "BUCEPHALUS_RELEASE_VERSION=${VERSION}"
    --build-arg "BUCEPHALUS_RELEASE_GIT_SHA=${GIT_SHA}"
    --build-arg "BUCEPHALUS_RELEASE_MANIFEST_SHA256=${RELEASE_MANIFEST_SHA}"
    --build-arg "BUCEPHALUS_IMAGE_COMPONENT=${component}"
    --label "org.opencontainers.image.source_revision=${GIT_SHA}"
    --label "org.bucephalus.release.manifest.sha256=${RELEASE_MANIFEST_SHA}"
  )

  if [[ "${PUSH}" == "true" ]]; then
    inspected_ref="${boundary_ref}"
    step_started_at="${SECONDS}"
    docker "${common_build_args[@]}" \
      --cache-from "type=registry,ref=${cache_ref}" \
      --cache-to "type=registry,ref=${cache_ref},mode=max" \
      --tag "${boundary_ref}" \
      --iidfile "${boundary_iid_file}" \
      --metadata-file "${boundary_metadata_file}" \
      --load \
      "${context_dir}"
    build_seconds=$((SECONDS - step_started_at))
    step_started_at="${SECONDS}"
    "${ROOT_DIR}/scripts/release/verify-cloud-image-boundary.sh" "${boundary_ref}" \
      --component "${component}" \
      --release-manifest-sha256 "${RELEASE_MANIFEST_SHA}"
    boundary_verify_seconds=$((SECONDS - step_started_at))
    step_started_at="${SECONDS}"
    docker tag "${boundary_ref}" "${image_ref}"
    push_output="$(docker push "${image_ref}")"
    printf '%s\n' "${push_output}"
    digest="$(printf '%s\n' "${push_output}" | awk '$0 ~ /digest: sha256:[a-f0-9]{64}/ { for (i = 1; i <= NF; i++) if ($i == "digest:") print $(i + 1) }' | tail -n 1)"
    push_seconds=$((SECONDS - step_started_at))
    cp "${boundary_iid_file}" "${iid_file}"
    printf '{\n  "containerimage.digest": "%s"\n}\n' "${digest}" > "${metadata_file}"
  else
    step_started_at="${SECONDS}"
    docker "${common_build_args[@]}" \
      --tag "${image_ref}" \
      --iidfile "${iid_file}" \
      --metadata-file "${metadata_file}" \
      --load \
      "${context_dir}"
    build_seconds=$((SECONDS - step_started_at))
    cp "${iid_file}" "${boundary_iid_file}"
    cp "${metadata_file}" "${boundary_metadata_file}"
    step_started_at="${SECONDS}"
    "${ROOT_DIR}/scripts/release/verify-cloud-image-boundary.sh" "${image_ref}" \
      --component "${component}" \
      --release-manifest-sha256 "${RELEASE_MANIFEST_SHA}"
    boundary_verify_seconds=$((SECONDS - step_started_at))
  fi
  component_seconds=$((SECONDS - component_started_at))

  image_id="$(cat "${iid_file}")"
  boundary_image_id="$(cat "${boundary_iid_file}")"
  digest=""
  if [[ -f "${metadata_file}" ]]; then
    digest="$(bun -e 'const p = process.argv[1]; const data = JSON.parse(await Bun.file(p).text()); console.log(data["containerimage.digest"] ?? "");' "${metadata_file}")"
  fi
  if [[ "${PUSH}" == "true" && ! "${digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
    echo "docker buildx did not report a registry digest for pushed ${component} image" >&2
    exit 1
  fi
  immutable_ref=""
  if [[ -n "${digest}" ]]; then
    immutable_ref="${image_repository}@${digest}"
  fi
  bun -e 'const [component, imageRepository, imageRef, immutableRef, imageId, digest, metadataFile, boundaryImageRef, boundaryImageId, boundaryMetadataFile, dockerfilePath, dockerfileSha256, buildSeconds, boundaryVerifySeconds, pushSeconds, componentSeconds] = process.argv.slice(1); console.log(JSON.stringify({ component, image_repository: imageRepository, tag_ref: imageRef, immutable_ref: immutableRef || null, image_id: imageId, digest: digest || null, metadata_file: metadataFile, boundary_verified: true, boundary_image_ref: boundaryImageRef, boundary_image_id: boundaryImageId, boundary_metadata_file: boundaryMetadataFile, dockerfile: { path: dockerfilePath, sha256: dockerfileSha256 }, timings_seconds: { build: Number(buildSeconds), boundary_verify: Number(boundaryVerifySeconds), push: Number(pushSeconds), total: Number(componentSeconds) } }));' \
    "${component}" "${image_repository}" "${image_ref}" "${immutable_ref}" "${image_id}" "${digest}" "${metadata_name}" "${inspected_ref}" "${boundary_image_id}" "${boundary_metadata_name}" "${dockerfile_path}" "${dockerfile_sha}" "${build_seconds}" "${boundary_verify_seconds}" "${push_seconds}" "${component_seconds}" >> "${ENTRIES_JSONL}"
done

MANIFEST_PATH="${OUT_DIR}/cloud-image-build-manifest.json"
if [[ -z "${WORK_DIR}" ]]; then
  WORK_DIR="$(mktemp -d)"
fi
WRITE_MANIFEST_JS="${WORK_DIR}/write-cloud-image-build-manifest.mjs"
cat > "${WRITE_MANIFEST_JS}" <<'JS'
const entries = (await Bun.file(process.env.ENTRIES_JSONL).text())
  .split("\n")
  .filter(Boolean)
  .map((line) => JSON.parse(line));
const manifest = JSON.parse(await Bun.file(`${process.env.RELEASE_DIR}/release-manifest.json`).text());
const isGithubActions = process.env.GITHUB_ACTIONS === "true";
await Bun.write(process.env.MANIFEST_PATH, `${JSON.stringify({
  schema_version: "bucephalus_cloud_image_build_manifest_v1",
  release: {
    version: manifest.version,
    target: manifest.target,
    git_sha: manifest.git_sha,
    manifest_sha256: process.env.RELEASE_MANIFEST_SHA,
    archive_sha256: process.env.RELEASE_ARCHIVE_SHA || null,
  },
  image_context: {
    path: ".dockerignore",
    sha256: process.env.DOCKERIGNORE_SHA,
  },
  base_image: process.env.BASE_IMAGE,
  builder: {
    kind: isGithubActions ? "github_actions" : "local",
    github_server_url: isGithubActions ? process.env.GITHUB_SERVER_URL || null : null,
    github_repository: isGithubActions ? process.env.GITHUB_REPOSITORY || null : null,
    github_run_id: isGithubActions ? process.env.GITHUB_RUN_ID || null : null,
    github_run_attempt: isGithubActions ? process.env.GITHUB_RUN_ATTEMPT || null : null,
    github_workflow: isGithubActions ? process.env.GITHUB_WORKFLOW || null : null,
    github_ref: isGithubActions ? process.env.GITHUB_REF || null : null,
    github_sha: isGithubActions ? process.env.GITHUB_SHA || null : null,
  },
  pushed: process.env.PUSH === "true",
  images: entries,
}, null, 2)}\n`);
JS
ENTRIES_JSONL="${ENTRIES_JSONL}" MANIFEST_PATH="${MANIFEST_PATH}" RELEASE_MANIFEST_SHA="${RELEASE_MANIFEST_SHA}" DOCKERIGNORE_SHA="${DOCKERIGNORE_SHA}" RELEASE_ARCHIVE_SHA="${RELEASE_ARCHIVE_SHA}" RELEASE_DIR="${RELEASE_DIR}" BASE_IMAGE="${BASE_IMAGE}" PUSH="${PUSH}" bun "${WRITE_MANIFEST_JS}"

"${ROOT_DIR}/scripts/release/verify-cloud-image-build-manifest.sh" "${MANIFEST_PATH}" --release "${RELEASE_INPUT}"
echo "image_manifest=${MANIFEST_PATH}"
