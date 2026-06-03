#!/usr/bin/env bash
set -euo pipefail

REPOSITORY=""
PUSH="false"
REQUIRE_READY="false"

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-registry-auth-boundary.sh --repository <repo-prefix> [--push] [--require-ready]

Preflights the registry authentication boundary for Cloud image publication.
Local image inspection does not require registry auth. Pushed image publication
requires OIDC/workload identity inputs. Use --require-ready at the point Docker
pushes can run; it requires the registry-auth step to have configured Docker/GAR
access and set BUCEPHALUS_GCP_REGISTRY_AUTH_READY=true.

This script intentionally rejects static credential surfaces. A generated
GOOGLE_APPLICATION_CREDENTIALS file from google-github-actions/auth is allowed
only when it is paired with the GOOGLE_GHA_CREDS_PATH marker.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repository)
      REPOSITORY="${2:-}"
      shift 2
      ;;
    --push)
      PUSH="true"
      shift
      ;;
    --require-ready)
      REQUIRE_READY="true"
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

if [[ -z "${REPOSITORY}" ]]; then
  usage >&2
  exit 2
fi

if [[ "${PUSH}" != "true" ]]; then
  exit 0
fi

if [[ -n "${GOOGLE_APPLICATION_CREDENTIALS:-}" ]]; then
  if [[ -z "${GOOGLE_GHA_CREDS_PATH:-}" || "${GOOGLE_APPLICATION_CREDENTIALS}" != "${GOOGLE_GHA_CREDS_PATH}" || "$(basename "${GOOGLE_APPLICATION_CREDENTIALS}")" != gha-creds-*.json ]]; then
    echo "pushed image publication must not use manual GOOGLE_APPLICATION_CREDENTIALS; use GitHub OIDC/workload identity generated credentials" >&2
    exit 1
  fi
fi

if [[ -n "${GOOGLE_CREDENTIALS:-}" || -n "${GCP_SERVICE_ACCOUNT_KEY:-}" || -n "${GCLOUD_SERVICE_KEY:-}" ]]; then
  echo "pushed image publication must not use static JSON service account key environment variables" >&2
  exit 1
fi

if [[ ! "${REPOSITORY}" =~ ^([a-z0-9-]+-docker\.pkg\.dev)/[a-z0-9][a-z0-9-]*/[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*$ ]]; then
  echo "pushed image publication requires a GCP Artifact Registry repository prefix" >&2
  exit 1
fi

if [[ -z "${BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER:-}" ]]; then
  echo "pushed image publication requires BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER for the OIDC auth boundary" >&2
  exit 1
fi

if [[ ! "${BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER}" =~ ^projects/[0-9]+/locations/global/workloadIdentityPools/[A-Za-z0-9_-]+/providers/[A-Za-z0-9_-]+$ ]]; then
  echo "BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER must be a workload identity provider resource name" >&2
  exit 1
fi

if [[ -z "${BUCEPHALUS_GCP_SERVICE_ACCOUNT:-}" ]]; then
  echo "pushed image publication requires BUCEPHALUS_GCP_SERVICE_ACCOUNT for the registry publisher identity" >&2
  exit 1
fi

if [[ ! "${BUCEPHALUS_GCP_SERVICE_ACCOUNT}" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.iam\.gserviceaccount\.com$ ]]; then
  echo "BUCEPHALUS_GCP_SERVICE_ACCOUNT must be a GCP service account email" >&2
  exit 1
fi

if [[ "${REQUIRE_READY}" == "true" && "${BUCEPHALUS_GCP_REGISTRY_AUTH_READY:-}" != "true" ]]; then
  echo "pushed image publication requires BUCEPHALUS_GCP_REGISTRY_AUTH_READY=true after an explicit OIDC registry-auth step" >&2
  exit 1
fi
