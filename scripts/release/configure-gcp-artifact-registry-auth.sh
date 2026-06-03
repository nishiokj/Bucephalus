#!/usr/bin/env bash
set -euo pipefail

REPOSITORY=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/configure-gcp-artifact-registry-auth.sh --repository <repo-prefix>

Configures Docker to authenticate to the GCP Artifact Registry hostname for a
Bucephalus Cloud image repository prefix. This script expects the workflow to
have already authenticated gcloud through GitHub OIDC/Workload Identity, not a
static service-account key.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repository)
      REPOSITORY="${2:-}"
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

if [[ -z "${REPOSITORY}" ]]; then
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

if [[ -n "${GOOGLE_APPLICATION_CREDENTIALS:-}" ]]; then
  if [[ -z "${GOOGLE_GHA_CREDS_PATH:-}" || "${GOOGLE_APPLICATION_CREDENTIALS}" != "${GOOGLE_GHA_CREDS_PATH}" || "$(basename "${GOOGLE_APPLICATION_CREDENTIALS}")" != gha-creds-*.json ]]; then
    echo "registry auth must not use manual GOOGLE_APPLICATION_CREDENTIALS; use GitHub OIDC/workload identity generated credentials" >&2
    exit 1
  fi
fi

for forbidden in GOOGLE_CREDENTIALS GCP_SERVICE_ACCOUNT_KEY GCLOUD_SERVICE_KEY; do
  if [[ -n "${!forbidden:-}" ]]; then
    echo "registry auth must not use static credential surface ${forbidden}; use GitHub OIDC/workload identity" >&2
    exit 1
  fi
done

if [[ ! "${REPOSITORY}" =~ ^([a-z0-9-]+-docker\.pkg\.dev)/[a-z0-9][a-z0-9-]*/[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*$ ]]; then
  echo "registry auth requires a GCP Artifact Registry repository prefix" >&2
  exit 1
fi
REGISTRY_HOST="${BASH_REMATCH[1]}"

if [[ -z "${BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER:-}" ]]; then
  echo "registry auth requires BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER" >&2
  exit 1
fi

if [[ ! "${BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER}" =~ ^projects/[0-9]+/locations/global/workloadIdentityPools/[A-Za-z0-9_-]+/providers/[A-Za-z0-9_-]+$ ]]; then
  echo "BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER must be a workload identity provider resource name" >&2
  exit 1
fi

if [[ -z "${BUCEPHALUS_GCP_SERVICE_ACCOUNT:-}" ]]; then
  echo "registry auth requires BUCEPHALUS_GCP_SERVICE_ACCOUNT" >&2
  exit 1
fi

if [[ ! "${BUCEPHALUS_GCP_SERVICE_ACCOUNT}" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.iam\.gserviceaccount\.com$ ]]; then
  echo "BUCEPHALUS_GCP_SERVICE_ACCOUNT must be a GCP service account email" >&2
  exit 1
fi

require_command bun
require_command docker
require_command gcloud

active_account="$(gcloud auth list --filter=status:ACTIVE --format='value(account)' 2>/dev/null | head -n 1 || true)"
if [[ -z "${active_account}" ]]; then
  echo "gcloud has no active account; run google-github-actions/auth before registry auth" >&2
  exit 1
fi

if [[ "${active_account}" != "${BUCEPHALUS_GCP_SERVICE_ACCOUNT}" ]]; then
  echo "active gcloud account ${active_account} does not match BUCEPHALUS_GCP_SERVICE_ACCOUNT" >&2
  exit 1
fi

gcloud auth configure-docker "${REGISTRY_HOST}" --quiet

DOCKER_CONFIG_PATH="${DOCKER_CONFIG:-${HOME}/.docker}/config.json"
if [[ ! -f "${DOCKER_CONFIG_PATH}" ]]; then
  echo "Docker config was not written after Artifact Registry auth: ${DOCKER_CONFIG_PATH}" >&2
  exit 1
fi

DOCKER_CONFIG_PATH="${DOCKER_CONFIG_PATH}" REGISTRY_HOST="${REGISTRY_HOST}" bun -e '
const config = JSON.parse(await Bun.file(process.env.DOCKER_CONFIG_PATH).text());
const helper = config.credHelpers?.[process.env.REGISTRY_HOST];
if (helper !== "gcloud") {
  console.error(`Docker config missing gcloud credential helper for ${process.env.REGISTRY_HOST}`);
  process.exit(1);
}
'

if [[ -n "${GITHUB_ENV:-}" ]]; then
  {
    echo "BUCEPHALUS_GCP_REGISTRY_AUTH_READY=true"
    echo "BUCEPHALUS_GCP_REGISTRY_HOST=${REGISTRY_HOST}"
  } >> "${GITHUB_ENV}"
fi

echo "configured Artifact Registry Docker auth for ${REGISTRY_HOST}"
