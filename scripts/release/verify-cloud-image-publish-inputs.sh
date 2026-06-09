#!/usr/bin/env bash
set -euo pipefail

REPOSITORY=""
BASE_IMAGE=""
PUSH="false"

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-image-publish-inputs.sh --repository <repo-prefix> --base-image <image@sha256:digest> [--push]

Validates Cloud image build/publish inputs. Local image inspection may use a
throwaway repository prefix, but pushed publication must target the declared
first-cloud GCP Artifact Registry shape:

  <location>-docker.pkg.dev/<project>/<repository>/<image-prefix>

This script does not authenticate to the registry; it prevents accidental or
unreviewable publication destinations before docker buildx runs.
USAGE
}

public_repository_input_ref() {
  printf '%s\n' "image-repository://input"
}

public_base_image_input_ref() {
  printf '%s\n' "base-image://input"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repository)
      REPOSITORY="${2:-}"
      shift 2
      ;;
    --base-image)
      BASE_IMAGE="${2:-}"
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
      echo "unknown argument" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${REPOSITORY}" || -z "${BASE_IMAGE}" ]]; then
  usage >&2
  exit 2
fi

if [[ ! "${BASE_IMAGE}" =~ ^[^[:space:]]+@sha256:[a-f0-9]{64}$ ]]; then
  echo "--base-image must be digest-addressed" >&2
  echo "base_image_ref: $(public_base_image_input_ref)" >&2
  exit 2
fi
if [[ "${BASE_IMAGE}" == *":latest"* || "${BASE_IMAGE}" == http://* || "${BASE_IMAGE}" == https://* ]]; then
  echo "--base-image must not be a tag, URL, or latest reference" >&2
  echo "base_image_ref: $(public_base_image_input_ref)" >&2
  exit 2
fi

if [[ "${REPOSITORY}" =~ [[:space:]] || "${REPOSITORY}" == *":latest" || "${REPOSITORY}" == *"@sha256:" || "${REPOSITORY}" == http://* || "${REPOSITORY}" == https://* ]]; then
  echo "--repository must be an image repository prefix, not a URL, tag, digest, or whitespace-bearing value" >&2
  echo "repository_ref: $(public_repository_input_ref)" >&2
  exit 2
fi

for forbidden in \
  "DATABASE_URL" \
  "BUCEPHALUS_CLOUD_WORKER_TOKEN" \
  "GOOGLE_APPLICATION_CREDENTIALS" \
  "TAILSCALE_AUTHKEY" \
  "ghp_" \
  "github_pat_" \
  "ya29."; do
  if [[ "${REPOSITORY}" == *"${forbidden}"* || "${BASE_IMAGE}" == *"${forbidden}"* ]]; then
    echo "image publish input contains forbidden-looking secret/runtime material" >&2
    echo "repository_ref: $(public_repository_input_ref)" >&2
    echo "base_image_ref: $(public_base_image_input_ref)" >&2
    exit 1
  fi
done

if [[ "${PUSH}" != "true" ]]; then
  exit 0
fi

if [[ "${REPOSITORY}" == "bucephalus-cloud-ci" || "${REPOSITORY}" == example.* || "${REPOSITORY}" == */example/* || "${REPOSITORY}" == localhost* || "${REPOSITORY}" == 127.* || "${REPOSITORY}" == docker.io/* || "${REPOSITORY}" == index.docker.io/* ]]; then
  echo "pushed Cloud images must not target local, example, default, or Docker Hub repositories" >&2
  echo "repository_ref: $(public_repository_input_ref)" >&2
  exit 1
fi

if [[ ! "${REPOSITORY}" =~ ^[a-z0-9-]+-docker\.pkg\.dev/[a-z0-9][a-z0-9-]*/[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*$ ]]; then
  echo "pushed Cloud images must target GCP Artifact Registry as <location>-docker.pkg.dev/<project>/<repository>/<image-prefix>" >&2
  echo "repository_ref: $(public_repository_input_ref)" >&2
  exit 1
fi
