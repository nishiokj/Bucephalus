#!/usr/bin/env bash
set -euo pipefail

PROJECT_ID=""
PROJECT_NUMBER=""
REGION="us-central1"
REPOSITORY="nishiokj/Bucephalus"
GITHUB_ENVIRONMENT="bucephalus"
ENVIRONMENT="bucephalus"
RESOURCE_PREFIX="buc"
STATE_BUCKET=""
STATE_PREFIX=""
POOL_ID="bucephalus-github"
PROVIDER_ID="github"
PUBLISHER_ACCOUNT_ID=""
DEPLOY_ACCOUNT_ID=""
IMAGE_REPOSITORY=""
GOOGLE_OAUTH_CLIENT_ID=""
CLOUDFLARE_WORKER_NAME="bucephalus-cloud-ui"
BUN_BASE_IMAGE="oven/bun@sha256:e10577f0db68676a7024391c6e5cb4b879ebd17188ab750cf10024a6d700e5c4"
RELEASE_RUN_ID=""
PROMOTION_ARTIFACT_NAME=""
REQUIRE_SUBSTRATE="false"
REQUIRE_PUSHED_IMAGES="false"
REQUIRE_PROMOTION_ARTIFACT="false"
REQUIRE_DEPLOY_SECRETS="false"
REQUIRE_API_STAGE="false"
REQUIRE_POOL_STAGE="false"
REQUIRE_CLOUDFLARE_UI_STAGE="false"

usage() {
  cat <<'USAGE'
Usage: scripts/deploy/verify-gcp-cicd-readiness.sh --project-id <project> [options]

Read-only readiness verifier for the Bucephalus GCP/GitHub CI/CD boundary.
It checks the resources and secrets created by bootstrap-gcp-github-oidc.sh and
can optionally require the Terraform substrate, pushed image digests, promotion
evidence artifact, and deploy smoke secrets.

Options:
  --project-id <id>              GCP project id. Required.
  --project-number <number>      GCP project number. Auto-discovered when omitted.
  --region <region>              GCP region. Default: us-central1.
  --repository <owner/repo>      GitHub repository. Default: nishiokj/Bucephalus.
  --github-environment <name>    GitHub environment for deploy smoke secrets. Default: bucephalus.
  --environment <name>           Deployment environment label. Default: bucephalus.
  --resource-prefix <prefix>     Resource prefix. Default: buc.
  --state-bucket <bucket>        Terraform state bucket. Default: <project>-bucephalus-tfstate.
  --state-prefix <prefix>        Terraform state prefix. Default: <environment>/gcp.
  --pool-id <id>                 Workload Identity pool id. Default: bucephalus-github.
  --provider-id <id>             Workload Identity provider id. Default: github.
  --publisher-account-id <id>    Publisher service account id. Default: <prefix>-<env>-gh-publish.
  --deploy-account-id <id>       Deploy service account id. Default: <prefix>-<env>-gh-deploy.
  --image-repository <prefix>    Image repository prefix. Default: <region>-docker.pkg.dev/<project>/<prefix>-<env>-cloud/bucephalus-cloud.
  --google-oauth-client-id <id>  Expected Google OAuth client ID for API/UI deploys. Required.
  --cloudflare-worker-name <n>   Expected Cloudflare Worker name for UI deploys.
  --bun-base-image <image>       Expected digest-pinned Bun base image for release image builds.
  --release-run-id <id>          Release workflow run that should contain promotion evidence.
  --promotion-artifact-name <n>  Promotion artifact name. Default: any cloud-release-promotion-<version> artifact.
  --require-substrate            Require the Terraform-created Artifact Registry repository.
  --require-pushed-images        Require pushed API/pool/migration/worker image digests.
  --require-promotion-artifact   Require the promotion evidence artifact in --release-run-id.
  --require-deploy-secrets       Require deploy smoke token secrets in --github-environment.
  --require-api-stage            Require API deploy/cleanup stage runtime config.
  --require-pool-stage           Require pool-controller deploy stage runtime config.
  --require-cloudflare-ui-stage  Require Cloudflare UI deploy config and credentials.
  --require-all-deploy-stages    Require API, pool-controller, and Cloudflare UI stage config.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project-id)
      PROJECT_ID="${2:-}"
      shift 2
      ;;
    --project-number)
      PROJECT_NUMBER="${2:-}"
      shift 2
      ;;
    --region)
      REGION="${2:-}"
      shift 2
      ;;
    --repository)
      REPOSITORY="${2:-}"
      shift 2
      ;;
    --github-environment)
      GITHUB_ENVIRONMENT="${2:-}"
      shift 2
      ;;
    --environment)
      ENVIRONMENT="${2:-}"
      shift 2
      ;;
    --resource-prefix)
      RESOURCE_PREFIX="${2:-}"
      shift 2
      ;;
    --state-bucket)
      STATE_BUCKET="${2:-}"
      shift 2
      ;;
    --state-prefix)
      STATE_PREFIX="${2:-}"
      shift 2
      ;;
    --pool-id)
      POOL_ID="${2:-}"
      shift 2
      ;;
    --provider-id)
      PROVIDER_ID="${2:-}"
      shift 2
      ;;
    --publisher-account-id)
      PUBLISHER_ACCOUNT_ID="${2:-}"
      shift 2
      ;;
    --deploy-account-id)
      DEPLOY_ACCOUNT_ID="${2:-}"
      shift 2
      ;;
    --image-repository)
      IMAGE_REPOSITORY="${2:-}"
      shift 2
      ;;
    --google-oauth-client-id)
      GOOGLE_OAUTH_CLIENT_ID="${2:-}"
      shift 2
      ;;
    --cloudflare-worker-name)
      CLOUDFLARE_WORKER_NAME="${2:-}"
      shift 2
      ;;
    --bun-base-image)
      BUN_BASE_IMAGE="${2:-}"
      shift 2
      ;;
    --release-run-id)
      RELEASE_RUN_ID="${2:-}"
      shift 2
      ;;
    --promotion-artifact-name)
      PROMOTION_ARTIFACT_NAME="${2:-}"
      shift 2
      ;;
    --require-substrate)
      REQUIRE_SUBSTRATE="true"
      shift
      ;;
    --require-pushed-images)
      REQUIRE_PUSHED_IMAGES="true"
      shift
      ;;
    --require-promotion-artifact)
      REQUIRE_PROMOTION_ARTIFACT="true"
      shift
      ;;
    --require-deploy-secrets)
      REQUIRE_DEPLOY_SECRETS="true"
      shift
      ;;
    --require-api-stage)
      REQUIRE_API_STAGE="true"
      shift
      ;;
    --require-pool-stage)
      REQUIRE_POOL_STAGE="true"
      shift
      ;;
    --require-cloudflare-ui-stage)
      REQUIRE_CLOUDFLARE_UI_STAGE="true"
      shift
      ;;
    --require-all-deploy-stages)
      REQUIRE_API_STAGE="true"
      REQUIRE_POOL_STAGE="true"
      REQUIRE_CLOUDFLARE_UI_STAGE="true"
      REQUIRE_DEPLOY_SECRETS="true"
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

if [[ -z "${PROJECT_ID}" ]]; then
  usage >&2
  exit 2
fi
if [[ ! "${PROJECT_ID}" =~ ^[a-z][a-z0-9-]{4,28}[a-z0-9]$ ]]; then
  echo "--project-id must be a valid GCP project id" >&2
  exit 2
fi
if [[ ! "${REGION}" =~ ^[a-z]+-[a-z]+[0-9]$ ]]; then
  echo "--region must be a GCP region such as us-central1" >&2
  exit 2
fi
if [[ ! "${REPOSITORY}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "--repository must be owner/repo" >&2
  exit 2
fi
if [[ ! "${ENVIRONMENT}" =~ ^[a-z][a-z0-9-]{1,10}[a-z0-9]$ ]]; then
  echo "--environment must match Terraform environment validation" >&2
  exit 2
fi
if [[ ! "${RESOURCE_PREFIX}" =~ ^[a-z][a-z0-9-]{1,6}[a-z0-9]$ ]]; then
  echo "--resource-prefix must match Terraform resource_prefix validation" >&2
  exit 2
fi
if [[ -z "${GOOGLE_OAUTH_CLIENT_ID}" || ! "${GOOGLE_OAUTH_CLIENT_ID}" =~ ^[A-Za-z0-9._-]+\.apps\.googleusercontent\.com$ ]]; then
  echo "--google-oauth-client-id must be a Google OAuth web client ID" >&2
  exit 2
fi
if [[ ! "${CLOUDFLARE_WORKER_NAME}" =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]]; then
  echo "--cloudflare-worker-name must be a Cloudflare-compatible lowercase name" >&2
  exit 2
fi
if [[ -z "${BUN_BASE_IMAGE}" || ! "${BUN_BASE_IMAGE}" =~ @sha256:[a-f0-9]{64}$ ]]; then
  echo "--bun-base-image must be digest-addressed" >&2
  exit 2
fi

STATE_BUCKET="${STATE_BUCKET:-${PROJECT_ID}-bucephalus-tfstate}"
STATE_PREFIX="${STATE_PREFIX:-${ENVIRONMENT}/gcp}"
PUBLISHER_ACCOUNT_ID="${PUBLISHER_ACCOUNT_ID:-${RESOURCE_PREFIX}-${ENVIRONMENT}-gh-publish}"
DEPLOY_ACCOUNT_ID="${DEPLOY_ACCOUNT_ID:-${RESOURCE_PREFIX}-${ENVIRONMENT}-gh-deploy}"
ARTIFACT_REPOSITORY_ID="${RESOURCE_PREFIX}-${ENVIRONMENT}-cloud"
IMAGE_REPOSITORY="${IMAGE_REPOSITORY:-${REGION}-docker.pkg.dev/${PROJECT_ID}/${ARTIFACT_REPOSITORY_ID}/bucephalus-cloud}"

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

require_command gcloud
require_command gh
require_command bun

failures=()

pass() {
  echo "ok - $1"
}

missing() {
  echo "missing - $1"
  failures+=("$1")
}

contains_line() {
  local needle="$1"
  local haystack="$2"
  grep -Fxq "${needle}" <<< "${haystack}"
}

truthy() {
  [[ "${1:-}" == "True" || "${1:-}" == "true" || "${1:-}" == "TRUE" ]]
}

json_variable_value() {
  local json="$1"
  local name="$2"
  JSON_DATA="${json}" NAME="${name}" bun -e '
    const variables = JSON.parse(process.env.JSON_DATA || "[]");
    const entry = variables.find((item) => item.name === process.env.NAME);
    process.stdout.write(entry?.value ?? "");
  '
}

assert_variable_equals() {
  local label="$1"
  local json="$2"
  local name="$3"
  local expected="$4"
  local actual
  actual="$(json_variable_value "${json}" "${name}")"
  if [[ "${actual}" == "${expected}" ]]; then
    pass "${label} variable ${name} matches declared value"
  elif [[ -z "${actual}" ]]; then
    missing "${label} variable ${name} is configured"
  else
    missing "${label} variable ${name} matches declared value (expected '${expected}', got '${actual}')"
  fi
}

assert_variable_present() {
  local label="$1"
  local json="$2"
  local name="$3"
  local actual
  actual="$(json_variable_value "${json}" "${name}")"
  if [[ -n "${actual}" ]]; then
    pass "${label} variable ${name} is configured"
  else
    missing "${label} variable ${name} is configured"
  fi
}

assert_variable_or_secret_present() {
  local label="$1"
  local json="$2"
  local secrets="$3"
  local name="$4"
  local actual
  actual="$(json_variable_value "${json}" "${name}")"
  if [[ -n "${actual}" ]]; then
    pass "${label} variable ${name} is configured"
  elif contains_line "${name}" "${secrets}"; then
    pass "${label} secret ${name} is configured"
  else
    missing "${label} variable or secret ${name} is configured"
  fi
}

assert_numeric_variable_or_enabled_secret_version() {
  local json="$1"
  local name="$2"
  local secret_id="$3"
  local current version
  current="$(json_variable_value "${json}" "${name}")"
  if [[ "${current}" =~ ^[1-9][0-9]*$ ]]; then
    pass "GitHub environment variable ${name} pins Secret Manager version ${current}"
    return
  fi
  version="$(gcloud secrets versions list "${secret_id}" \
    --project "${PROJECT_ID}" \
    --filter "state=enabled" \
    --sort-by "~createTime" \
    --limit 1 \
    --format "value(name)" 2>/dev/null || true)"
  if [[ "${version}" =~ ^[1-9][0-9]*$ ]]; then
    pass "Secret Manager has enabled version for ${secret_id}; ${name} can be auto-resolved"
  elif [[ -n "${current}" ]]; then
    missing "GitHub environment variable ${name} is numeric or ${secret_id} has an enabled Secret Manager version"
  else
    missing "GitHub environment variable ${name} is configured or ${secret_id} has an enabled Secret Manager version"
  fi
}

assert_optional_numeric_variable_or_enabled_secret_version() {
  local json="$1"
  local name="$2"
  local secret_id="$3"
  local current version
  current="$(json_variable_value "${json}" "${name}")"
  if [[ "${current}" =~ ^[1-9][0-9]*$ ]]; then
    pass "optional GitHub environment variable ${name} pins Secret Manager version ${current}"
    return
  fi
  if [[ -n "${current}" ]]; then
    missing "optional GitHub environment variable ${name} is numeric when set"
    return
  fi
  version="$(gcloud secrets versions list "${secret_id}" \
    --project "${PROJECT_ID}" \
    --filter "state=enabled" \
    --sort-by "~createTime" \
    --limit 1 \
    --format "value(name)" 2>/dev/null || true)"
  if [[ "${version}" =~ ^[1-9][0-9]*$ ]]; then
    pass "optional Secret Manager has enabled version for ${secret_id}; ${name} can be auto-resolved"
  else
    pass "optional runner admin token secret is absent; API deploy will use worker-token compatibility admin"
  fi
}

if [[ -z "${PROJECT_NUMBER}" ]]; then
  PROJECT_NUMBER="$(gcloud projects describe "${PROJECT_ID}" --format='value(projectNumber)' 2>/dev/null || true)"
fi
if [[ -z "${PROJECT_NUMBER}" ]]; then
  missing "GCP project is reachable: ${PROJECT_ID}"
else
  pass "GCP project is reachable: ${PROJECT_ID} (${PROJECT_NUMBER})"
fi

PUBLISHER_EMAIL="${PUBLISHER_ACCOUNT_ID}@${PROJECT_ID}.iam.gserviceaccount.com"
DEPLOY_EMAIL="${DEPLOY_ACCOUNT_ID}@${PROJECT_ID}.iam.gserviceaccount.com"
POOL_NAME="projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/${POOL_ID}"
PROVIDER_NAME="${POOL_NAME}/providers/${PROVIDER_ID}"
PRINCIPAL_SET="principalSet://iam.googleapis.com/${POOL_NAME}/attribute.repository/${REPOSITORY}"

required_apis=(
  artifactregistry.googleapis.com
  cloudresourcemanager.googleapis.com
  compute.googleapis.com
  iam.googleapis.com
  iamcredentials.googleapis.com
  run.googleapis.com
  secretmanager.googleapis.com
  servicenetworking.googleapis.com
  sqladmin.googleapis.com
  sts.googleapis.com
  vpcaccess.googleapis.com
)
enabled_apis="$(gcloud services list --enabled --project "${PROJECT_ID}" --format='value(config.name)' 2>/dev/null || true)"
for api in "${required_apis[@]}"; do
  if contains_line "${api}" "${enabled_apis}"; then
    pass "API enabled: ${api}"
  else
    missing "API enabled: ${api}"
  fi
done

bucket_values="$(gcloud storage buckets describe "gs://${STATE_BUCKET}" --project "${PROJECT_ID}" --format='value(uniform_bucket_level_access,public_access_prevention,versioning_enabled)' 2>/dev/null || true)"
if [[ -z "${bucket_values}" ]]; then
  missing "Terraform state bucket exists: gs://${STATE_BUCKET}"
else
  read -r bucket_ubla bucket_pap bucket_versioning <<< "${bucket_values}"
  pass "Terraform state bucket exists: gs://${STATE_BUCKET}"
  if truthy "${bucket_ubla}"; then
    pass "Terraform state bucket has uniform bucket-level access"
  else
    missing "Terraform state bucket has uniform bucket-level access"
  fi
  if [[ "${bucket_pap}" == "enforced" ]]; then
    pass "Terraform state bucket has public access prevention enforced"
  else
    missing "Terraform state bucket has public access prevention enforced"
  fi
  if truthy "${bucket_versioning}"; then
    pass "Terraform state bucket has object versioning"
  else
    missing "Terraform state bucket has object versioning"
  fi
fi

for email in "${PUBLISHER_EMAIL}" "${DEPLOY_EMAIL}"; do
  if gcloud iam service-accounts describe "${email}" --project "${PROJECT_ID}" >/dev/null 2>&1; then
    pass "service account exists: ${email}"
  else
    missing "service account exists: ${email}"
  fi
done

if gcloud iam workload-identity-pools describe "${POOL_ID}" --project "${PROJECT_ID}" --location global >/dev/null 2>&1; then
  pass "Workload Identity pool exists: ${POOL_ID}"
else
  missing "Workload Identity pool exists: ${POOL_ID}"
fi
if gcloud iam workload-identity-pools providers describe "${PROVIDER_ID}" --project "${PROJECT_ID}" --location global --workload-identity-pool "${POOL_ID}" >/dev/null 2>&1; then
  pass "Workload Identity provider exists: ${PROVIDER_ID}"
else
  missing "Workload Identity provider exists: ${PROVIDER_ID}"
fi

service_account_has_binding() {
  local email="$1"
  local role="$2"
  local member="$3"
  local found
  found="$(gcloud iam service-accounts get-iam-policy "${email}" \
    --project "${PROJECT_ID}" \
    --flatten='bindings[].members' \
    --filter="bindings.role:${role} AND bindings.members:${member}" \
    --format='value(bindings.role)' 2>/dev/null || true)"
  [[ -n "${found}" ]]
}

project_has_binding() {
  local role="$1"
  local member="$2"
  local found
  found="$(gcloud projects get-iam-policy "${PROJECT_ID}" \
    --flatten='bindings[].members' \
    --filter="bindings.role:${role} AND bindings.members:${member}" \
    --format='value(bindings.role)' 2>/dev/null || true)"
  [[ -n "${found}" ]]
}

for email in "${PUBLISHER_EMAIL}" "${DEPLOY_EMAIL}"; do
  if service_account_has_binding "${email}" "roles/iam.workloadIdentityUser" "${PRINCIPAL_SET}"; then
    pass "GitHub repository can impersonate ${email}"
  else
    missing "GitHub repository can impersonate ${email}"
  fi
done

if project_has_binding "roles/artifactregistry.writer" "serviceAccount:${PUBLISHER_EMAIL}"; then
  pass "publisher has roles/artifactregistry.writer"
else
  missing "publisher has roles/artifactregistry.writer"
fi

deploy_roles=(
  roles/artifactregistry.reader
  roles/cloudsql.admin
  roles/compute.networkAdmin
  roles/iam.serviceAccountAdmin
  roles/iam.serviceAccountUser
  roles/monitoring.admin
  roles/run.admin
  roles/secretmanager.admin
  roles/servicenetworking.networksAdmin
  roles/serviceusage.serviceUsageAdmin
  roles/storage.admin
  roles/storage.objectAdmin
)
for role in "${deploy_roles[@]}"; do
  if project_has_binding "${role}" "serviceAccount:${DEPLOY_EMAIL}"; then
    pass "deployer has ${role}"
  else
    missing "deployer has ${role}"
  fi
done

repo_secrets="$(gh secret list --app actions --repo "${REPOSITORY}" --json name --jq '.[].name' 2>/dev/null || true)"
legacy_gcp_secret_count=0
for secret in \
  BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER \
  BUCEPHALUS_GCP_SERVICE_ACCOUNT \
  BUCEPHALUS_GCP_DEPLOY_WORKLOAD_IDENTITY_PROVIDER \
  BUCEPHALUS_GCP_DEPLOY_SERVICE_ACCOUNT; do
  if contains_line "${secret}" "${repo_secrets}"; then
    legacy_gcp_secret_count=$((legacy_gcp_secret_count + 1))
  fi
done
if contains_line "BUC_CI_CD" "${repo_secrets}"; then
  pass "GitHub Actions repository secret exists: BUC_CI_CD"
elif [[ "${legacy_gcp_secret_count}" -eq 4 ]]; then
  pass "legacy split GitHub Actions GCP secrets exist"
else
  missing "GitHub Actions repository secret exists: BUC_CI_CD or complete legacy split GCP secret set"
fi

repo_variables="$(gh variable list --repo "${REPOSITORY}" --json name,value 2>/dev/null || true)"
env_variables="$(gh variable list --repo "${REPOSITORY}" --env "${GITHUB_ENVIRONMENT}" --json name,value 2>/dev/null || true)"
env_secrets="$(gh secret list --app actions --repo "${REPOSITORY}" --env "${GITHUB_ENVIRONMENT}" --json name --jq '.[].name' 2>/dev/null || true)"

if [[ -z "${repo_variables}" ]]; then
  repo_variables="[]"
fi
if [[ -z "${env_variables}" ]]; then
  env_variables="[]"
fi

assert_variable_equals "GitHub repository" "${repo_variables}" BUCEPHALUS_IMAGE_REPOSITORY "${IMAGE_REPOSITORY}"
assert_variable_equals "GitHub repository" "${repo_variables}" BUCEPHALUS_BUN_BASE_IMAGE "${BUN_BASE_IMAGE}"

assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_TERRAFORM_BACKEND_BUCKET "${STATE_BUCKET}"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_TERRAFORM_BACKEND_PREFIX "${STATE_PREFIX}"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_GCP_PROJECT_ID "${PROJECT_ID}"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_GCP_REGION "${REGION}"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_DEPLOYMENT_ENVIRONMENT "${ENVIRONMENT}"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_GCP_RESOURCE_PREFIX "${RESOURCE_PREFIX}"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_GOOGLE_OAUTH_CLIENT_ID "${GOOGLE_OAUTH_CLIENT_ID}"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_API_INGRESS "INGRESS_TRAFFIC_ALL"
assert_variable_equals "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" BUCEPHALUS_CLOUDFLARE_WORKER_NAME "${CLOUDFLARE_WORKER_NAME}"

if [[ "${REQUIRE_DEPLOY_SECRETS}" == "true" ]]; then
  if contains_line "BUCEPHALUS_WORKER_SMOKE" "${env_secrets}"; then
    pass "GitHub environment secret exists in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_WORKER_SMOKE"
  else
    missing "GitHub environment secret exists in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_WORKER_SMOKE"
  fi
  if contains_line "BUCEPHALUS_RUNNER_ADMIN_SMOKE" "${env_secrets}"; then
    pass "optional GitHub environment secret exists in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_RUNNER_ADMIN_SMOKE"
  else
    pass "optional GitHub environment secret absent in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_RUNNER_ADMIN_SMOKE"
  fi
  if contains_line "BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN" "${env_secrets}"; then
    pass "optional GitHub environment secret exists in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN"
  else
    pass "optional GitHub environment secret absent in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN"
  fi
fi

name_prefix="${RESOURCE_PREFIX}-${ENVIRONMENT}"
if [[ "${REQUIRE_API_STAGE}" == "true" || "${REQUIRE_POOL_STAGE}" == "true" ]]; then
  assert_numeric_variable_or_enabled_secret_version "${env_variables}" BUCEPHALUS_API_DATABASE_URL_SECRET_VERSION "${name_prefix}-api-database-url"
  assert_numeric_variable_or_enabled_secret_version "${env_variables}" BUCEPHALUS_MIGRATOR_DATABASE_URL_SECRET_VERSION "${name_prefix}-migrator-database-url"
  assert_numeric_variable_or_enabled_secret_version "${env_variables}" BUCEPHALUS_WORKER_TOKEN_SECRET_VERSION "${name_prefix}-worker-token"
  assert_optional_numeric_variable_or_enabled_secret_version "${env_variables}" BUCEPHALUS_RUNNER_ADMIN_TOKEN_SECRET_VERSION "${name_prefix}-runner-admin-token"
fi

if [[ "${REQUIRE_POOL_STAGE}" == "true" ]]; then
  assert_variable_or_secret_present "GitHub environment ${GITHUB_ENVIRONMENT}" "${env_variables}" "${env_secrets}" BUCEPHALUS_POOL_CONTROLLER_RUNNER_POOL_ID
  assert_numeric_variable_or_enabled_secret_version "${env_variables}" BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON_SECRET_VERSION "${name_prefix}-pool-provision-cmd-json"
  assert_numeric_variable_or_enabled_secret_version "${env_variables}" BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON_SECRET_VERSION "${name_prefix}-pool-reap-cmd-json"
fi

if [[ "${REQUIRE_CLOUDFLARE_UI_STAGE}" == "true" ]]; then
  cloudflare_account_id="$(json_variable_value "${env_variables}" BUCEPHALUS_CLOUDFLARE_ACCOUNT_ID)"
  legacy_cloudflare_account_id="$(json_variable_value "${env_variables}" CLOUDFLARE_ACCOUNT_ID)"
  if [[ -n "${cloudflare_account_id}" ]]; then
    pass "GitHub environment variable BUCEPHALUS_CLOUDFLARE_ACCOUNT_ID is configured"
  elif [[ -n "${legacy_cloudflare_account_id}" ]]; then
    pass "legacy GitHub environment variable CLOUDFLARE_ACCOUNT_ID is configured"
  elif contains_line "CLOUDFLARE_SECRET_ID" "${env_secrets}"; then
    pass "legacy GitHub environment secret CLOUDFLARE_SECRET_ID is configured"
  else
    missing "GitHub environment Cloudflare account id is configured: BUCEPHALUS_CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_ACCOUNT_ID, or CLOUDFLARE_SECRET_ID"
  fi
  if contains_line "CLOUDFLARE_SECRET_KEY" "${env_secrets}" || contains_line "CLOUDFLARE_API_TOKEN" "${env_secrets}"; then
    pass "GitHub environment Cloudflare API token secret exists in ${GITHUB_ENVIRONMENT}"
  else
    missing "GitHub environment Cloudflare API token secret exists in ${GITHUB_ENVIRONMENT}: CLOUDFLARE_SECRET_KEY or CLOUDFLARE_API_TOKEN"
  fi
  cloud_api_base="$(json_variable_value "${env_variables}" BUCEPHALUS_CLOUD_API_BASE)"
  if [[ -n "${cloud_api_base}" && "${cloud_api_base}" == http*://* ]]; then
    pass "Cloudflare UI deploy has configured BUCEPHALUS_CLOUD_API_BASE"
  elif gcloud run services describe "${name_prefix}-api" --project "${PROJECT_ID}" --region "${REGION}" --format "value(status.url)" >/dev/null 2>&1; then
    pass "Cloudflare UI deploy can discover API base from Cloud Run service ${name_prefix}-api"
  else
    missing "Cloudflare UI deploy has BUCEPHALUS_CLOUD_API_BASE or discoverable Cloud Run service ${name_prefix}-api"
  fi
fi

if [[ "${REQUIRE_SUBSTRATE}" == "true" || "${REQUIRE_PUSHED_IMAGES}" == "true" ]]; then
  if gcloud artifacts repositories describe "${ARTIFACT_REPOSITORY_ID}" --project "${PROJECT_ID}" --location "${REGION}" >/dev/null 2>&1; then
    pass "Artifact Registry repository exists: ${REGION}-docker.pkg.dev/${PROJECT_ID}/${ARTIFACT_REPOSITORY_ID}"
  else
    missing "Artifact Registry repository exists: ${REGION}-docker.pkg.dev/${PROJECT_ID}/${ARTIFACT_REPOSITORY_ID}"
  fi
fi

if [[ "${REQUIRE_PUSHED_IMAGES}" == "true" ]]; then
  artifact_repository_path="${REGION}-docker.pkg.dev/${PROJECT_ID}/${ARTIFACT_REPOSITORY_ID}"
  image_rows="$(gcloud artifacts docker images list "${artifact_repository_path}" --project "${PROJECT_ID}" --include-tags --format='csv[no-heading](package,version)' 2>/dev/null || true)"
  for component in api pool-controller migrations worker; do
    image_path="${IMAGE_REPOSITORY}/${component}"
    digest_count="$(printf '%s\n' "${image_rows}" | awk -F, -v package="${image_path}" '$1 == package && $2 ~ /^sha256:[a-f0-9]{64}$/ { print $2 }' | sort -u | wc -l | tr -d ' ')"
    if [[ "${digest_count}" -gt 0 ]]; then
      pass "pushed image digest exists: ${image_path}"
    else
      missing "pushed image digest exists: ${image_path}"
    fi
  done
fi

if [[ "${REQUIRE_PROMOTION_ARTIFACT}" == "true" ]]; then
  if [[ -z "${RELEASE_RUN_ID}" ]]; then
    missing "release_run_id provided for promotion artifact verification"
  else
    artifact_json="$(gh api "repos/${REPOSITORY}/actions/runs/${RELEASE_RUN_ID}/artifacts" 2>/dev/null || true)"
    artifact_found="$(ARTIFACT_JSON="${artifact_json}" PROMOTION_ARTIFACT_NAME="${PROMOTION_ARTIFACT_NAME}" bun -e '
      const data = JSON.parse(process.env.ARTIFACT_JSON || "{}");
      const requested = process.env.PROMOTION_ARTIFACT_NAME || "";
      const artifact = (data.artifacts || []).find((entry) => {
        if (entry.expired) return false;
        if (requested) return entry.name === requested;
        return /^cloud-release-promotion-.+$/.test(entry.name);
      });
      console.log(artifact ? "yes" : "");
    ' 2>/dev/null || true)"
    artifact_label="${PROMOTION_ARTIFACT_NAME:-cloud-release-promotion-<version>}"
    if [[ "${artifact_found}" == "yes" ]]; then
      pass "promotion evidence artifact exists in release run ${RELEASE_RUN_ID}: ${artifact_label}"
    else
      missing "promotion evidence artifact exists in release run ${RELEASE_RUN_ID}: ${artifact_label}"
    fi
  fi
fi

cat <<SUMMARY

readiness_summary
project_id=${PROJECT_ID}
project_number=${PROJECT_NUMBER}
repository=${REPOSITORY}
state_bucket=${STATE_BUCKET}
state_prefix=${STATE_PREFIX}
publisher_service_account=${PUBLISHER_EMAIL}
deploy_service_account=${DEPLOY_EMAIL}
workload_identity_provider=${PROVIDER_NAME}
image_repository=${IMAGE_REPOSITORY}
failures=${#failures[@]}
SUMMARY

if [[ "${#failures[@]}" -gt 0 ]]; then
  exit 1
fi
