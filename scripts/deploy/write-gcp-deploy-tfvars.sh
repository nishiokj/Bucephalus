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
CLOUD_OBJECT_STORAGE_BACKEND="${BUCEPHALUS_CLOUD_OBJECT_STORAGE_BACKEND:-gcs}"
CLOUD_GCS_BUCKET="${BUCEPHALUS_CLOUD_GCS_BUCKET:-}"
CLOUD_GCS_PREFIX="${BUCEPHALUS_CLOUD_GCS_PREFIX:-}"
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
MODAL_BACKEND_ENABLED="${BUCEPHALUS_MODAL_BACKEND_ENABLED:-false}"
MODAL_APP_NAME="${BUCEPHALUS_MODAL_APP_NAME:-}"
MODAL_ENVIRONMENT="${BUCEPHALUS_MODAL_ENVIRONMENT:-}"
MODAL_TOKEN_ID_SECRET_VERSION="${BUCEPHALUS_MODAL_TOKEN_ID_SECRET_VERSION:-}"
MODAL_TOKEN_SECRET_SECRET_VERSION="${BUCEPHALUS_MODAL_TOKEN_SECRET_SECRET_VERSION:-}"
MODAL_S3_BUCKET="${BUCEPHALUS_MODAL_S3_BUCKET:-}"
MODAL_S3_PREFIX="${BUCEPHALUS_MODAL_S3_PREFIX:-}"
MODAL_S3_ENDPOINT_URL="${BUCEPHALUS_MODAL_S3_ENDPOINT_URL:-}"
MODAL_S3_REGION="${BUCEPHALUS_MODAL_S3_REGION:-}"
MODAL_S3_SECRET_NAME="${BUCEPHALUS_MODAL_S3_SECRET_NAME:-}"
MODAL_S3_ACCESS_KEY_ID_SECRET_VERSION="${BUCEPHALUS_MODAL_S3_ACCESS_KEY_ID_SECRET_VERSION:-}"
MODAL_S3_SECRET_ACCESS_KEY_SECRET_VERSION="${BUCEPHALUS_MODAL_S3_SECRET_ACCESS_KEY_SECRET_VERSION:-}"
MODAL_S3_FORCE_PATH_STYLE="${BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE:-false}"
MODAL_GCP_ARTIFACT_REGISTRY_SECRET_NAME="${BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SECRET_NAME:-}"
MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION="${BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION:-}"

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
  [--cloud-object-storage-backend filesystem|gcs|r2] \
  [--cloud-gcs-bucket <bucket>] \
  [--cloud-gcs-prefix <prefix>] \
  [--api-ingress <Cloud Run ingress enum>] \
  [--deploy-control-plane-services true|false] \
  [--deploy-api-services true|false] \
  [--deploy-pool-controller true|false] \
  [--modal-backend-enabled true|false] \
  [--modal-app-name <name>] \
  [--modal-environment <name>] \
  [--modal-token-id-secret-version <number>] \
  [--modal-token-secret-secret-version <number>] \
  [--modal-s3-bucket <bucket>] \
  [--modal-s3-prefix <prefix>] \
  [--modal-s3-endpoint-url <https-url>] \
  [--modal-s3-region <region>] \
  [--modal-s3-secret-name <modal-secret>] \
  [--modal-s3-access-key-id-secret-version <number>] \
  [--modal-s3-secret-access-key-secret-version <number>] \
  [--modal-s3-force-path-style true|false] \
  [--modal-gcp-artifact-registry-secret-name <modal-secret>] \
  [--modal-gcp-artifact-registry-service-account-json-secret-version <number>]

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
    --cloud-object-storage-backend)
      CLOUD_OBJECT_STORAGE_BACKEND="${2:-}"
      shift 2
      ;;
    --cloud-gcs-bucket)
      CLOUD_GCS_BUCKET="${2:-}"
      shift 2
      ;;
    --cloud-gcs-prefix)
      CLOUD_GCS_PREFIX="${2:-}"
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
    --modal-backend-enabled)
      MODAL_BACKEND_ENABLED="${2:-}"
      shift 2
      ;;
    --modal-app-name)
      MODAL_APP_NAME="${2:-}"
      shift 2
      ;;
    --modal-environment)
      MODAL_ENVIRONMENT="${2:-}"
      shift 2
      ;;
    --modal-token-id-secret-version)
      MODAL_TOKEN_ID_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --modal-token-secret-secret-version)
      MODAL_TOKEN_SECRET_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --modal-s3-bucket)
      MODAL_S3_BUCKET="${2:-}"
      shift 2
      ;;
    --modal-s3-prefix)
      MODAL_S3_PREFIX="${2:-}"
      shift 2
      ;;
    --modal-s3-endpoint-url)
      MODAL_S3_ENDPOINT_URL="${2:-}"
      shift 2
      ;;
    --modal-s3-region)
      MODAL_S3_REGION="${2:-}"
      shift 2
      ;;
    --modal-s3-secret-name)
      MODAL_S3_SECRET_NAME="${2:-}"
      shift 2
      ;;
    --modal-s3-access-key-id-secret-version)
      MODAL_S3_ACCESS_KEY_ID_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --modal-s3-secret-access-key-secret-version)
      MODAL_S3_SECRET_ACCESS_KEY_SECRET_VERSION="${2:-}"
      shift 2
      ;;
    --modal-s3-force-path-style)
      MODAL_S3_FORCE_PATH_STYLE="${2:-}"
      shift 2
      ;;
    --modal-gcp-artifact-registry-secret-name)
      MODAL_GCP_ARTIFACT_REGISTRY_SECRET_NAME="${2:-}"
      shift 2
      ;;
    --modal-gcp-artifact-registry-service-account-json-secret-version)
      MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION="${2:-}"
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
CLOUD_OBJECT_STORAGE_BACKEND="${CLOUD_OBJECT_STORAGE_BACKEND}" \
CLOUD_GCS_BUCKET="${CLOUD_GCS_BUCKET}" \
CLOUD_GCS_PREFIX="${CLOUD_GCS_PREFIX}" \
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
MODAL_BACKEND_ENABLED="${MODAL_BACKEND_ENABLED}" \
MODAL_APP_NAME="${MODAL_APP_NAME}" \
MODAL_ENVIRONMENT="${MODAL_ENVIRONMENT}" \
MODAL_TOKEN_ID_SECRET_VERSION="${MODAL_TOKEN_ID_SECRET_VERSION}" \
MODAL_TOKEN_SECRET_SECRET_VERSION="${MODAL_TOKEN_SECRET_SECRET_VERSION}" \
MODAL_S3_BUCKET="${MODAL_S3_BUCKET}" \
MODAL_S3_PREFIX="${MODAL_S3_PREFIX}" \
MODAL_S3_ENDPOINT_URL="${MODAL_S3_ENDPOINT_URL}" \
MODAL_S3_REGION="${MODAL_S3_REGION}" \
MODAL_S3_SECRET_NAME="${MODAL_S3_SECRET_NAME}" \
MODAL_S3_ACCESS_KEY_ID_SECRET_VERSION="${MODAL_S3_ACCESS_KEY_ID_SECRET_VERSION}" \
MODAL_S3_SECRET_ACCESS_KEY_SECRET_VERSION="${MODAL_S3_SECRET_ACCESS_KEY_SECRET_VERSION}" \
MODAL_S3_FORCE_PATH_STYLE="${MODAL_S3_FORCE_PATH_STYLE}" \
MODAL_GCP_ARTIFACT_REGISTRY_SECRET_NAME="${MODAL_GCP_ARTIFACT_REGISTRY_SECRET_NAME}" \
MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION="${MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION}" \
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
  cloud_object_storage_backend: process.env.CLOUD_OBJECT_STORAGE_BACKEND,
  cloud_gcs_bucket: optional(process.env.CLOUD_GCS_BUCKET),
  cloud_gcs_prefix: process.env.CLOUD_GCS_PREFIX ?? "",
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
  modal_backend_enabled: process.env.MODAL_BACKEND_ENABLED,
  modal_app_name: optional(process.env.MODAL_APP_NAME),
  modal_environment: optional(process.env.MODAL_ENVIRONMENT),
  modal_token_id_secret_version: optional(process.env.MODAL_TOKEN_ID_SECRET_VERSION),
  modal_token_secret_secret_version: optional(process.env.MODAL_TOKEN_SECRET_SECRET_VERSION),
  modal_s3_bucket: optional(process.env.MODAL_S3_BUCKET),
  modal_s3_prefix: process.env.MODAL_S3_PREFIX ?? "",
  modal_s3_endpoint_url: optional(process.env.MODAL_S3_ENDPOINT_URL),
  modal_s3_region: optional(process.env.MODAL_S3_REGION),
  modal_s3_secret_name: optional(process.env.MODAL_S3_SECRET_NAME),
  modal_s3_access_key_id_secret_version: optional(process.env.MODAL_S3_ACCESS_KEY_ID_SECRET_VERSION),
  modal_s3_secret_access_key_secret_version: optional(process.env.MODAL_S3_SECRET_ACCESS_KEY_SECRET_VERSION),
  modal_s3_force_path_style: process.env.MODAL_S3_FORCE_PATH_STYLE,
  modal_gcp_artifact_registry_secret_name: optional(process.env.MODAL_GCP_ARTIFACT_REGISTRY_SECRET_NAME),
  modal_gcp_artifact_registry_service_account_json_secret_version: optional(process.env.MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION),
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
  ["oauth_user_client_id", /^[A-Za-z0-9._-]+\.apps\.googleusercontent\.com(?:\s*,\s*[A-Za-z0-9._-]+\.apps\.googleusercontent\.com)*$/],
  ["cloud_object_storage_backend", /^(filesystem|r2|gcs)$/],
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
for (const name of ["modal_backend_enabled", "modal_s3_force_path_style"]) {
  if (!["true", "false"].includes(values[name])) {
    fail(`${name} must be true or false`);
  }
}
const deployServices = values.deploy_control_plane_services === "true";
const deployApi = deployServices || values.deploy_api_services === "true" || values.deploy_pool_controller === "true";
const deployPoolController = deployServices || values.deploy_pool_controller === "true";
const modalEnabled = values.modal_backend_enabled === "true";
const modalUsesGcsServiceAccountSync =
  typeof values.modal_s3_endpoint_url === "string" &&
  values.modal_s3_endpoint_url.includes("storage.googleapis.com") &&
  typeof values.modal_gcp_artifact_registry_service_account_json_secret_version === "string" &&
  /^[1-9][0-9]*$/.test(values.modal_gcp_artifact_registry_service_account_json_secret_version);
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
if (values.cloud_gcs_bucket !== null && !/^[a-z0-9][a-z0-9._-]{1,61}[a-z0-9]$/.test(values.cloud_gcs_bucket)) {
  fail("cloud_gcs_bucket is invalid for deploy tfvars");
}
if (deployPoolController && modalEnabled) {
  const modalChecks = [
    ["modal_app_name", /^\S+$/],
    ["modal_token_id_secret_version", /^[1-9][0-9]*$/],
    ["modal_token_secret_secret_version", /^[1-9][0-9]*$/],
    ["modal_s3_bucket", /^[A-Za-z0-9][A-Za-z0-9._-]{1,61}[A-Za-z0-9]$/],
    ["modal_s3_prefix", /^.+$/],
  ];
  for (const [name, pattern] of modalChecks) {
    if (typeof values[name] !== "string" || !pattern.test(values[name])) {
      fail(`${name} is invalid for deploy tfvars`);
    }
  }
  if (values.modal_s3_secret_name === null && !modalUsesGcsServiceAccountSync) {
    for (const name of ["modal_s3_access_key_id_secret_version", "modal_s3_secret_access_key_secret_version"]) {
      if (typeof values[name] !== "string" || !/^[1-9][0-9]*$/.test(values[name])) {
        fail(`${name} is required when modal_s3_secret_name is unset and the sync bucket is not using the GCS service-account path`);
      }
    }
  }
  if (values.modal_s3_endpoint_url !== null && !/^https:\/\/\S+$/.test(values.modal_s3_endpoint_url)) {
    fail("modal_s3_endpoint_url is invalid for deploy tfvars");
  }
  if (values.modal_gcp_artifact_registry_service_account_json_secret_version !== null && !/^[1-9][0-9]*$/.test(values.modal_gcp_artifact_registry_service_account_json_secret_version)) {
    fail("modal_gcp_artifact_registry_service_account_json_secret_version is invalid for deploy tfvars");
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
  "cloud_object_storage_backend",
  "cloud_gcs_bucket",
  "cloud_gcs_prefix",
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
  "modal_backend_enabled",
  "modal_app_name",
  "modal_environment",
  "modal_token_id_secret_version",
  "modal_token_secret_secret_version",
  "modal_s3_bucket",
  "modal_s3_prefix",
  "modal_s3_endpoint_url",
  "modal_s3_region",
  "modal_s3_secret_name",
  "modal_s3_access_key_id_secret_version",
  "modal_s3_secret_access_key_secret_version",
  "modal_s3_force_path_style",
  "modal_gcp_artifact_registry_secret_name",
  "modal_gcp_artifact_registry_service_account_json_secret_version",
];
const lines = [
  "# Generated by scripts/deploy/write-gcp-deploy-tfvars.sh",
  "# Contains non-secret deploy configuration only.",
  "# Image digest variables must come from verified gcp-image-digests.tfvars.",
  "",
  ...order.map((name) => {
    if (["deploy_control_plane_services", "deploy_api_services", "deploy_pool_controller", "modal_backend_enabled", "modal_s3_force_path_style"].includes(name)) {
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
