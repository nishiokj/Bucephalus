#!/usr/bin/env bash
set -euo pipefail

INSTALL_ROOT="/opt/bucephalus"
ENV_DIR="/etc/bucephalus-cloud"
ENV_FILE="${ENV_DIR}/control-plane.env"
SERVICE_USER="bucephalus-cloud"
SERVICE_GROUP="bucephalus-cloud"
RELEASE_DIR=""
START_SERVICES="false"
RUN_MIGRATIONS="false"
INSTALL_CORE_BIN="true"

usage() {
  cat <<'USAGE'
Usage: deploy/control-plane/install-control-plane.sh [options]

Install a Bucephalus release bundle as a Linux control-plane VM:
  - /opt/bucephalus/releases/<release>
  - /opt/bucephalus/current -> selected release
  - /etc/systemd/system/bucephalus-cloud-api.service
  - /etc/systemd/system/bucephalus-cloud-pool-controller.service
  - /etc/bucephalus-cloud/control-plane.env sample if missing

Options:
  --release-dir <dir>       Release root. Defaults to the bundle containing this script.
  --install-root <dir>      Install root. Defaults to /opt/bucephalus.
  --env-file <path>         Env file. Defaults to /etc/bucephalus-cloud/control-plane.env.
  --user <name>             Service user. Defaults to bucephalus-cloud.
  --group <name>            Service group. Defaults to bucephalus-cloud.
  --migrate                 Run bun run db:migrate after install.
  --start                   Enable and restart API and pool-controller services.
  --no-core-bin             Do not install /usr/local/bin/bucephalus.
  -h, --help                Show this help.

This script intentionally does not create databases, users, or DDL grants. Run
Postgres provisioning and migrations from the admin/deploy side.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-dir)
      RELEASE_DIR="${2:-}"
      shift 2
      ;;
    --install-root)
      INSTALL_ROOT="${2:-}"
      shift 2
      ;;
    --env-file)
      ENV_FILE="${2:-}"
      ENV_DIR="$(dirname "${ENV_FILE}")"
      shift 2
      ;;
    --user)
      SERVICE_USER="${2:-}"
      shift 2
      ;;
    --group)
      SERVICE_GROUP="${2:-}"
      shift 2
      ;;
    --migrate)
      RUN_MIGRATIONS="true"
      shift
      ;;
    --start)
      START_SERVICES="true"
      shift
      ;;
    --no-core-bin)
      INSTALL_CORE_BIN="false"
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

if [[ "${EUID}" -ne 0 ]]; then
  echo "install-control-plane.sh must run as root" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -z "${RELEASE_DIR}" ]]; then
  RELEASE_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd -P)"
else
  RELEASE_DIR="$(cd "${RELEASE_DIR}" && pwd -P)"
fi

if [[ ! -x "${RELEASE_DIR}/bin/bucephalus" ]]; then
  echo "release is missing executable bin/bucephalus: ${RELEASE_DIR}" >&2
  exit 2
fi
if [[ ! -f "${RELEASE_DIR}/bucephalus-cloud/package.json" ]]; then
  echo "release is missing bucephalus-cloud/package.json: ${RELEASE_DIR}" >&2
  exit 2
fi
if [[ ! -f "${RELEASE_DIR}/bucephalus-cloud/deploy/control-plane/bucephalus-cloud-api.service" ]]; then
  echo "release is missing control-plane service files: ${RELEASE_DIR}" >&2
  exit 2
fi

require_command bun
require_command cp
require_command install
require_command systemctl

RELEASE_NAME="$(basename "${RELEASE_DIR}")"
TARGET_DIR="${INSTALL_ROOT}/releases/${RELEASE_NAME}"
CURRENT_LINK="${INSTALL_ROOT}/current"
DATA_DIR="/var/lib/bucephalus-cloud"
CREATED_ENV_FILE="false"

if ! getent group "${SERVICE_GROUP}" >/dev/null; then
  groupadd --system "${SERVICE_GROUP}"
fi

if ! id -u "${SERVICE_USER}" >/dev/null 2>&1; then
  useradd --system --gid "${SERVICE_GROUP}" --home-dir "${DATA_DIR}" --shell /usr/sbin/nologin "${SERVICE_USER}"
fi

install -d -m 0755 "${INSTALL_ROOT}/releases"
if [[ "${RELEASE_DIR}" != "${TARGET_DIR}" ]]; then
  rm -rf "${TARGET_DIR}"
  install -d -m 0755 "${TARGET_DIR}"
  cp -a "${RELEASE_DIR}/." "${TARGET_DIR}/"
else
  echo "release already installed: ${TARGET_DIR}"
fi
ln -sfn "${TARGET_DIR}" "${CURRENT_LINK}"

install -d -m 0750 -o "${SERVICE_USER}" -g "${SERVICE_GROUP}" "${DATA_DIR}"
install -d -m 0750 "${ENV_DIR}"

if [[ ! -f "${ENV_FILE}" ]]; then
  install -m 0640 -o root -g "${SERVICE_GROUP}" \
    "${TARGET_DIR}/bucephalus-cloud/deploy/control-plane/control-plane.env.example" \
    "${ENV_FILE}"
  CREATED_ENV_FILE="true"
  echo "installed env template: ${ENV_FILE}"
  echo "edit ${ENV_FILE} before using --migrate or --start"
fi

chown -R root:root "${TARGET_DIR}"
chown -h root:root "${CURRENT_LINK}"

if [[ "${INSTALL_CORE_BIN}" == "true" ]]; then
  install -m 0755 "${TARGET_DIR}/bin/bucephalus" /usr/local/bin/bucephalus
fi

(
  cd "${TARGET_DIR}/bucephalus-cloud"
  bun install --frozen-lockfile
)

install -m 0644 "${TARGET_DIR}/bucephalus-cloud/deploy/control-plane/bucephalus-cloud-api.service" \
  /etc/systemd/system/bucephalus-cloud-api.service
install -m 0644 "${TARGET_DIR}/bucephalus-cloud/deploy/control-plane/bucephalus-cloud-pool-controller.service" \
  /etc/systemd/system/bucephalus-cloud-pool-controller.service

systemctl daemon-reload

if [[ "${CREATED_ENV_FILE}" == "true" && ("${RUN_MIGRATIONS}" == "true" || "${START_SERVICES}" == "true") ]]; then
  echo "refusing to migrate/start with a freshly installed env template; edit ${ENV_FILE} first" >&2
  exit 2
fi

if [[ "${RUN_MIGRATIONS}" == "true" ]]; then
  set -a
  # shellcheck disable=SC1090
  . "${ENV_FILE}"
  set +a
  (
    cd "${TARGET_DIR}/bucephalus-cloud"
    bun run db:migrate
  )
fi

if [[ "${START_SERVICES}" == "true" ]]; then
  systemctl enable bucephalus-cloud-api.service
  systemctl enable bucephalus-cloud-pool-controller.service
  systemctl restart bucephalus-cloud-api.service
  systemctl restart bucephalus-cloud-pool-controller.service
  systemctl --no-pager --full status bucephalus-cloud-api.service || true
  systemctl --no-pager --full status bucephalus-cloud-pool-controller.service || true
fi

echo "Bucephalus control plane installed"
echo "release=${TARGET_DIR}"
echo "current=${CURRENT_LINK}"
echo "env_file=${ENV_FILE}"
