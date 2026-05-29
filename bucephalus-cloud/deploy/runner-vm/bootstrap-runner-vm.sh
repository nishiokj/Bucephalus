#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="bucephalus-runner"
SERVICE_USER="${BUCEPHALUS_RUNNER_USER:-bucephalus-runner}"
SERVICE_GROUP="${BUCEPHALUS_RUNNER_GROUP:-bucephalus-runner}"
WORKER_DIR="${BUCEPHALUS_CLOUD_WORKER_DIR:-/opt/bucephalus-cloud}"
DATA_DIR="${BUCEPHALUS_CLOUD_DATA_DIR:-/var/lib/bucephalus-runner}"
SECRET_DIR="${BUCEPHALUS_WORKER_SECRET_DIR:-/etc/bucephalus-runner/secrets}"
ENV_DIR="/etc/bucephalus-runner"
ENV_FILE="${ENV_DIR}/runner.env"
UNIT_FILE="/etc/systemd/system/${SERVICE_NAME}.service"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "missing required environment variable: ${name}" >&2
    exit 2
  fi
}

require_command() {
  local command_name="$1"
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "required command not found: ${command_name}" >&2
    exit 2
  fi
}

contains_csv() {
  local csv="$1"
  local needle="$2"
  [[ ",${csv}," == *",${needle},"* ]]
}

if [[ "${EUID}" -ne 0 ]]; then
  echo "bootstrap-runner-vm.sh must run as root because it writes systemd and /etc files" >&2
  exit 2
fi

require_env BUCEPHALUS_CLOUD_API_URL
require_env DATABASE_URL
require_env BUCEPHALUS_CLOUD_WORKER_TOKEN
require_env BUCEPHALUS_RUNNER_POOL_ID

BUCEPHALUS_WORKER_ID="${BUCEPHALUS_WORKER_ID:-$(hostname -s)}"
BUCEPHALUS_WORKER_EXECUTORS="${BUCEPHALUS_WORKER_EXECUTORS:-runner-docker}"
BUCEPHALUS_WORKER_RESOURCES="${BUCEPHALUS_WORKER_RESOURCES:-core_runner,docker_daemon,registry_pull}"
BUCEPHALUS_CORE_RUNNER_CMD="${BUCEPHALUS_CORE_RUNNER_CMD:-/usr/local/bin/bucephalus}"

require_command bun
require_command systemctl

if [[ ! -d "${WORKER_DIR}" ]]; then
  echo "worker directory does not exist: ${WORKER_DIR}" >&2
  exit 2
fi

if [[ ! -f "${WORKER_DIR}/src/worker.ts" ]]; then
  echo "worker source is missing: ${WORKER_DIR}/src/worker.ts" >&2
  exit 2
fi

if [[ ! -x "${BUCEPHALUS_CORE_RUNNER_CMD}" ]]; then
  echo "Core runner command is not executable: ${BUCEPHALUS_CORE_RUNNER_CMD}" >&2
  exit 2
fi

if contains_csv "${BUCEPHALUS_WORKER_RESOURCES}" "docker_daemon"; then
  require_command docker
  if [[ ! -S /var/run/docker.sock ]]; then
    echo "docker_daemon capability requested, but /var/run/docker.sock is not present" >&2
    exit 2
  fi
fi

if ! getent group "${SERVICE_GROUP}" >/dev/null; then
  groupadd --system "${SERVICE_GROUP}"
fi

if ! id -u "${SERVICE_USER}" >/dev/null 2>&1; then
  useradd --system --gid "${SERVICE_GROUP}" --home-dir "${DATA_DIR}" --shell /usr/sbin/nologin "${SERVICE_USER}"
fi

if contains_csv "${BUCEPHALUS_WORKER_RESOURCES}" "docker_daemon" && getent group docker >/dev/null; then
  usermod -aG docker "${SERVICE_USER}"
fi

if ! runuser -u "${SERVICE_USER}" -- env PATH="/usr/local/bin:/usr/bin:/bin" bun --version >/dev/null; then
  echo "service user ${SERVICE_USER} cannot execute bun; install bun somewhere outside a private home directory, such as /usr/local/bin/bun" >&2
  exit 2
fi

install -d -m 0750 -o "${SERVICE_USER}" -g "${SERVICE_GROUP}" "${DATA_DIR}"
install -d -m 0750 -o "${SERVICE_USER}" -g "${SERVICE_GROUP}" "${SECRET_DIR}"
install -d -m 0750 "${ENV_DIR}"

cat > "${ENV_FILE}" <<EOF
BUCEPHALUS_CLOUD_API_URL=${BUCEPHALUS_CLOUD_API_URL}
DATABASE_URL=${DATABASE_URL}
BUCEPHALUS_CLOUD_WORKER_TOKEN=${BUCEPHALUS_CLOUD_WORKER_TOKEN}
BUCEPHALUS_RUNNER_POOL_ID=${BUCEPHALUS_RUNNER_POOL_ID}
BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID=${BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID:-}
BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID=${BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID:-}
BUCEPHALUS_WORKER_ID=${BUCEPHALUS_WORKER_ID}
BUCEPHALUS_WORKER_EXECUTORS=${BUCEPHALUS_WORKER_EXECUTORS}
BUCEPHALUS_WORKER_RESOURCES=${BUCEPHALUS_WORKER_RESOURCES}
BUCEPHALUS_CORE_RUNNER_CMD=${BUCEPHALUS_CORE_RUNNER_CMD}
BUCEPHALUS_CLOUD_DATA_DIR=${DATA_DIR}
BUCEPHALUS_WORKER_SECRET_DIR=${SECRET_DIR}
BUCEPHALUS_WORKER_LEASE_SECONDS=${BUCEPHALUS_WORKER_LEASE_SECONDS:-30}
BUCEPHALUS_WORKER_POLL_MS=${BUCEPHALUS_WORKER_POLL_MS:-2000}
BUCEPHALUS_WORKER_SWEEPER_MS=${BUCEPHALUS_WORKER_SWEEPER_MS:-5000}
BUCEPHALUS_WORKER_MIN_FREE_BYTES=${BUCEPHALUS_WORKER_MIN_FREE_BYTES:-${BUCEPHALUS_MIN_FREE_BYTES:-21474836480}}
BUCEPHALUS_WORKER_RETAIN_ATTEMPT_WORKSPACES=${BUCEPHALUS_WORKER_RETAIN_ATTEMPT_WORKSPACES:-false}
EOF

chown root:"${SERVICE_GROUP}" "${ENV_FILE}"
chmod 0640 "${ENV_FILE}"

if [[ -f "${WORKER_DIR}/bun.lock" ]]; then
  (cd "${WORKER_DIR}" && bun install --frozen-lockfile)
fi

install -m 0644 "${SCRIPT_DIR}/bucephalus-runner.service" "${UNIT_FILE}"
sed -i "s#WorkingDirectory=/opt/bucephalus-cloud#WorkingDirectory=${WORKER_DIR}#g" "${UNIT_FILE}"
sed -i "s#User=bucephalus-runner#User=${SERVICE_USER}#g" "${UNIT_FILE}"
sed -i "s#Group=bucephalus-runner#Group=${SERVICE_GROUP}#g" "${UNIT_FILE}"
sed -i "s#ReadWritePaths=/var/lib/bucephalus-runner /etc/bucephalus-runner#ReadWritePaths=${DATA_DIR} ${ENV_DIR}#g" "${UNIT_FILE}"

if contains_csv "${BUCEPHALUS_WORKER_RESOURCES}" "docker_daemon" && getent group docker >/dev/null; then
  sed -i "/^Group=/a SupplementaryGroups=docker" "${UNIT_FILE}"
fi

systemctl daemon-reload
systemctl enable "${SERVICE_NAME}.service"
systemctl restart "${SERVICE_NAME}.service"
systemctl --no-pager --full status "${SERVICE_NAME}.service" || true

echo "Bucephalus runner VM bootstrap complete"
echo "service=${SERVICE_NAME}.service"
echo "worker_id=${BUCEPHALUS_WORKER_ID}"
echo "runner_pool_id=${BUCEPHALUS_RUNNER_POOL_ID}"
