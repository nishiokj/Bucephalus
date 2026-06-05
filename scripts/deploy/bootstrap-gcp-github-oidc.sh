#!/usr/bin/env bash
set -euo pipefail

PROJECT_ID=""
PROJECT_NUMBER=""
REGION="us-central1"
REPOSITORY="nishiokj/Bucephalus"
ENVIRONMENT="bucephalus"
RESOURCE_PREFIX="buc"
STATE_BUCKET=""
STATE_PREFIX=""
POOL_ID="bucephalus-github"
PROVIDER_ID="github"
PUBLISHER_ACCOUNT_ID=""
DEPLOY_ACCOUNT_ID=""
APPLY="false"

usage() {
  cat <<'USAGE'
Usage: scripts/deploy/bootstrap-gcp-github-oidc.sh --project-id <project> [options] [--apply]

One-time bootstrap for the Bucephalus Cloud GCP/GitHub Actions boundary:

  - enables APIs required before Terraform can run from GitHub
  - creates the Terraform state bucket if missing
  - creates GitHub Actions publisher/deployer service accounts if missing
  - creates a Workload Identity pool/provider for the GitHub repository
  - grants the provider workloadIdentityUser on both service accounts
  - grants project roles needed by release image publication and deploy
  - writes the required GitHub Actions secrets

The script is dry-run by default. Pass --apply to mutate GCP/GitHub state.

Options:
  --project-id <id>              GCP project id. Required.
  --project-number <number>      GCP project number. Auto-discovered when omitted.
  --region <region>              GCP region. Default: us-central1.
  --repository <owner/repo>      GitHub repository. Default: nishiokj/Bucephalus.
  --environment <name>           Deployment environment label. Default: bucephalus.
  --resource-prefix <prefix>     Resource prefix. Default: buc.
  --state-bucket <bucket>        Terraform state bucket. Default: <project>-bucephalus-tfstate.
  --state-prefix <prefix>        Terraform state prefix. Default: bucephalus-cloud/<environment>.
  --pool-id <id>                 Workload Identity pool id. Default: bucephalus-github.
  --provider-id <id>             Workload Identity provider id. Default: github.
  --publisher-account-id <id>    Publisher service account id. Default: <prefix>-<env>-gh-publish.
  --deploy-account-id <id>       Deploy service account id. Default: <prefix>-<env>-gh-deploy.
  --apply                        Execute changes. Without this, print commands only.
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
    --apply)
      APPLY="true"
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

for account_id in "${PUBLISHER_ACCOUNT_ID}" "${DEPLOY_ACCOUNT_ID}"; do
  if [[ ! "${account_id}" =~ ^[a-z][a-z0-9-]{4,28}[a-z0-9]$ ]]; then
    echo "service account id is invalid: ${account_id}" >&2
    exit 2
  fi
done

run() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
  if [[ "${APPLY}" == "true" ]]; then
    "$@"
  fi
}

capture() {
  if [[ "${APPLY}" == "true" ]]; then
    "$@"
  else
    printf ''
  fi
}

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

require_command gcloud
require_command gh

if [[ -z "${PROJECT_NUMBER}" ]]; then
  if [[ "${APPLY}" == "true" ]]; then
    PROJECT_NUMBER="$(gcloud projects describe "${PROJECT_ID}" --format='value(projectNumber)')"
  else
    PROJECT_NUMBER="<project-number>"
  fi
fi

PUBLISHER_EMAIL="${PUBLISHER_ACCOUNT_ID}@${PROJECT_ID}.iam.gserviceaccount.com"
DEPLOY_EMAIL="${DEPLOY_ACCOUNT_ID}@${PROJECT_ID}.iam.gserviceaccount.com"
POOL_NAME="projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/${POOL_ID}"
PROVIDER_NAME="${POOL_NAME}/providers/${PROVIDER_ID}"
PRINCIPAL_SET="principalSet://iam.googleapis.com/${POOL_NAME}/attribute.repository/${REPOSITORY}"

apis=(
  artifactregistry.googleapis.com
  cloudresourcemanager.googleapis.com
  compute.googleapis.com
  iam.googleapis.com
  iamcredentials.googleapis.com
  run.googleapis.com
  secretmanager.googleapis.com
  serviceusage.googleapis.com
  servicenetworking.googleapis.com
  sqladmin.googleapis.com
  sts.googleapis.com
  vpcaccess.googleapis.com
)

for api in "${apis[@]}"; do
  run gcloud services enable "${api}" --project "${PROJECT_ID}"
done

if ! gcloud storage buckets describe "gs://${STATE_BUCKET}" --project "${PROJECT_ID}" >/dev/null 2>&1; then
  run gcloud storage buckets create "gs://${STATE_BUCKET}" \
    --project "${PROJECT_ID}" \
    --location "${REGION}" \
    --uniform-bucket-level-access
else
  echo "state bucket exists: gs://${STATE_BUCKET}"
fi
run gcloud storage buckets update "gs://${STATE_BUCKET}" \
  --project "${PROJECT_ID}" \
  --uniform-bucket-level-access \
  --public-access-prevention \
  --versioning

for account in "${PUBLISHER_ACCOUNT_ID}" "${DEPLOY_ACCOUNT_ID}"; do
  email="${account}@${PROJECT_ID}.iam.gserviceaccount.com"
  if ! gcloud iam service-accounts describe "${email}" --project "${PROJECT_ID}" >/dev/null 2>&1; then
    run gcloud iam service-accounts create "${account}" \
      --project "${PROJECT_ID}" \
      --display-name "Bucephalus ${ENVIRONMENT} GitHub ${account}"
  else
    echo "service account exists: ${email}"
  fi
done

if ! gcloud iam workload-identity-pools describe "${POOL_ID}" --project "${PROJECT_ID}" --location global >/dev/null 2>&1; then
  run gcloud iam workload-identity-pools create "${POOL_ID}" \
    --project "${PROJECT_ID}" \
    --location global \
    --display-name "Bucephalus GitHub Actions"
else
  echo "workload identity pool exists: ${POOL_ID}"
fi

if ! gcloud iam workload-identity-pools providers describe "${PROVIDER_ID}" --project "${PROJECT_ID}" --location global --workload-identity-pool "${POOL_ID}" >/dev/null 2>&1; then
  run gcloud iam workload-identity-pools providers create-oidc "${PROVIDER_ID}" \
    --project "${PROJECT_ID}" \
    --location global \
    --workload-identity-pool "${POOL_ID}" \
    --display-name "GitHub ${REPOSITORY}" \
    --issuer-uri "https://token.actions.githubusercontent.com" \
    --attribute-mapping "google.subject=assertion.sub,attribute.actor=assertion.actor,attribute.repository=assertion.repository,attribute.ref=assertion.ref,attribute.workflow=assertion.workflow" \
    --attribute-condition "assertion.repository == '${REPOSITORY}'"
else
  echo "workload identity provider exists: ${PROVIDER_ID}"
fi

for email in "${PUBLISHER_EMAIL}" "${DEPLOY_EMAIL}"; do
  run gcloud iam service-accounts add-iam-policy-binding "${email}" \
    --project "${PROJECT_ID}" \
    --role roles/iam.workloadIdentityUser \
    --member "${PRINCIPAL_SET}"
done

publisher_roles=(
  roles/artifactregistry.writer
)
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

for role in "${publisher_roles[@]}"; do
  run gcloud projects add-iam-policy-binding "${PROJECT_ID}" \
    --member "serviceAccount:${PUBLISHER_EMAIL}" \
    --role "${role}"
done
for role in "${deploy_roles[@]}"; do
  run gcloud projects add-iam-policy-binding "${PROJECT_ID}" \
    --member "serviceAccount:${DEPLOY_EMAIL}" \
    --role "${role}"
done

if [[ "${APPLY}" == "true" ]]; then
  BUC_CI_CD_VALUE="$(printf '{"publish":{"workload_identity_provider":"%s","service_account":"%s"},"deploy":{"workload_identity_provider":"%s","service_account":"%s"}}' "${PROVIDER_NAME}" "${PUBLISHER_EMAIL}" "${PROVIDER_NAME}" "${DEPLOY_EMAIL}")"
  printf '%s' "${BUC_CI_CD_VALUE}" | gh secret set BUC_CI_CD --app actions --repo "${REPOSITORY}"
  printf '%s' "${PROVIDER_NAME}" | gh secret set BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER --app actions --repo "${REPOSITORY}"
  printf '%s' "${PUBLISHER_EMAIL}" | gh secret set BUCEPHALUS_GCP_SERVICE_ACCOUNT --app actions --repo "${REPOSITORY}"
  printf '%s' "${PROVIDER_NAME}" | gh secret set BUCEPHALUS_GCP_DEPLOY_WORKLOAD_IDENTITY_PROVIDER --app actions --repo "${REPOSITORY}"
  printf '%s' "${DEPLOY_EMAIL}" | gh secret set BUCEPHALUS_GCP_DEPLOY_SERVICE_ACCOUNT --app actions --repo "${REPOSITORY}"
else
  echo "+ gh secret set BUC_CI_CD --repo ${REPOSITORY} <<< '{\"publish\":{\"workload_identity_provider\":\"${PROVIDER_NAME}\",\"service_account\":\"${PUBLISHER_EMAIL}\"},\"deploy\":{\"workload_identity_provider\":\"${PROVIDER_NAME}\",\"service_account\":\"${DEPLOY_EMAIL}\"}}'"
  echo "+ gh secret set BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER --repo ${REPOSITORY} <<< ${PROVIDER_NAME}"
  echo "+ gh secret set BUCEPHALUS_GCP_SERVICE_ACCOUNT --repo ${REPOSITORY} <<< ${PUBLISHER_EMAIL}"
  echo "+ gh secret set BUCEPHALUS_GCP_DEPLOY_WORKLOAD_IDENTITY_PROVIDER --repo ${REPOSITORY} <<< ${PROVIDER_NAME}"
  echo "+ gh secret set BUCEPHALUS_GCP_DEPLOY_SERVICE_ACCOUNT --repo ${REPOSITORY} <<< ${DEPLOY_EMAIL}"
fi

cat <<SUMMARY

bootstrap_${APPLY}
project_id=${PROJECT_ID}
project_number=${PROJECT_NUMBER}
repository=${REPOSITORY}
state_bucket=${STATE_BUCKET}
state_prefix=${STATE_PREFIX}
publisher_service_account=${PUBLISHER_EMAIL}
deploy_service_account=${DEPLOY_EMAIL}
workload_identity_provider=${PROVIDER_NAME}

Next:
1. Run the GCP deploy workflow with terraform_action=substrate-apply and backend bucket/prefix above.
2. Run the release workflow with build_images=true and push_images=true after approving a Bun base digest.
3. Run the GCP deploy workflow with terraform_action=apply and the pushed promotion evidence artifact.
SUMMARY
