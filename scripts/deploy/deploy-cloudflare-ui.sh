#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARTIFACT_DIR=""
DIST_DIR=""
WORKER_NAME="bucephalus-cloud-ui"
API_BASE=""
ACCOUNT_ID="${CLOUDFLARE_ACCOUNT_ID:-}"
COMPATIBILITY_DATE="2026-06-04"

usage() {
  cat <<'USAGE'
Usage: scripts/deploy/deploy-cloudflare-ui.sh [--artifact <cloud-ui-artifact-dir> | --dist <dist-dir>] [--worker-name <name>] [--api-base <url>] [--account-id <id>]

Deploys the Bucephalus Cloud UI to Cloudflare Workers Static Assets. Local
Wrangler auth is supported; CI should provide CLOUDFLARE_API_TOKEN and usually
CLOUDFLARE_ACCOUNT_ID.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifact)
      ARTIFACT_DIR="${2:-}"
      shift 2
      ;;
    --dist)
      DIST_DIR="${2:-}"
      shift 2
      ;;
    --worker-name)
      WORKER_NAME="${2:-}"
      shift 2
      ;;
    --api-base)
      API_BASE="${2:-}"
      shift 2
      ;;
    --account-id)
      ACCOUNT_ID="${2:-}"
      shift 2
      ;;
    --compatibility-date)
      COMPATIBILITY_DATE="${2:-}"
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

if [[ -n "${ARTIFACT_DIR}" && -n "${DIST_DIR}" ]]; then
  echo "choose --artifact or --dist, not both" >&2
  exit 2
fi
if [[ -z "${ARTIFACT_DIR}" && -z "${DIST_DIR}" ]]; then
  ARTIFACT_DIR="${ROOT_DIR}/dist/releases/cloud-ui"
fi
if [[ ! "${WORKER_NAME}" =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]]; then
  echo "--worker-name must be a Cloudflare-compatible lowercase name" >&2
  exit 2
fi
if [[ -n "${ACCOUNT_ID}" && ! "${ACCOUNT_ID}" =~ ^[A-Za-z0-9_-]+$ ]]; then
  echo "--account-id contains unsupported characters" >&2
  exit 2
fi
if [[ -n "${API_BASE}" && ! "${API_BASE}" =~ ^https?:// ]]; then
  echo "--api-base must be an http(s) URL" >&2
  exit 2
fi

if [[ -n "${ARTIFACT_DIR}" ]]; then
  "${ROOT_DIR}/scripts/release/verify-cloud-ui-assets.sh" "${ARTIFACT_DIR}"
  DIST_DIR="${ARTIFACT_DIR}/dist"
fi
if [[ ! -f "${DIST_DIR}/index.html" ]]; then
  echo "UI dist is missing index.html: ${DIST_DIR}" >&2
  exit 1
fi
if [[ ! -f "${ROOT_DIR}/bucephalus-cloud/web/worker.ts" ]]; then
  echo "Worker shell is missing" >&2
  exit 1
fi

if command -v wrangler >/dev/null 2>&1; then
  WRANGLER=(wrangler)
elif command -v bunx >/dev/null 2>&1; then
  WRANGLER=(bunx wrangler)
else
  echo "wrangler or bunx is required" >&2
  exit 2
fi
if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

toml_string() {
  bun -e 'console.log(JSON.stringify(process.argv[1] ?? ""))' "$1"
}

WORK_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

cp "${ROOT_DIR}/bucephalus-cloud/web/worker.ts" "${WORK_DIR}/worker.ts"
{
  echo "name = $(toml_string "${WORKER_NAME}")"
  if [[ -n "${ACCOUNT_ID}" ]]; then
    echo "account_id = $(toml_string "${ACCOUNT_ID}")"
  fi
  echo "main = \"./worker.ts\""
  echo "compatibility_date = $(toml_string "${COMPATIBILITY_DATE}")"
  echo ""
  echo "[assets]"
  echo "directory = $(toml_string "$(cd "${DIST_DIR}" && pwd)")"
  echo "binding = \"ASSETS\""
  echo "not_found_handling = \"single-page-application\""
  echo "run_worker_first = [\"/buc-config.js\"]"
  echo ""
  echo "[vars]"
  echo "BUCEPHALUS_API_BASE = $(toml_string "${API_BASE}")"
} > "${WORK_DIR}/wrangler.toml"

"${WRANGLER[@]}" deploy --config "${WORK_DIR}/wrangler.toml"
