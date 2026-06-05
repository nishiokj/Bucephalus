#!/usr/bin/env bash
set -euo pipefail

OUT=""
PROJECT_ID=""
REGION=""
DEPLOYMENT_ENVIRONMENT=""
RESOURCE_PREFIX="buc"
OAUTH_ISSUER="https://accounts.google.com"
OAUTH_USER_CLIENT_ID=""
OAUTH_JWKS_URL="https://www.googleapis.com/oauth2/v3/certs"
POOL_CONTROLLER_RUNNER_POOL_ID=""
API_DATABASE_URL_SECRET_VERSION=""
MIGRATOR_DATABASE_URL_SECRET_VERSION=""
WORKER_TOKEN_SECRET_VERSION=""
POOL_CONTROLLER_PROVISION_CMD_JSON_SECRET_VERSION=""
POOL_CONTROLLER_REAP_CMD_JSON_SECRET_VERSION=""
API_INGRESS="INGRESS_TRAFFIC_ALL"
DEPLOY_CONTROL_PLANE_SERVICES="false"
DEPLOY_API_SERVICES="false"
DEPLOY_POOL_CONTROLLER="false"

usage() {
  cat <<'USAGE'
Usage: scripts/deploy/write-gcp-deploy-tfvars.sh --out <path> \
  --project-id <gcp-project> \
  --region <gcp-region> \
  --environment <name> \
  --oauth-user-client-id <client-id.apps.googleusercontent.com> \
  --pool-controller-runner-pool-id <uuid> \
  --api-database-url-secret-version <number> \
  --migrator-database-url-secret-version <number> \
  --worker-token-secret-version <number> \
  --pool-controller-provision-cmd-json-secret-version <number> \
  --pool-controller-reap-cmd-json-secret-version <number> \
  [--resource-prefix <prefix>] \
  [--oauth-issuer <url>] \
  [--oauth-jwks-url <url>] \
  [--api-ingress <Cloud Run ingress enum>] \
  [--deploy-control-plane-services true|false] \
  [--deploy-api-services true|false] \
  [--deploy-pool-controller true|false]

Writes a Terraform tfvars fragment for non-secret GCP deploy inputs. Image
digest variables are intentionally not accepted here; deployment image refs must
come from scripts/release/write-gcp-image-tfvars.sh after pushed image
promotion evidence has been verified.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)
      OUT="${2:-}"
      shift 2
      ;;
    --project-id)
      PROJECT_ID="${2:-}"
      shift 2
      ;;
    --region)
      REGION="${2:-}"
      shift 2
      ;;
    --environment)
      DEPLOYMENT_ENVIRONMENT="${2:-}"
      shift 2
      ;;
    --resource-prefix)
      RESOURCE_PREFIX="${2:-}"
      shift 2
      ;;
    --oauth-issuer)
      OAUTH_ISSUER="${2:-}"
      shift 2
      ;;
    --oauth-user-client-id)
      OAUTH_USER_CLIENT_ID="${2:-}"
      shift 2
      ;;
    --oauth-jwks-url)
      OAUTH_JWKS_URL="${2:-}"
      shift 2
      ;;
    --pool-controller-runner-pool-id)
      POOL_CONTROLLER_RUNNER_POOL_ID="${2:-}"
      shift 2
      ;;
    --api-database-url-secret-version)
      API_DATABASE_URL_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --migrator-database-url-secret-version)
      MIGRATOR_DATABASE_URL_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --worker-token-secret-version)
      WORKER_TOKEN_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --pool-controller-provision-cmd-json-secret-version)
      POOL_CONTROLLER_PROVISION_CMD_JSON_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --pool-controller-reap-cmd-json-secret-version)
      POOL_CONTROLLER_REAP_CMD_JSON_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --api-ingress)
      API_INGRESS="${2:-}"
      shift 2
      ;;
    --deploy-control-plane-services)
      DEPLOY_CONTROL_PLANE_SERVICES="${2:-}"
      shift 2
      ;;
    --deploy-api-services)
      DEPLOY_API_SERVICES="${2:-}"
      shift 2
      ;;
    --deploy-pool-controller)
      DEPLOY_POOL_CONTROLLER="${2:-}"
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

if [[ -z "${OUT}" || -z "${PROJECT_ID}" || -z "${REGION}" || -z "${DEPLOYMENT_ENVIRONMENT}" ]]; then
  usage >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

mkdir -p "$(dirname "${OUT}")"

OUT="${OUT}" \
PROJECT_ID="${PROJECT_ID}" \
REGION="${REGION}" \
DEPLOYMENT_ENVIRONMENT="${DEPLOYMENT_ENVIRONMENT}" \
RESOURCE_PREFIX="${RESOURCE_PREFIX}" \
OAUTH_ISSUER="${OAUTH_ISSUER}" \
OAUTH_USER_CLIENT_ID="${OAUTH_USER_CLIENT_ID}" \
OAUTH_JWKS_URL="${OAUTH_JWKS_URL}" \
POOL_CONTROLLER_RUNNER_POOL_ID="${POOL_CONTROLLER_RUNNER_POOL_ID}" \
API_DATABASE_URL_SECRET_VERSION="${API_DATABASE_URL_SECRET_VERSION}" \
MIGRATOR_DATABASE_URL_SECRET_VERSION="${MIGRATOR_DATABASE_URL_SECRET_VERSION}" \
WORKER_TOKEN_SECRET_VERSION="${WORKER_TOKEN_SECRET_VERSION}" \
POOL_CONTROLLER_PROVISION_CMD_JSON_SECRET_VERSION="${POOL_CONTROLLER_PROVISION_CMD_JSON_SECRET_VERSION}" \
POOL_CONTROLLER_REAP_CMD_JSON_SECRET_VERSION="${POOL_CONTROLLER_REAP_CMD_JSON_SECRET_VERSION}" \
API_INGRESS="${API_INGRESS}" \
DEPLOY_CONTROL_PLANE_SERVICES="${DEPLOY_CONTROL_PLANE_SERVICES}" \
DEPLOY_API_SERVICES="${DEPLOY_API_SERVICES}" \
DEPLOY_POOL_CONTROLLER="${DEPLOY_POOL_CONTROLLER}" \
bun -e '
function optional(value) {
  return value === undefined || value === "" ? null : value;
}

const values = {
  project_id: process.env.PROJECT_ID,
  region: process.env.REGION,
  environment: process.env.DEPLOYMENT_ENVIRONMENT,
  resource_prefix: process.env.RESOURCE_PREFIX,
  oauth_issuer: process.env.OAUTH_ISSUER,
  oauth_user_client_id: optional(process.env.OAUTH_USER_CLIENT_ID),
  oauth_jwks_url: process.env.OAUTH_JWKS_URL,
  pool_controller_runner_pool_id: optional(process.env.POOL_CONTROLLER_RUNNER_POOL_ID),
  api_database_url_secret_version: optional(process.env.API_DATABASE_URL_SECRET_VERSION),
  migrator_database_url_secret_version: optional(process.env.MIGRATOR_DATABASE_URL_SECRET_VERSION),
  worker_token_secret_version: optional(process.env.WORKER_TOKEN_SECRET_VERSION),
  pool_controller_provision_cmd_json_secret_version: optional(process.env.POOL_CONTROLLER_PROVISION_CMD_JSON_SECRET_VERSION),
  pool_controller_reap_cmd_json_secret_version: optional(process.env.POOL_CONTROLLER_REAP_CMD_JSON_SECRET_VERSION),
  api_ingress: process.env.API_INGRESS,
  deploy_control_plane_services: process.env.DEPLOY_CONTROL_PLANE_SERVICES,
  deploy_api_services: process.env.DEPLOY_API_SERVICES,
  deploy_pool_controller: process.env.DEPLOY_POOL_CONTROLLER,
};

const alwaysChecks = [
  ["project_id", /^[a-z][a-z0-9-]{4,28}[a-z0-9]$/],
  ["region", /^[a-z]+-[a-z]+[0-9]$/],
  ["environment", /^[a-z][a-z0-9-]{1,10}[a-z0-9]$/],
  ["resource_prefix", /^[a-z][a-z0-9-]{1,6}[a-z0-9]$/],
  ["oauth_issuer", /^https:\/\/\S+$/],
  ["oauth_jwks_url", /^https:\/\/\S+$/],
];
const serviceChecks = [
  ["oauth_user_client_id", /^[A-Za-z0-9._-]+\.apps\.googleusercontent\.com$/],
  ["api_database_url_secret_version", /^[1-9][0-9]*$/],
  ["migrator_database_url_secret_version", /^[1-9][0-9]*$/],
  ["worker_token_secret_version", /^[1-9][0-9]*$/],
];
const poolControllerChecks = [
  ["pool_controller_runner_pool_id", /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/],
  ["pool_controller_provision_cmd_json_secret_version", /^[1-9][0-9]*$/],
  ["pool_controller_reap_cmd_json_secret_version", /^[1-9][0-9]*$/],
];

function fail(message) {
  console.error(message);
  process.exit(1);
}

for (const name of ["deploy_control_plane_services", "deploy_api_services", "deploy_pool_controller"]) {
  if (!["true", "false"].includes(values[name])) {
    fail(`${name} must be true or false`);
  }
}
const deployServices = values.deploy_control_plane_services === "true";
const deployApi = deployServices || values.deploy_api_services === "true" || values.deploy_pool_controller === "true";
const deployPoolController = deployServices || values.deploy_pool_controller === "true";
const checks = [
  ...alwaysChecks,
  ...(deployApi ? serviceChecks : []),
  ...(deployPoolController ? poolControllerChecks : []),
];

for (const [name, pattern] of checks) {
  if (typeof values[name] !== "string" || !pattern.test(values[name])) {
    fail(`${name} is invalid for deploy tfvars`);
  }
}

for (const [name, value] of Object.entries(values)) {
  if (value === null) {
    continue;
  }
  if (/replace-with|postgres:\/\/|BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|ghp_|github_pat_|ya29\.|tailscale/i.test(values[name])) {
    fail(`${name} looks like a placeholder or secret material`);
  }
}

if (deployPoolController && values.pool_controller_runner_pool_id === "00000000-0000-0000-0000-000000000000") {
  fail("pool_controller_runner_pool_id must be an API-created runner pool UUID, not an all-zero placeholder");
}
if (![
  "INGRESS_TRAFFIC_ALL",
  "INGRESS_TRAFFIC_INTERNAL_ONLY",
  "INGRESS_TRAFFIC_INTERNAL_LOAD_BALANCER",
].includes(values.api_ingress)) {
  fail("api_ingress is not a supported Cloud Run ingress enum");
}

const order = [
  "project_id",
  "region",
  "environment",
  "resource_prefix",
  "oauth_issuer",
  "oauth_user_client_id",
  "oauth_jwks_url",
  "pool_controller_runner_pool_id",
  "api_database_url_secret_version",
  "migrator_database_url_secret_version",
  "worker_token_secret_version",
  "pool_controller_provision_cmd_json_secret_version",
  "pool_controller_reap_cmd_json_secret_version",
  "api_ingress",
  "deploy_control_plane_services",
  "deploy_api_services",
  "deploy_pool_controller",
];
const lines = [
  "# Generated by scripts/deploy/write-gcp-deploy-tfvars.sh",
  "# Contains non-secret deploy configuration only.",
  "# Image digest variables must come from verified gcp-image-digests.tfvars.",
  "",
  ...order.map((name) => {
    if (["deploy_control_plane_services", "deploy_api_services", "deploy_pool_controller"].includes(name)) {
      return `${name} = ${values[name]}`;
    }
    if (values[name] === null) {
      return `${name} = null`;
    }
    return `${name} = ${JSON.stringify(values[name])}`;
  }),
  "",
];
await Bun.write(process.env.OUT, lines.join("\n"));
console.log(`tfvars=${process.env.OUT}`);
'
