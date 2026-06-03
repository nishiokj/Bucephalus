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
RELEASE_RUN_ID=""
PROMOTION_ARTIFACT_NAME="cloud-image-promotion-evidence-x86_64-unknown-linux-gnu"
REQUIRE_SUBSTRATE="false"
REQUIRE_PUSHED_IMAGES="false"
REQUIRE_PROMOTION_ARTIFACT="false"
REQUIRE_DEPLOY_SECRETS="false"

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
  --state-prefix <prefix>        Terraform state prefix. Default: bucephalus-cloud/<environment>.
  --pool-id <id>                 Workload Identity pool id. Default: bucephalus-github.
  --provider-id <id>             Workload Identity provider id. Default: github.
  --publisher-account-id <id>    Publisher service account id. Default: <prefix>-<env>-gh-publish.
  --deploy-account-id <id>       Deploy service account id. Default: <prefix>-<env>-gh-deploy.
  --image-repository <prefix>    Image repository prefix. Default: <region>-docker.pkg.dev/<project>/<prefix>-<env>-cloud/bucephalus-cloud.
  --release-run-id <id>          Release workflow run that should contain promotion evidence.
  --promotion-artifact-name <n>  Promotion artifact name. Default: cloud-image-promotion-evidence-x86_64-unknown-linux-gnu.
  --require-substrate            Require the Terraform-created Artifact Registry repository.
  --require-pushed-images        Require pushed API/pool/migration/worker image digests.
  --require-promotion-artifact   Require the promotion evidence artifact in --release-run-id.
  --require-deploy-secrets       Require deploy smoke token secrets in --github-environment.
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

STATE_BUCKET="${STATE_BUCKET:-${PROJECT_ID}-bucephalus-tfstate}"
STATE_PREFIX="${STATE_PREFIX:-bucephalus-cloud/${ENVIRONMENT}}"
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

if [[ "${REQUIRE_DEPLOY_SECRETS}" == "true" ]]; then
  env_secrets="$(gh secret list --app actions --repo "${REPOSITORY}" --env "${GITHUB_ENVIRONMENT}" --json name --jq '.[].name' 2>/dev/null || true)"
  if contains_line "BUCEPHALUS_WORKER_SMOKE" "${env_secrets}"; then
    pass "GitHub environment secret exists in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_WORKER_SMOKE"
  else
    missing "GitHub environment secret exists in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_WORKER_SMOKE"
  fi
  if contains_line "BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN" "${env_secrets}"; then
    pass "optional GitHub environment secret exists in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN"
  else
    pass "optional GitHub environment secret absent in ${GITHUB_ENVIRONMENT}: BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN"
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
  for component in api pool-controller migrations worker; do
    image_path="${IMAGE_REPOSITORY}/${component}"
    digest_count="$(gcloud artifacts docker images list "${image_path}" --project "${PROJECT_ID}" --include-tags --format='value(image_summary.digest)' 2>/dev/null | grep -E '^sha256:[a-f0-9]{64}$' | sort -u | wc -l | tr -d ' ')"
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
      const artifact = (data.artifacts || []).find((entry) => entry.name === process.env.PROMOTION_ARTIFACT_NAME && !entry.expired);
      console.log(artifact ? "yes" : "");
    ' 2>/dev/null || true)"
    if [[ "${artifact_found}" == "yes" ]]; then
      pass "promotion evidence artifact exists in release run ${RELEASE_RUN_ID}: ${PROMOTION_ARTIFACT_NAME}"
    else
      missing "promotion evidence artifact exists in release run ${RELEASE_RUN_ID}: ${PROMOTION_ARTIFACT_NAME}"
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
