#!/usr/bin/env bash
set -euo pipefail

PROJECT_ID=""
PROVISION_SECRET="buc-bucephalus-pool-provision-cmd-json"
REAP_SECRET="buc-bucephalus-pool-reap-cmd-json"

usage() {
  cat <<'USAGE'
Usage: scripts/deploy/write-gcp-runner-provider-command-secrets.sh --project-id <gcp-project> [--provision-secret <name>] [--reap-secret <name>]

Writes the GCE per-run provider command arrays into existing Secret Manager
secret names and prints only the created numeric versions.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project-id)
      PROJECT_ID="${2:-}"
      shift 2
      ;;
    --provision-secret)
      PROVISION_SECRET="${2:-}"
      shift 2
      ;;
    --reap-secret)
      REAP_SECRET="${2:-}"
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

if [[ -z "${PROJECT_ID}" ]]; then
  usage >&2
  exit 2
fi

if ! command -v gcloud >/dev/null 2>&1; then
  echo "required command not found: gcloud" >&2
  exit 2
fi

provision_version="$(
  printf '%s' '["bun","run","deploy/provider/gcp/provision-runner-vm.js"]' \
    | gcloud secrets versions add "${PROVISION_SECRET}" \
      --project="${PROJECT_ID}" \
      --data-file=- \
      --format='value(name)'
)"
reap_version="$(
  printf '%s' '["bun","run","deploy/provider/gcp/reap-runner-vm.js"]' \
    | gcloud secrets versions add "${REAP_SECRET}" \
      --project="${PROJECT_ID}" \
      --data-file=- \
      --format='value(name)'
)"

printf 'pool_controller_provision_cmd_json_version=%s\n' "${provision_version##*/}"
printf 'pool_controller_reap_cmd_json_version=%s\n' "${reap_version##*/}"
