#!/usr/bin/env bash
set -euo pipefail

ENV_FILE="${BUCEPHALUS_CONTROL_PLANE_ENV_FILE:-/etc/bucephalus-cloud/control-plane.env}"
API_URL="${BUCEPHALUS_CLOUD_API_URL:-}"
WORKER_TOKEN="${BUCEPHALUS_CLOUD_WORKER_TOKEN:-}"
CHECK_SYSTEMD="true"

usage() {
  cat <<'USAGE'
Usage: deploy/control-plane/smoke-control-plane.sh [options]

Smoke-check a Bucephalus Linux control-plane VM.

Options:
  --env-file <path>       Env file to load. Defaults to /etc/bucephalus-cloud/control-plane.env.
  --api-url <url>         Override BUCEPHALUS_CLOUD_API_URL.
  --worker-token <token>  Override BUCEPHALUS_CLOUD_WORKER_TOKEN.
  --no-systemd            Skip local systemd service checks.
  -h, --help              Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --env-file)
      ENV_FILE="${2:-}"
      shift 2
      ;;
    --api-url)
      API_URL="${2:-}"
      shift 2
      ;;
    --worker-token)
      WORKER_TOKEN="${2:-}"
      shift 2
      ;;
    --no-systemd)
      CHECK_SYSTEMD="false"
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

if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  . "${ENV_FILE}"
  set +a
fi

API_URL="${API_URL:-${BUCEPHALUS_CLOUD_API_URL:-http://127.0.0.1:${PORT:-8099}}}"
API_URL="${API_URL%/}"
WORKER_TOKEN="${WORKER_TOKEN:-${BUCEPHALUS_CLOUD_WORKER_TOKEN:-}}"

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

require_command curl

if [[ "${CHECK_SYSTEMD}" == "true" ]] && command -v systemctl >/dev/null 2>&1; then
  systemctl is-active --quiet bucephalus-cloud-api.service
  systemctl is-active --quiet bucephalus-cloud-pool-controller.service
fi

echo "== API health =="
curl -fsS "${API_URL}/healthz"
echo

echo "== API readiness =="
curl -fsS "${API_URL}/readyz"
echo

if [[ -n "${WORKER_TOKEN}" ]]; then
  echo "== Runner pools =="
  curl -fsS -H "authorization: Bearer ${WORKER_TOKEN}" "${API_URL}/v1/runner-pools"
  echo

  echo "== Provision requests =="
  curl -fsS -H "authorization: Bearer ${WORKER_TOKEN}" "${API_URL}/v1/runner-provision-requests?limit=20"
  echo
else
  echo "worker token not set; skipped runner management endpoint checks"
fi

echo "control-plane smoke passed: ${API_URL}"
