#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

require_command bun

WORK_DIR="$(mktemp -d "${ROOT_DIR}/bucephalus-cloud/.verify-release-boundary.XXXXXX")"
cleanup() {
  rm -rf "${WORK_DIR}"
}
trap cleanup EXIT
VERIFY_JS="${WORK_DIR}/verify-cloud-release-boundary.mjs"
cat > "${VERIFY_JS}" <<'JS'
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import YAML from "yaml";

const root = process.env.ROOT_DIR;
const failures = [];

function fail(message) {
  failures.push(message);
}

function read(path) {
  return readFileSync(join(root, path), "utf8");
}

function listFiles(dir) {
  const base = join(root, dir);
  const out = [];
  function walk(path) {
    for (const name of readdirSync(path)) {
      const child = join(path, name);
      const stat = statSync(child);
      if (stat.isDirectory()) {
        walk(child);
      } else if (stat.isFile()) {
        out.push(relative(root, child).split("\\").join("/"));
      }
    }
  }
  walk(base);
  return out.sort();
}

const releaseWorkflowPath = ".github/workflows/bucephalus-release.yml";
const releaseWorkflowText = read(releaseWorkflowPath);
const releaseWorkflow = YAML.parse(releaseWorkflowText);
const deployWorkflowPath = ".github/workflows/bucephalus-gcp-deploy.yml";
const deployWorkflowText = read(deployWorkflowPath);
const deployWorkflow = YAML.parse(deployWorkflowText);
const cleanupWorkflowPath = ".github/workflows/bucephalus-gcp-cleanup.yml";
const cleanupWorkflowText = read(cleanupWorkflowPath);
const cleanupWorkflow = YAML.parse(cleanupWorkflowText);
const cloudCiWorkflowPath = ".github/workflows/bucephalus-cloud-ci.yml";
const cloudCiWorkflowText = read(cloudCiWorkflowPath);
const cloudCiWorkflow = YAML.parse(cloudCiWorkflowText);
const rustQualityWorkflowPath = ".github/workflows/rust-quality-security.yml";
const rustQualityWorkflowText = read(rustQualityWorkflowPath);
const rustQualityWorkflow = YAML.parse(rustQualityWorkflowText);
const cloudGatesPath = "scripts/ci/cloud-gates.sh";
const cloudGatesText = read(cloudGatesPath);
const installScriptPath = "scripts/install.sh";
const installScriptText = read(installScriptPath);
const cloudImageBuildPath = "scripts/release/build-cloud-images.sh";
const cloudImageBuildText = read(cloudImageBuildPath);
const gcpInfraPath = "bucephalus-cloud/infra/gcp/main.tf";
const gcpInfraText = read(gcpInfraPath);
const gcpVariablesPath = "bucephalus-cloud/infra/gcp/variables.tf";
const gcpVariablesText = read(gcpVariablesPath);

for (const forbidden of [
  /\bscp\b/,
  /\bssh\b/,
  /startup-script/i,
  /systemd/i,
  /macstudio/i,
  /orbstack/i,
  /\.env(?:\.example)?\b/,
]) {
  if (forbidden.test(releaseWorkflowText)) {
    fail(`${releaseWorkflowPath} contains retired deployment surface matching ${forbidden}`);
  }
  if (forbidden.test(deployWorkflowText)) {
    fail(`${deployWorkflowPath} contains retired deployment surface matching ${forbidden}`);
  }
}

if (!releaseWorkflowText.includes("scripts/release/verify-buc-release.sh")) {
  fail(`${releaseWorkflowPath} must verify cloud release archives before upload`);
}
if (!releaseWorkflowText.includes("scripts/release/verify-core-release.sh")) {
  fail(`${releaseWorkflowPath} must verify core release archives before upload`);
}
if (!releaseWorkflowText.includes("scripts/release/write-cloud-release-provenance.sh")) {
  fail(`${releaseWorkflowPath} must write recorded release provenance`);
}
if (!releaseWorkflowText.includes("scripts/release/write-core-release-provenance.sh")) {
  fail(`${releaseWorkflowPath} must write recorded core release provenance`);
}
if (!releaseWorkflowText.includes("scripts/release/write-release-asset-index.sh")) {
  fail(`${releaseWorkflowPath} must write a verified release asset index before publication`);
}
if (!releaseWorkflowText.includes("scripts/release/verify-release-asset-index.sh")) {
  fail(`${releaseWorkflowPath} must verify the release asset index before publication`);
}
if (!releaseWorkflowText.includes("scripts/release/write-cloud-image-promotion-evidence-index.sh")) {
  fail(`${releaseWorkflowPath} must write a verified image promotion evidence index before pushed-image handoff`);
}
if (!releaseWorkflowText.includes("scripts/release/verify-cloud-image-promotion-evidence-index.sh")) {
  fail(`${releaseWorkflowPath} must verify the image promotion evidence index before upload`);
}
if (!releaseWorkflowText.includes("scripts/release/build-cloud-images.sh")) {
  fail(`${releaseWorkflowPath} must build Cloud images through the release-bundle image script`);
}
if (!releaseWorkflowText.includes("scripts/release/write-gcp-image-tfvars.sh")) {
  fail(`${releaseWorkflowPath} must derive GCP image tfvars from verified pushed image manifests`);
}
if (!releaseWorkflowText.includes("scripts/release/verify-cloud-image-publish-inputs.sh")) {
  fail(`${releaseWorkflowPath} must verify image publish inputs before image build/push`);
}
if (!releaseWorkflowText.includes("scripts/release/verify-cloud-base-image-policy.sh")) {
  fail(`${releaseWorkflowPath} must verify the approved base image policy before image build/push`);
}
if (!releaseWorkflowText.includes("google-github-actions/auth@v3")) {
  fail(`${releaseWorkflowPath} must authenticate pushed image publication through Google Workload Identity`);
}
if (!releaseWorkflowText.includes("google-github-actions/setup-gcloud@v3")) {
  fail(`${releaseWorkflowPath} must install gcloud before configuring Artifact Registry Docker auth`);
}
if (!releaseWorkflowText.includes("scripts/release/configure-gcp-artifact-registry-auth.sh")) {
  fail(`${releaseWorkflowPath} must configure Artifact Registry Docker auth before pushed image publication`);
}
for (const removedInput of ["build_images", "push_images", "build_public_core_artifacts", "publish_github_release", "source_release_version"]) {
  if (releaseWorkflow.on?.workflow_dispatch?.inputs?.[removedInput]) {
    fail(`${releaseWorkflowPath} must not expose ${removedInput}; a release always publishes installable assets and Cloud images`);
  }
}
if (!releaseWorkflowText.includes("BUCEPHALUS_BUN_BASE_IMAGE") || !releaseWorkflowText.includes("BUCEPHALUS_IMAGE_REPOSITORY")) {
  fail(`${releaseWorkflowPath} must read image publish repository/base-image policy from shared workflow config`);
}
const releasePush = releaseWorkflow.on?.push ?? {};
const releaseBranches = Array.isArray(releasePush.branches) ? releasePush.branches : [];
if (releaseBranches.length > 0) {
  fail(`${releaseWorkflowPath} must not create release artifacts on branch pushes; only v* tags or manual release dispatches publish releases`);
}
if (!Array.isArray(releasePush.tags) || !releasePush.tags.includes("v*")) {
  fail(`${releaseWorkflowPath} must run full product releases on v* tag pushes`);
}
const releaseWorkflowInputs = releaseWorkflow.on?.workflow_dispatch?.inputs ?? {};
const releaseInputNames = Object.keys(releaseWorkflowInputs);
if (releaseInputNames.length !== 1 || releaseWorkflowInputs.version || releaseWorkflowInputs.version_override?.type !== "string" || releaseWorkflowInputs.version_override?.required !== true) {
  fail(`${releaseWorkflowPath} manual releases must expose exactly one required version_override string input`);
}
if (!releaseWorkflowText.includes("scripts/release/resolve-release-version.sh")) {
  fail(`${releaseWorkflowPath} must resolve artifact versions from tags or tracked package metadata`);
}
for (const inputName of [
  "cloudflare_worker_name",
  "cloudflare_api_base",
  "cloudflare_google_oauth_client_id",
  "deploy_cloudflare_ui",
  "image_repository",
  "bun_base_image",
]) {
  if (releaseWorkflowInputs[inputName]) {
    fail(`${releaseWorkflowPath} must not ask operators for ${inputName}; frontend deploy configuration belongs in the frontend repository`);
  }
}
for (const inputName of ["source_release_run_id", "source_release_artifact_name", "source_release_version"]) {
  if (releaseWorkflowInputs[inputName]) {
    fail(`${releaseWorkflowPath} must not ask operators for ${inputName}; releases publish images from the just-built release bundle`);
  }
}
if (!read("scripts/release/resolve-cloud-release-artifacts.sh").includes("cloud-release-promotion-${VERSION}")) {
  fail("scripts/release/resolve-cloud-release-artifacts.sh must resolve the single versioned release promotion artifact");
}

if (!deployWorkflowText.includes("actions/download-artifact@v4")) {
  fail(`${deployWorkflowPath} must download pushed image promotion evidence from a release workflow run`);
}
const deployWorkflowInputs = deployWorkflow.on?.workflow_dispatch?.inputs ?? {};
const deployStageInput = deployWorkflowInputs.deployment_stage;
if (deployStageInput?.type !== "choice" || deployStageInput?.default !== "substrate") {
  fail(`${deployWorkflowPath} must expose a substrate/api/pool deployment_stage dropdown`);
}
for (const stage of ["substrate", "api", "pool"]) {
  if (!Array.isArray(deployStageInput?.options) || !deployStageInput.options.includes(stage)) {
    fail(`${deployWorkflowPath} deployment_stage dropdown must include ${stage}`);
  }
}
if (deployWorkflowInputs.apply?.type !== "boolean" || deployWorkflowInputs.apply?.default !== false) {
  fail(`${deployWorkflowPath} must expose an explicit boolean apply switch that defaults to plan-only`);
}
if (deployWorkflow.on?.workflow_dispatch?.inputs?.release_version?.type !== "string" || deployWorkflow.on?.workflow_dispatch?.inputs?.release_version?.required === true) {
  fail(`${deployWorkflowPath} must default to latest promotion evidence and expose only an optional release version override`);
}
if (deployWorkflowInputs.release_artifact) {
  fail(`${deployWorkflowPath} must not ask operators to choose a promotion artifact flavor`);
}
for (const inputName of Object.keys(deployWorkflowInputs)) {
  if (/archive.*(url|sha|checksum)|artifact.*(url|sha|checksum)|sha256/i.test(inputName)) {
    fail(`${deployWorkflowPath} must not ask operators for raw release archive URLs or SHA256 inputs (${inputName})`);
  }
}
if (!deployWorkflowText.includes("Resolve release promotion evidence") || !deployWorkflowText.includes("resolve-cloud-release-artifacts.sh") || !deployWorkflowText.includes("--latest") || deployWorkflowText.includes("--promotion-artifact")) {
  fail(`${deployWorkflowPath} must resolve latest promotion evidence by default before download`);
}
if (!deployWorkflowText.includes("scripts/release/verify-cloud-image-promotion-evidence-index.sh")) {
  fail(`${deployWorkflowPath} must verify pushed image promotion evidence before Terraform plan/apply`);
}
if (!deployWorkflowText.includes("scripts/deploy/write-gcp-deploy-tfvars.sh")) {
  fail(`${deployWorkflowPath} must render deploy tfvars through the checked deploy input writer`);
}
if (!deployWorkflowText.includes("Resolve deploy secret versions") || !deployWorkflowText.includes("gcloud secrets versions list") || !deployWorkflowText.includes("state=enabled")) {
  fail(`${deployWorkflowPath} must auto-resolve Secret Manager versions for API/pool deploy plans after GCP auth`);
}
if (!deployWorkflowText.includes("gcp-image-digests.tfvars")) {
  fail(`${deployWorkflowPath} must consume generated digest tfvars from promotion evidence`);
}
if (!deployWorkflowText.includes("--deploy-control-plane-services")) {
  fail(`${deployWorkflowPath} must explicitly choose substrate-only versus service deployment`);
}
if (/api_image_digest|pool_controller_image_digest|migration_image_digest/.test(JSON.stringify(deployWorkflow.on?.workflow_dispatch?.inputs ?? {}))) {
  fail(`${deployWorkflowPath} must not accept handwritten deploy image digest inputs`);
}
for (const inputName of [
  "release_run_id",
  "promotion_artifact_name",
  "terraform_backend_bucket",
  "terraform_backend_prefix",
  "project_id",
  "region",
  "deployment_environment",
  "resource_prefix",
  "oauth_user_client_id",
  "pool_controller_runner_pool_id",
  "api_database_url_secret_version",
  "migrator_database_url_secret_version",
  "worker_token_secret_version",
  "pool_controller_provision_cmd_json_secret_version",
  "pool_controller_reap_cmd_json_secret_version",
  "api_ingress",
]) {
  if (deployWorkflowInputs[inputName]) {
    fail(`${deployWorkflowPath} must not ask operators for ${inputName}; deploy config belongs in the GitHub environment`);
  }
}
for (const requiredEnv of [
  "BUCEPHALUS_TERRAFORM_BACKEND_BUCKET",
  "BUCEPHALUS_TERRAFORM_BACKEND_PREFIX",
  "BUCEPHALUS_GCP_PROJECT_ID",
  "BUCEPHALUS_GCP_REGION",
  "BUCEPHALUS_DEPLOYMENT_ENVIRONMENT",
  "BUCEPHALUS_GCP_RESOURCE_PREFIX",
  "BUCEPHALUS_GOOGLE_OAUTH_CLIENT_ID",
  "BUCEPHALUS_API_DATABASE_URL_SECRET_VERSION",
  "BUCEPHALUS_MIGRATOR_DATABASE_URL_SECRET_VERSION",
  "BUCEPHALUS_WORKER_TOKEN_SECRET_VERSION",
]) {
  if (!deployWorkflowText.includes(requiredEnv)) {
    fail(`${deployWorkflowPath} must read ${requiredEnv} from GitHub environment configuration`);
  }
}
if (!deployWorkflowText.includes("terraform init") || !deployWorkflowText.includes("terraform plan") || !deployWorkflowText.includes("terraform apply")) {
  fail(`${deployWorkflowPath} must run Terraform init, plan, and gated apply`);
}
if (!deployWorkflowText.includes('-out="${RUNNER_TEMP}/bucephalus-gcp.tfplan"') || !deployWorkflowText.includes('terraform apply -input=false -auto-approve "${RUNNER_TEMP}/bucephalus-gcp.tfplan"')) {
  fail(`${deployWorkflowPath} must apply the exact Terraform plan generated in the same run`);
}
if (deployWorkflowText.includes("Refuse implicit service cleanup") || deployWorkflowText.includes("Use the Bucephalus GCP Cleanup workflow")) {
  fail(`${deployWorkflowPath} must not force a separate cleanup workflow before substrate deploys`);
}
if (!deployWorkflowText.includes("BUCEPHALUS_TERRAFORM_BACKEND_BUCKET") || !deployWorkflowText.includes("BUCEPHALUS_TERRAFORM_BACKEND_PREFIX")) {
  fail(`${deployWorkflowPath} must use a remote Terraform backend from GitHub environment config`);
}
if (!deployWorkflowText.includes("gcloud run jobs execute") || !deployWorkflowText.includes("-migrations")) {
  fail(`${deployWorkflowPath} must run the scoped Cloud Run migration job after apply`);
}
if (deployWorkflowText.includes("-target=google_cloud_run_v2_job.migrations")) {
  fail(`${deployWorkflowPath} must not use targeted Terraform applies for normal deploy flow`);
}
if (deployWorkflowText.includes("terraform_action") || deployWorkflowText.includes("substrate-plan") || deployWorkflowText.includes("api-apply") || deployWorkflowText.includes("pool-apply")) {
  fail(`${deployWorkflowPath} must use deployment_stage plus apply instead of six plan/apply action names`);
}
if (!deployWorkflowText.includes("BUCEPHALUS_WORKER_SMOKE")) {
  fail(`${deployWorkflowPath} must require a worker smoke identity after apply`);
}
if (!deployWorkflowText.includes("BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN") || !deployWorkflowText.includes("skipping user-route smoke check")) {
  fail(`${deployWorkflowPath} must support optional user smoke identity after apply`);
}
if (!deployWorkflowText.includes("/v1/packages") || !deployWorkflowText.includes("/v1/runner-pools")) {
  fail(`${deployWorkflowPath} must smoke both user and worker API authentication paths`);
}

const cleanupWorkflowInputs = cleanupWorkflow.on?.workflow_dispatch?.inputs ?? {};
const cleanupTargetInput = cleanupWorkflowInputs.cleanup_target;
if (cleanupTargetInput?.type !== "choice" || cleanupTargetInput?.default !== "control-plane-services") {
  fail(`${cleanupWorkflowPath} must expose a cleanup_target dropdown defaulting to control-plane-services`);
}
for (const target of ["control-plane-services", "pool-controller"]) {
  if (!Array.isArray(cleanupTargetInput?.options) || !cleanupTargetInput.options.includes(target)) {
    fail(`${cleanupWorkflowPath} cleanup_target dropdown must include ${target}`);
  }
}
if (cleanupWorkflowInputs.apply?.type !== "boolean" || cleanupWorkflowInputs.apply?.default !== false) {
  fail(`${cleanupWorkflowPath} must expose an explicit boolean apply switch that defaults to plan-only`);
}
if (!cleanupWorkflowText.includes("scripts/deploy/write-gcp-deploy-tfvars.sh") || !cleanupWorkflowText.includes("--deploy-api-services") || !cleanupWorkflowText.includes("--deploy-pool-controller")) {
  fail(`${cleanupWorkflowPath} must render cleanup tfvars by explicitly setting service deploy flags`);
}
if (!cleanupWorkflowText.includes("control-plane-services") || !cleanupWorkflowText.includes("pool-controller")) {
  fail(`${cleanupWorkflowPath} must support explicit control-plane service and pool-controller cleanup targets`);
}
if (!cleanupWorkflowText.includes('-out="${RUNNER_TEMP}/bucephalus-gcp-cleanup.tfplan"') || !cleanupWorkflowText.includes('terraform apply -input=false -auto-approve "${RUNNER_TEMP}/bucephalus-gcp-cleanup.tfplan"')) {
  fail(`${cleanupWorkflowPath} must apply the exact Terraform cleanup plan generated in the same run`);
}
if (cleanupWorkflowText.includes("terraform destroy")) {
  fail(`${cleanupWorkflowPath} must not expose full substrate destroy as a routine cleanup action`);
}

const permissions = releaseWorkflow.permissions ?? {};
if (permissions.contents !== "read" || permissions["id-token"] !== "none") {
  fail(`${releaseWorkflowPath} top-level permissions must default to contents: read and id-token: none`);
}

const deployPermissions = deployWorkflow.permissions ?? {};
if (deployPermissions.contents !== "read" || deployPermissions.actions !== "read" || deployPermissions["id-token"] !== "none") {
  fail(`${deployWorkflowPath} top-level permissions must default to contents/actions read and id-token: none`);
}
const releaseJobs = releaseWorkflow.jobs ?? {};
const deployJobs = deployWorkflow.jobs ?? {};
const cleanupJobs = cleanupWorkflow.jobs ?? {};
function artifactUploadSteps(job) {
  return (job?.steps ?? []).filter((step) => step.uses === "actions/upload-artifact@v4");
}

const deployGcp = deployJobs["deploy-gcp"];
if (!deployGcp) {
  fail(`${deployWorkflowPath} must contain deploy-gcp job`);
} else {
  if (deployGcp.permissions?.contents !== "read" || deployGcp.permissions?.actions !== "read" || deployGcp.permissions?.["id-token"] !== "write") {
    fail(`${deployWorkflowPath} deploy-gcp must receive contents/actions read and OIDC token write permissions`);
  }
  const deployStepNames = (deployGcp.steps ?? []).map((step) => step.name).filter(Boolean);
  for (const required of [
    "Resolve GCP deploy config",
    "Resolve release promotion evidence",
    "Download pushed image promotion evidence",
    "Locate and verify promotion evidence",
    "Authenticate to Google Cloud for deployment",
    "Set up gcloud for deployment helpers",
    "Resolve deploy secret versions",
    "Render deploy tfvars",
    "Terraform plan",
    "Terraform apply",
    "Run migration job",
    "Smoke deployed API",
  ]) {
    if (!deployStepNames.includes(required)) {
      fail(`${deployWorkflowPath} deploy-gcp missing step: ${required}`);
    }
  }
  const deploySteps = deployGcp.steps ?? [];
  const planStep = deploySteps.find((step) => step.name === "Terraform plan");
  if (String(planStep?.if ?? "").trim()) {
    fail(`${deployWorkflowPath} Terraform plan must run for every deployment_stage`);
  }
  const applyStep = deploySteps.find((step) => step.name === "Terraform apply");
  if (!String(applyStep?.if ?? "").includes("inputs.apply") || !String(applyStep?.run ?? "").includes("bucephalus-gcp.tfplan")) {
    fail(`${deployWorkflowPath} Terraform apply must be gated by inputs.apply and consume the generated plan file`);
  }
  const authIndex = deploySteps.findIndex((step) => step.name === "Authenticate to Google Cloud for deployment");
  const renderIndex = deploySteps.findIndex((step) => step.name === "Render deploy tfvars");
  const resolveSecretIndex = deploySteps.findIndex((step) => step.name === "Resolve deploy secret versions");
  if (authIndex < 0 || resolveSecretIndex < authIndex || renderIndex < resolveSecretIndex) {
    fail(`${deployWorkflowPath} must authenticate, resolve deploy secret versions, then render tfvars`);
  }
  const gcloudDeployStep = deploySteps.find((step) => step.name === "Set up gcloud for deployment helpers");
  const gcloudIf = String(gcloudDeployStep?.if ?? "");
  if (!gcloudIf.includes("inputs.deployment_stage != 'substrate'")) {
    fail(`${deployWorkflowPath} must install gcloud for API and pool secret-version discovery`);
  }
}

const cleanupGcp = cleanupJobs["cleanup-gcp"];
if (!cleanupGcp) {
  fail(`${cleanupWorkflowPath} must contain cleanup-gcp job`);
} else {
  if (cleanupGcp.permissions?.contents !== "read" || cleanupGcp.permissions?.actions !== "read" || cleanupGcp.permissions?.["id-token"] !== "write") {
    fail(`${cleanupWorkflowPath} cleanup-gcp must receive contents/actions read and OIDC token write permissions`);
  }
  const cleanupStepNames = (cleanupGcp.steps ?? []).map((step) => step.name).filter(Boolean);
  for (const required of [
    "Resolve cleanup target",
    "Resolve GCP cleanup config",
    "Resolve GCP CI/CD auth secret for cleanup",
    "Authenticate to Google Cloud for cleanup",
    "Render cleanup tfvars",
    "Terraform cleanup plan",
    "Terraform cleanup apply",
  ]) {
    if (!cleanupStepNames.includes(required)) {
      fail(`${cleanupWorkflowPath} cleanup-gcp missing step: ${required}`);
    }
  }
  const cleanupApplyStep = (cleanupGcp.steps ?? []).find((step) => step.name === "Terraform cleanup apply");
  if (!String(cleanupApplyStep?.if ?? "").includes("inputs.apply") || !String(cleanupApplyStep?.run ?? "").includes("bucephalus-gcp-cleanup.tfplan")) {
    fail(`${cleanupWorkflowPath} Terraform cleanup apply must be gated by inputs.apply and consume the generated cleanup plan file`);
  }
}

for (const [jobName, job] of Object.entries(releaseJobs)) {
  for (const step of artifactUploadSteps(job)) {
    const retentionDays = step.with?.["retention-days"];
    if (!Number.isInteger(retentionDays) || retentionDays < 1 || retentionDays > 30) {
      fail(`${releaseWorkflowPath} ${jobName}/${step.name ?? "<unnamed upload>"} must set retention-days between 1 and 30`);
    }
    if (step.with?.["if-no-files-found"] !== "error") {
      fail(`${releaseWorkflowPath} ${jobName}/${step.name ?? "<unnamed upload>"} must use if-no-files-found: error`);
    }
  }
}

const releaseGates = releaseJobs["release-gates"];
if (!releaseGates) {
  fail(`${releaseWorkflowPath} must contain release-gates job`);
} else {
  if (releaseGates.permissions?.contents !== "read" || releaseGates.permissions?.["id-token"] === "write") {
    fail(`${releaseWorkflowPath} release-gates must not receive OIDC token write permission`);
  }
  const steps = releaseGates.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  if (!stepNames.includes("Validate image build inputs")) {
    fail(`${releaseWorkflowPath} release-gates missing step: Validate image build inputs`);
  }
}

if (releaseJobs["build-cloud-ui-assets"] || releaseJobs["deploy-cloudflare-ui"]) {
  fail(`${releaseWorkflowPath} must not build or deploy frontend assets; Cloud UI CI/CD lives in the frontend repository`);
}

if (releaseJobs["publish-cloud-images-from-release"]) {
  fail(`${releaseWorkflowPath} must not contain a separate from-release image publication job; product releases publish images from the just-built release bundle`);
}

const buildLinux = releaseJobs["build-linux-release"];
if (!buildLinux) {
  fail(`${releaseWorkflowPath} must contain build-linux-release job`);
} else {
  if (buildLinux.permissions?.contents !== "read" || buildLinux.permissions?.["id-token"] !== "write") {
    fail(`${releaseWorkflowPath} build-linux-release must be the Linux core/Cloud job with OIDC token write permission for optional image publication`);
  }
  const matrixTargets = (buildLinux.strategy?.matrix?.include ?? []).map((entry) => entry.target).filter(Boolean);
  for (const requiredTarget of ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]) {
    if (!matrixTargets.includes(requiredTarget)) {
      fail(`${releaseWorkflowPath} build-linux-release matrix missing Linux target ${requiredTarget}`);
    }
  }
  if (matrixTargets.some((target) => !String(target).includes("linux"))) {
    fail(`${releaseWorkflowPath} build-linux-release must not wait on non-Linux targets before Cloud release/image work`);
  }
  const steps = buildLinux.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  const hasBunSetup = steps.some((step) => step.uses === "oven-sh/setup-bun@v2" && step.with?.["bun-version"] === "1.3.14");
  if (!hasBunSetup) {
    fail(`${releaseWorkflowPath} build-linux-release must install pinned Bun before building core and Cloud release bundles`);
  }
  for (const required of [
    "Build core release archive",
    "Verify core release archive",
    "Write core release provenance",
    "Upload core release archive",
    "Extract verified core binary for Cloud bundle",
    "Authenticate to Google Cloud for image publication",
    "Set up gcloud for image publication",
    "Configure Artifact Registry Docker auth",
    "Set up Docker Buildx for image cache",
    "Verify release bundle",
    "Write release provenance",
    "Build and inspect Cloud images",
    "Write image build provenance",
    "Write GCP image tfvars",
    "Verify GCP image promotion evidence",
    "Write Cloud image promotion evidence index",
    "Verify Cloud image promotion evidence index",
    "Upload Cloud image promotion evidence",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${releaseWorkflowPath} build-linux-release missing step: ${required}`);
    }
  }
  const imageBuildStep = steps.find((step) => step.name === "Build and inspect Cloud images");
  if (!String(imageBuildStep?.run ?? "").includes("--push")) {
    fail(`${releaseWorkflowPath} image build step must always pass --push for product releases`);
  }
  const buildxStep = steps.find((step) => step.name === "Set up Docker Buildx for image cache");
  const buildxIndex = stepNames.indexOf("Set up Docker Buildx for image cache");
  const imageBuildIndex = stepNames.indexOf("Build and inspect Cloud images");
  if (buildxStep?.uses !== "docker/setup-buildx-action@v3" || buildxIndex < 0 || buildxIndex >= imageBuildIndex) {
    fail(`${releaseWorkflowPath} must set up a Docker Buildx driver that supports registry cache before image builds`);
  }
  const buildBundleStep = steps.find((step) => step.name === "Build release bundle");
  if (buildBundleStep?.env?.BUCEPHALUS_RELEASE_SKIP_CLOUD_CHECKS !== "true") {
    fail(`${releaseWorkflowPath} build-linux-release must skip repeated Cloud checks after release-gates`);
  }
  if (!String(buildBundleStep?.run ?? "").includes("--core-bin")) {
    fail(`${releaseWorkflowPath} build-linux-release must pass the verified core binary into build-buc-release.sh`);
  }
  const buildCoreStep = steps.find((step) => step.name === "Build core release archive");
  if (!String(buildCoreStep?.run ?? "").includes("build-core-release.sh")) {
    fail(`${releaseWorkflowPath} build-linux-release must build the verified Linux Core archive before assembling the Cloud bundle`);
  }
  const extractCoreStep = steps.find((step) => step.name === "Extract verified core binary for Cloud bundle");
  if (!String(extractCoreStep?.run ?? "").includes("dist/releases/bucephalus-${{ matrix.target }}.tar.gz")) {
    fail(`${releaseWorkflowPath} build-linux-release must extract the Cloud bundle binary from the just-verified Core archive`);
  }
  const authStep = steps.find((step) => step.name === "Authenticate to Google Cloud for image publication");
  const releaseProvenanceIndex = stepNames.indexOf("Write release provenance");
  const authIndex = stepNames.indexOf("Authenticate to Google Cloud for image publication");
  if (authIndex <= releaseProvenanceIndex || authIndex >= imageBuildIndex) {
    fail(`${releaseWorkflowPath} must authenticate to GCP after release artifact/provenance creation and before pushed image publication`);
  }
  if (String(authStep?.if ?? "") !== "${{ matrix.target == 'x86_64-unknown-linux-gnu' }}") {
    fail(`${releaseWorkflowPath} must authenticate to GCP for the x86_64 release image publication target`);
  }
  const resolveAuthStep = steps.find((step) => step.name === "Resolve GCP CI/CD auth secret for image publication");
  if (!String(resolveAuthStep?.run ?? "").includes("resolve-gcp-cicd-secret.sh --mode publish")) {
    fail(`${releaseWorkflowPath} must resolve image publication auth through BUC_CI_CD/legacy OIDC resolver`);
  }
  if (authStep?.with?.workload_identity_provider !== "${{ steps.gcp_publish_auth.outputs.workload_identity_provider }}" || authStep?.with?.service_account !== "${{ steps.gcp_publish_auth.outputs.service_account }}") {
    fail(`${releaseWorkflowPath} GCP auth must use resolved workload identity and service account outputs`);
  }
  const dockerAuthStep = steps.find((step) => step.name === "Configure Artifact Registry Docker auth");
  if (String(dockerAuthStep?.if ?? "") !== "${{ matrix.target == 'x86_64-unknown-linux-gnu' }}") {
    fail(`${releaseWorkflowPath} must configure Artifact Registry Docker auth for the x86_64 release image publication target`);
  }
  if (!String(dockerAuthStep?.run ?? "").includes("configure-gcp-artifact-registry-auth.sh")) {
    fail(`${releaseWorkflowPath} Docker auth step must use the checked-in Artifact Registry auth script`);
  }
  const tfvarsStep = steps.find((step) => step.name === "Write GCP image tfvars");
  if (String(tfvarsStep?.if ?? "") !== "${{ matrix.target == 'x86_64-unknown-linux-gnu' }}") {
    fail(`${releaseWorkflowPath} must write GCP image tfvars for the x86_64 release image publication target`);
  }
  const imageManifestUploadStep = steps.find((step) => step.name === "Upload Cloud image build manifest");
  if (String(imageManifestUploadStep?.with?.path ?? "").includes("gcp-image-digests.tfvars")) {
    fail(`${releaseWorkflowPath} build manifest artifact must stay inspection-only; promotion evidence carries deploy tfvars`);
  }
  const promotionIndexStep = steps.find((step) => step.name === "Write Cloud image promotion evidence index");
  if (String(promotionIndexStep?.if ?? "") !== "${{ matrix.target == 'x86_64-unknown-linux-gnu' }}") {
    fail(`${releaseWorkflowPath} must write the image promotion evidence index for the x86_64 release image publication target`);
  }
  const promotionIndexVerifyStep = steps.find((step) => step.name === "Verify Cloud image promotion evidence index");
  if (String(promotionIndexVerifyStep?.if ?? "") !== "${{ matrix.target == 'x86_64-unknown-linux-gnu' }}") {
    fail(`${releaseWorkflowPath} must verify the image promotion evidence index for the x86_64 release image publication target`);
  }
  const promotionUploadStep = steps.find((step) => step.name === "Upload Cloud image promotion evidence");
  if (String(promotionUploadStep?.if ?? "") !== "${{ matrix.target == 'x86_64-unknown-linux-gnu' }}") {
    fail(`${releaseWorkflowPath} must upload image promotion evidence for the x86_64 release image publication target`);
  }
  const promotionUploadPaths = String(promotionUploadStep?.with?.path ?? "");
  for (const requiredPath of [
    "cloud-image-build-manifest.json",
    "cloud-image-build.provenance.json",
    "gcp-image-digests.tfvars",
    "cloud-image-promotion-evidence.json",
  ]) {
    if (!promotionUploadPaths.includes(requiredPath)) {
      fail(`${releaseWorkflowPath} image promotion evidence upload is missing ${requiredPath}`);
    }
  }
  if (promotionUploadStep?.with?.name !== "cloud-release-promotion-${{ steps.version.outputs.version }}") {
    fail(`${releaseWorkflowPath} image promotion evidence artifact must have the single versioned release handoff name`);
  }
  const coreUploadStep = steps.find((step) => step.name === "Upload core release archive");
  if (coreUploadStep?.with?.name !== "cli-installer-${{ matrix.target }}") {
    fail(`${releaseWorkflowPath} Linux core archives must upload as explicit cli-installer target artifacts`);
  }
  const uploadStep = steps.find((step) => step.name === "Upload release bundle");
  const uploadPaths = String(uploadStep?.with?.path ?? "");
  const expandedBundlePattern = /dist\/releases\/bucephalus-\$\{\{\s*steps\.version\.outputs\.version\s*\}\}-\$\{\{\s*matrix\.target\s*\}\}(?:\n|$)/;
  if (expandedBundlePattern.test(uploadPaths)) {
    fail(`${releaseWorkflowPath} must not upload expanded Cloud release directories as publishable assets`);
  }
  for (const requiredPath of [
    ".tar.gz",
    ".tar.gz.sha256",
    ".provenance.json",
  ]) {
    if (!uploadPaths.includes(requiredPath)) {
      fail(`${releaseWorkflowPath} Cloud release upload is missing ${requiredPath} asset`);
    }
  }
}
const buildMacosCore = releaseJobs["build-macos-core-release"];
if (!buildMacosCore) {
  fail(`${releaseWorkflowPath} must contain build-macos-core-release job`);
} else {
  if (buildMacosCore.if) {
    fail(`${releaseWorkflowPath} build-macos-core-release must run for every product release`);
  }
  if (buildMacosCore.permissions?.contents !== "read" || buildMacosCore.permissions?.["id-token"] === "write") {
    fail(`${releaseWorkflowPath} build-macos-core-release must have contents read without OIDC token write permission`);
  }
  const matrixTargets = (buildMacosCore.strategy?.matrix?.include ?? []).map((entry) => entry.target).filter(Boolean);
  for (const requiredTarget of ["aarch64-apple-darwin", "x86_64-apple-darwin"]) {
    if (!matrixTargets.includes(requiredTarget)) {
      fail(`${releaseWorkflowPath} build-macos-core-release matrix missing macOS target ${requiredTarget}`);
    }
  }
  if (matrixTargets.some((target) => !String(target).includes("apple-darwin"))) {
    fail(`${releaseWorkflowPath} build-macos-core-release must stay macOS core-only`);
  }
  const steps = buildMacosCore.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  for (const required of [
    "Build core release archive",
    "Verify core release archive",
    "Write core release provenance",
    "Upload core release archive",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${releaseWorkflowPath} build-macos-core-release missing step: ${required}`);
    }
  }
  for (const forbidden of [
    "Build release bundle",
    "Build and inspect Cloud images",
    "Authenticate to Google Cloud for image publication",
  ]) {
    if (stepNames.includes(forbidden)) {
      fail(`${releaseWorkflowPath} build-macos-core-release must stay core-only and not gate Linux Cloud publication`);
    }
  }
  const coreUploadStep = steps.find((step) => step.name === "Upload core release archive");
  if (coreUploadStep?.with?.name !== "cli-installer-${{ matrix.target }}") {
    fail(`${releaseWorkflowPath} macOS core archives must upload as explicit cli-installer target artifacts`);
  }
}
const publishRelease = releaseJobs["publish-github-release"];
if (!publishRelease) {
  fail(`${releaseWorkflowPath} must contain publish-github-release job`);
} else {
  if (publishRelease.permissions?.contents !== "write" || publishRelease.permissions?.["id-token"] === "write") {
    fail(`${releaseWorkflowPath} publish-github-release must have contents write without OIDC token write permission`);
  }
  const steps = publishRelease.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  const publishNeeds = Array.isArray(publishRelease.needs) ? publishRelease.needs : [publishRelease.needs].filter(Boolean);
  for (const requiredNeed of ["build-linux-release", "build-macos-core-release"]) {
    if (!publishNeeds.includes(requiredNeed)) {
      fail(`${releaseWorkflowPath} publish-github-release must wait for ${requiredNeed}`);
    }
  }
  if (publishNeeds.includes("build-core-release")) {
    fail(`${releaseWorkflowPath} publish-github-release must not depend on a combined build-core-release matrix that makes Cloud images wait for macOS`);
  }
  const hasBunSetup = steps.some((step) => step.uses === "oven-sh/setup-bun@v2" && step.with?.["bun-version"] === "1.3.14");
  if (!hasBunSetup) {
    fail(`${releaseWorkflowPath} publish-github-release must install pinned Bun before running asset verifiers`);
  }
  for (const required of [
    "Write release asset index",
    "Verify release asset index",
    "Attach release archives to GitHub release",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${releaseWorkflowPath} publish-github-release missing step: ${required}`);
    }
  }
  const attachStep = steps.find((step) => step.name === "Attach release archives to GitHub release");
  if (!String(attachStep?.with?.files ?? "").includes("dist/release-assets.json")) {
    fail(`${releaseWorkflowPath} GitHub release must attach dist/release-assets.json`);
  }
  const indexStep = steps.find((step) => step.name === "Write release asset index");
  const indexRun = String(indexStep?.run ?? "");
  for (const requiredTarget of [
    "--required-core-target x86_64-unknown-linux-gnu",
    "--required-core-target aarch64-unknown-linux-gnu",
    "--required-core-target aarch64-apple-darwin",
    "--required-core-target x86_64-apple-darwin",
    "--required-cloud-target x86_64-unknown-linux-gnu",
    "--required-cloud-target aarch64-unknown-linux-gnu",
  ]) {
    if (!indexRun.includes(requiredTarget)) {
      fail(`${releaseWorkflowPath} release asset index must require target ${requiredTarget}`);
    }
  }
}

function inputValueUsesLatest(inputs) {
  return Object.values(inputs).some((input) => {
    if (typeof input?.default === "string" && /(?:^|[^A-Za-z0-9_.-])latest(?:[^A-Za-z0-9_.-]|$)/.test(input.default)) {
      return true;
    }
    return Array.isArray(input?.options) && input.options.some((option) => typeof option === "string" && /(?:^|[^A-Za-z0-9_.-])latest(?:[^A-Za-z0-9_.-]|$)/.test(option));
  });
}
if (inputValueUsesLatest(releaseWorkflow.on?.workflow_dispatch?.inputs ?? {})) {
  fail(`${releaseWorkflowPath} must not use latest as a release/image input`);
}
if (inputValueUsesLatest(deployWorkflow.on?.workflow_dispatch?.inputs ?? {})) {
  fail(`${deployWorkflowPath} must not use latest as a deploy/image input`);
}
if (/docker\s+(?:push|login)\b/.test(releaseWorkflowText)) {
  fail(`${releaseWorkflowPath} must not call docker push/login directly; use release scripts and future OIDC registry setup`);
}
if (/docker\s+(?:push|login)\b/.test(deployWorkflowText)) {
  fail(`${deployWorkflowPath} must not call docker push/login directly`);
}
if (!cloudGatesText.includes("scripts/release/verify-cloud-signing-policy.sh")) {
  fail(`${cloudGatesPath} must run the Path 2 signing policy verifier`);
}

if (!cloudImageBuildText.includes("prepare_image_context")) {
  fail(`${cloudImageBuildPath} must build from generated per-component image contexts`);
}
if (!cloudImageBuildText.includes("bucephalus-cloud/runtime-dist")) {
  fail(`${cloudImageBuildPath} must build image contexts from prebuilt runtime-dist entrypoints`);
}
for (const requiredRuntimeEntry of [
  "runtime-dist/server.js",
  "runtime-dist/db/migrate.js",
  "runtime-dist/poolController.js",
  "runtime-dist/worker.js",
  "runtime-dist/secretResolver.js",
  "runtime-dist/networkPolicyClient.js",
]) {
  if (!cloudImageBuildText.includes(requiredRuntimeEntry)) {
    fail(`${cloudImageBuildPath} must stage component-specific runtime entrypoint ${requiredRuntimeEntry}`);
  }
}
if (cloudImageBuildText.includes('"${RELEASE_DIR}/bucephalus-cloud/runtime-dist"')) {
  fail(`${cloudImageBuildPath} must not copy the entire runtime-dist tree into every image context`);
}
if (/(?:cp|copy_context_path)[^\n]+(?:release-manifest\.json|SHA256SUMS)[^\n]+\$\{context_dir\}/.test(cloudImageBuildText)) {
  fail(`${cloudImageBuildPath} must not stage release evidence files inside runtime image contexts`);
}
if (!cloudImageBuildText.includes("--cache-from \"type=registry") || !cloudImageBuildText.includes("--cache-to \"type=registry")) {
  fail(`${cloudImageBuildPath} must use registry-backed BuildKit cache for pushed image builds`);
}
if (!cloudImageBuildText.includes("docker tag \"${boundary_ref}\" \"${image_ref}\"") || !cloudImageBuildText.includes("docker push \"${image_ref}\"")) {
  fail(`${cloudImageBuildPath} must push the inspected boundary image instead of rebuilding pushed images`);
}
if (!cloudImageBuildText.includes("timings_seconds")) {
  fail(`${cloudImageBuildPath} must record per-component image build timing evidence`);
}

for (const dockerfilePath of listFiles("bucephalus-cloud/images").filter((file) => file.includes("Dockerfile."))) {
  const dockerfileText = read(dockerfilePath);
  if (dockerfileText.includes("release-inputs")) {
    fail(`${dockerfilePath} must not copy release-inputs into runtime images`);
  }
  if (/COPY[^\n]+(?:release-manifest\.json|SHA256SUMS)/.test(dockerfileText)) {
    fail(`${dockerfilePath} must not copy release evidence files into runtime images`);
  }
  if (!dockerfileText.includes("bucephalus-cloud/runtime-dist")) {
    fail(`${dockerfilePath} must copy prebuilt runtime-dist entrypoints`);
  }
  if (/COPY[^\n]+bucephalus-cloud\/runtime-dist\s+\.\/runtime-dist/.test(dockerfileText)) {
    fail(`${dockerfilePath} must not copy the entire runtime-dist tree into the image`);
  }
  if (dockerfileText.includes("bun install")) {
    fail(`${dockerfilePath} must not install package dependencies during image builds`);
  }
  if (dockerfileText.includes("bucephalus-cloud/src")) {
    fail(`${dockerfilePath} must not copy Cloud source into runtime images`);
  }
}
const componentRuntimeEntries = new Map([
  ["bucephalus-cloud/images/Dockerfile.api", ["runtime-dist/server.js"]],
  ["bucephalus-cloud/images/Dockerfile.migrations", ["runtime-dist/db/migrate.js"]],
  ["bucephalus-cloud/images/Dockerfile.pool-controller", ["runtime-dist/poolController.js"]],
  ["bucephalus-cloud/images/Dockerfile.worker", ["runtime-dist/worker.js", "runtime-dist/secretResolver.js", "runtime-dist/networkPolicyClient.js", "bin/bucephalus"]],
]);
for (const [dockerfilePath, requiredEntries] of componentRuntimeEntries) {
  const dockerfileText = read(dockerfilePath);
  for (const requiredEntry of requiredEntries) {
    if (!dockerfileText.includes(requiredEntry)) {
      fail(`${dockerfilePath} must copy only its required runtime payload ${requiredEntry}`);
    }
  }
}
const workerDockerfileText = read("bucephalus-cloud/images/Dockerfile.worker");
if (/apt-get|docker\.io/.test(workerDockerfileText)) {
  fail("worker image must not install Docker packages; it talks to the mounted host daemon through the Docker API");
}
const workerText = read("bucephalus-cloud/src/worker.ts");
if (!workerText.includes("DOCKER_SOCKET_PATH") || !workerText.includes("node:http")) {
  fail("Cloud worker cleanup must use the Docker Engine API over the mounted socket");
}
if (/runCommand\("docker"|spawn\("docker"/.test(workerText)) {
  fail("Cloud worker must not shell out to the Docker CLI");
}

const runtimePackage = JSON.parse(read("bucephalus-cloud/package.runtime.json"));
const runtimeDependencies = Object.keys(runtimePackage.dependencies ?? {}).sort();
if (JSON.stringify(runtimeDependencies) !== JSON.stringify(["postgres", "tar"])) {
  fail("bucephalus-cloud/package.runtime.json must contain only backend runtime dependencies postgres and tar");
}
for (const forbiddenRuntimeDependency of ["react", "react-dom", "vite", "@vitejs/plugin-react", "tailwindcss", "lucide-react", "recharts"]) {
  if (runtimeDependencies.includes(forbiddenRuntimeDependency)) {
    fail(`bucephalus-cloud/package.runtime.json must not include frontend dependency ${forbiddenRuntimeDependency}`);
  }
}
if (!read("bucephalus-cloud/bun.runtime.lock").includes('"postgres"') || !read("bucephalus-cloud/bun.runtime.lock").includes('"tar"')) {
  fail("bucephalus-cloud/bun.runtime.lock must lock backend runtime dependencies");
}

for (const eventName of ["pull_request", "push"]) {
  const paths = cloudCiWorkflow.on?.[eventName]?.paths ?? [];
  for (const requiredPath of [
    ".github/workflows/bucephalus-gcp-cleanup.yml",
    ".github/workflows/bucephalus-gcp-deploy.yml",
    ".github/workflows/bucephalus-release.yml",
    "docs/specs/CLOUD_DEPLOYMENT_GOAL_STATE.md",
    "docs/specs/CLOUD_PATH2_ARTIFACT_IMAGE_CI_READINESS.md",
    "docs/specs/CLOUD_PATH2_SIGNING_POLICY.json",
    "bucephalus-cloud/**",
    "bucephalus-cloud/infra/gcp/**",
    "scripts/install.sh",
    "scripts/ci/verify-cloud-release-boundary.sh",
    "scripts/deploy/**",
    "scripts/release/configure-gcp-artifact-registry-auth.sh",
    "scripts/release/verify-cloud-image-promotion-evidence-index.sh",
    "scripts/release/verify-cloud-signing-policy.sh",
    "scripts/release/write-cloud-image-promotion-evidence-index.sh",
  ]) {
    if (!paths.includes(requiredPath)) {
      fail(`${cloudCiWorkflowPath} ${eventName} paths must include ${requiredPath}`);
    }
  }
}

for (const [workflowPath, workflow] of [
  [cloudCiWorkflowPath, cloudCiWorkflow],
  [rustQualityWorkflowPath, rustQualityWorkflow],
]) {
  const pushBranches = workflow.on?.push?.branches ?? [];
  if (!pushBranches.includes("**")) {
    fail(`${workflowPath} push trigger must include all branches so migration branches run gates before release`);
  }
}

for (const requiredPackageCommand of [
  "cargo package --manifest-path Cargo.toml -p lab-core",
  "cargo package --manifest-path Cargo.toml -p lab-schemas",
  "cargo package --manifest-path Cargo.toml -p lab-provenance",
]) {
  if (!rustQualityWorkflowText.includes(requiredPackageCommand)) {
    fail(`${rustQualityWorkflowPath} must validate publishable split-crate packaging with: ${requiredPackageCommand}`);
  }
}

const cloudCiJobs = cloudCiWorkflow.jobs ?? {};
const releaseBoundaryPolicyJob = cloudCiJobs["release-boundary-policy"];
if (!releaseBoundaryPolicyJob) {
  fail(`${cloudCiWorkflowPath} must contain release-boundary-policy job`);
} else {
  const stepNames = (releaseBoundaryPolicyJob.steps ?? []).map((step) => step.name).filter(Boolean);
  for (const required of [
    "Verify release boundary policy",
    "Verify signing policy",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${cloudCiWorkflowPath} release-boundary-policy missing step: ${required}`);
    }
  }
}

for (const script of [
  "scripts/ci/cloud-gates.sh",
  "scripts/ci/verify-cloud-release-boundary.sh",
  ...listFiles("scripts/deploy").filter((file) => file.endsWith(".sh")),
  ...listFiles("scripts/release").filter((file) => file.endsWith(".sh")),
]) {
  if ((statSync(join(root, script)).mode & 0o111) === 0) {
    fail(`${script} must be executable for CI/release entrypoint use`);
  }
}

const bootstrapScriptText = read("scripts/deploy/bootstrap-gcp-github-oidc.sh");
for (const required of [
  "BUC_CI_CD",
  "BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER",
  "BUCEPHALUS_GCP_SERVICE_ACCOUNT",
  "BUCEPHALUS_GCP_DEPLOY_WORKLOAD_IDENTITY_PROVIDER",
  "BUCEPHALUS_GCP_DEPLOY_SERVICE_ACCOUNT",
  "BUCEPHALUS_TERRAFORM_BACKEND_BUCKET",
  "BUCEPHALUS_TERRAFORM_BACKEND_PREFIX",
  "BUCEPHALUS_GCP_PROJECT_ID",
  "BUCEPHALUS_GCP_REGION",
  "BUCEPHALUS_DEPLOYMENT_ENVIRONMENT",
  "BUCEPHALUS_GCP_RESOURCE_PREFIX",
  "BUCEPHALUS_GOOGLE_OAUTH_CLIENT_ID",
  "BUCEPHALUS_CLOUDFLARE_WORKER_NAME",
  "BUCEPHALUS_IMAGE_REPOSITORY",
  "BUCEPHALUS_BUN_BASE_IMAGE",
  "gh variable set",
  "roles/iam.workloadIdentityUser",
  "roles/artifactregistry.writer",
  "roles/run.admin",
  "roles/cloudsql.admin",
]) {
  if (!bootstrapScriptText.includes(required)) {
    fail(`scripts/deploy/bootstrap-gcp-github-oidc.sh must configure ${required}`);
  }
}
if (!/APPLY="false"/.test(bootstrapScriptText) || !/--apply/.test(bootstrapScriptText)) {
  fail("scripts/deploy/bootstrap-gcp-github-oidc.sh must default to dry-run and require --apply for mutation");
}

if (!/resource\s+"google_artifact_registry_repository"\s+"cloud"/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} must declare the first-cloud Artifact Registry repository`);
}
if (!/resource\s+"google_project_iam_member"\s+"runner_artifact_registry_reader"\s*\{[\s\S]*role\s*=\s*"roles\/artifactregistry\.reader"[\s\S]*member\s*=\s*"serviceAccount:\$\{google_service_account\.runner\.email\}"/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} must grant GCE runners Artifact Registry reader for digest-pinned run image pulls`);
}
if (!/google_project_iam_member\.runner_artifact_registry_reader/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} pool controller must depend on runner Artifact Registry reader IAM before provisioning VMs`);
}
for (const requiredService of [
  "iam.googleapis.com",
  "iamcredentials.googleapis.com",
  "sts.googleapis.com",
]) {
  if (!gcpInfraText.includes(requiredService)) {
    fail(`${gcpInfraPath} must include ${requiredService} in the required service set`);
  }
}
if (!/backend\s+"gcs"\s*\{\s*\}/.test(read("bucephalus-cloud/infra/gcp/versions.tf"))) {
  fail("bucephalus-cloud/infra/gcp/versions.tf must declare a partial GCS Terraform backend for CI/CD deploy state");
}
if (!/cleanup_policies\s*\{[\s\S]*id\s*=\s*"delete-untagged-after-30-days"[\s\S]*action\s*=\s*"DELETE"[\s\S]*tag_state\s*=\s*"UNTAGGED"[\s\S]*older_than\s*=\s*"2592000s"[\s\S]*\}/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} must keep the registry cleanup policy limited to untagged images older than 30 days`);
}
if (/tag_state\s*=\s*"TAGGED"/.test(gcpInfraText) || /tag_state\s*=\s*"ANY"/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} must not delete tagged image versions without a rollback-preserving policy`);
}
if (!/variable\s+"oauth_user_client_id"/.test(gcpVariablesText)) {
  fail(`${gcpVariablesPath} must require a first-class user OAuth client ID`);
}
if (!/variable\s+"deploy_control_plane_services"/.test(gcpVariablesText)) {
  fail(`${gcpVariablesPath} must support substrate-only applies before real image digests exist`);
}
if (/validation\s*\{[\s\S]*?condition\s*=[^\n]*(?:deploy_control_plane_services|deploy_api_services|deploy_pool_controller)/.test(gcpVariablesText)) {
  fail(`${gcpVariablesPath} variable validation must not reference other variables; use deploy preflight preconditions for cross-variable checks`);
}
if (!/resource\s+"terraform_data"\s+"deploy_input_preflight"/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} must keep cross-variable deploy input checks in a Terraform preflight resource`);
}
if (!/BUCEPHALUS_CLOUD_OAUTH_AUDIENCE[\s\S]*var\.oauth_user_client_id/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} must inject the user OAuth client ID as the API OAuth audience`);
}
if (!/variable\s+"oauth_jwks_url"/.test(gcpVariablesText) || !/https:\/\/www\.googleapis\.com\/oauth2\/v3\/certs/.test(gcpVariablesText)) {
  fail(`${gcpVariablesPath} must require an explicit Google-compatible OAuth JWKS URL`);
}
if (!/BUCEPHALUS_CLOUD_OAUTH_JWKS_URL[\s\S]*var\.oauth_jwks_url/.test(gcpInfraText)) {
  fail(`${gcpInfraPath} must inject the OAuth JWKS URL into the API service`);
}
if (!/@sha256:0\{64\}\$/.test(gcpVariablesText)) {
  fail(`${gcpVariablesPath} must reject all-zero placeholder image digests`);
}

for (const script of [
  "scripts/release/build-buc-release.sh",
  "scripts/release/build-cloud-images.sh",
  "scripts/release/configure-gcp-artifact-registry-auth.sh",
  "scripts/release/resolve-cloud-release-artifacts.sh",
  "scripts/release/verify-cloud-base-image-policy.sh",
    "scripts/release/verify-cloud-image-build-manifest.sh",
    "scripts/release/verify-cloud-image-publish-inputs.sh",
    "scripts/release/write-cloud-image-promotion-evidence-index.sh",
    "scripts/release/verify-cloud-image-promotion-evidence-index.sh",
    "scripts/release/write-cloud-release-provenance.sh",
  "scripts/release/verify-cloud-release-provenance.sh",
  "scripts/release/verify-cloud-registry-auth-boundary.sh",
  "scripts/release/verify-cloud-signing-policy.sh",
  "scripts/release/verify-gcp-image-promotion-evidence.sh",
  "scripts/release/verify-gcp-image-tfvars.sh",
  "scripts/release/write-core-release-provenance.sh",
  "scripts/release/verify-core-release-provenance.sh",
  "scripts/release/write-release-asset-index.sh",
  "scripts/release/verify-release-asset-index.sh",
  "scripts/release/write-gcp-image-tfvars.sh",
  "bucephalus-cloud/images/Dockerfile.worker",
]) {
  const text = read(script);
  if (/verify-cloud-image-publish-inputs\.sh/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must validate image publish inputs before buildx runs`);
  }
  if (/verify-cloud-base-image-policy\.sh/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must validate the approved base image policy before buildx runs`);
  }
  if (/verify-cloud-registry-auth-boundary\.sh/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must validate registry auth readiness before buildx runs`);
  }
  if (/release bundle is missing \.dockerignore image context guard/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must require the release bundle image context guard`);
  }
  if (/--require-ready/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must require configured registry auth before pushed buildx runs`);
  }
  if (/image_context_ignore/.test(text) === false && script === "scripts/release/build-buc-release.sh") {
    fail(`${script} must record the release image context ignore file in the manifest`);
  }
  if ((/cloud_runtime_bun/.test(text) === false || /cloud_runtime_package/.test(text) === false || /cloud_runtime_dist/.test(text) === false) && script === "scripts/release/build-buc-release.sh") {
    fail(`${script} must record the runtime-only image package manifest, lockfile, and bundled runtime output`);
  }
  if (/BUCEPHALUS_RELEASE_SKIP_CLOUD_CHECKS/.test(text) === false && script === "scripts/release/build-buc-release.sh") {
    fail(`${script} must support skipping repeated Cloud checks after a prior CI gate`);
  }
  if ((/--core-bin/.test(text) === false || /CORE_BIN_INPUT/.test(text) === false) && script === "scripts/release/build-buc-release.sh") {
    fail(`${script} must support reusing a verified prebuilt Core binary`);
  }
  if (/\.dockerignore is missing required image context exclusion/.test(text) === false && script === "scripts/release/verify-buc-release.sh") {
    fail(`${script} must verify required image context exclusions`);
  }
  if (/gcloud auth configure-docker/.test(text) === false && script === "scripts/release/configure-gcp-artifact-registry-auth.sh") {
    fail(`${script} must configure Docker with the gcloud Artifact Registry credential helper`);
  }
  if (/GOOGLE_GHA_CREDS_PATH/.test(text) === false && (script === "scripts/release/configure-gcp-artifact-registry-auth.sh" || script === "scripts/release/verify-cloud-registry-auth-boundary.sh")) {
    fail(`${script} must distinguish generated GitHub OIDC credentials from manual GOOGLE_APPLICATION_CREDENTIALS`);
  }
  if (/BUCEPHALUS_GCP_REGISTRY_AUTH_READY=true/.test(text) === false && script === "scripts/release/configure-gcp-artifact-registry-auth.sh") {
    fail(`${script} must set the registry auth ready marker after Docker auth is configured`);
  }
  if (/GOOGLE_APPLICATION_CREDENTIALS/.test(text) === false && script === "scripts/release/configure-gcp-artifact-registry-auth.sh") {
    fail(`${script} must reject static credential surfaces`);
  }
  if (/cloud-release-promotion-\$\{VERSION\}/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must resolve the single versioned release promotion artifact`);
  }
  if (/cloud-runner-release-\$\{VERSION\}-x86_64-unknown-linux-gnu/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must resolve the versioned x86_64 Linux Cloud release artifact`);
  }
  if (/status=completed/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must inspect completed release workflow runs when resolving releases`);
  }
  if (/cloud-ui-assets-\$\{VERSION\}/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must resolve versioned Cloud UI assets by release version`);
  }
  if (/pushed images require an approved base image policy entry/.test(text) === false && script === "scripts/release/verify-cloud-base-image-policy.sh") {
    fail(`${script} must block pushed images until the base digest is approved`);
  }
  if (text.includes("const digestRef = /^[^\\s]+@sha256:[a-f0-9]{64}$/;") === false && script === "scripts/release/verify-cloud-base-image-policy.sh") {
    fail(`${script} must parse digest-addressed base images without over-escaped whitespace classes`);
  }
  if (/bucephalus_cloud_base_image_policy_v1/.test(text) === false && script === "scripts/release/verify-cloud-base-image-policy.sh") {
    fail(`${script} must validate the base image policy schema`);
  }
  if (/--base-image must be digest-addressed/.test(text) === false && script === "scripts/release/verify-cloud-image-publish-inputs.sh") {
    fail(`${script} must enforce digest-addressed base images`);
  }
  if (/--base-image must not be a tag, URL, or latest reference/.test(text) === false && script === "scripts/release/verify-cloud-image-publish-inputs.sh") {
    fail(`${script} must reject mutable or URL-shaped base image inputs`);
  }
  if (/GCP Artifact Registry/.test(text) === false && script === "scripts/release/verify-cloud-image-publish-inputs.sh") {
    fail(`${script} must require GCP Artifact Registry shape for pushed image publication`);
  }
  if (/docker\.io/.test(text) === false && script === "scripts/release/verify-cloud-image-publish-inputs.sh") {
    fail(`${script} must reject broad public/default image publication destinations`);
  }
  if (/BUCEPHALUS_GCP_REGISTRY_AUTH_READY=true/.test(text) === false && script === "scripts/release/verify-cloud-registry-auth-boundary.sh") {
    fail(`${script} must cleanly block pushed images until OIDC registry auth is configured`);
  }
  if (/GOOGLE_APPLICATION_CREDENTIALS/.test(text) === false && script === "scripts/release/verify-cloud-registry-auth-boundary.sh") {
    fail(`${script} must reject static credential surfaces for pushed publication`);
  }
  if (/bucephalus_cloud_path2_signing_policy_v1/.test(text) === false && script === "scripts/release/verify-cloud-signing-policy.sh") {
    fail(`${script} must validate the Path 2 signing policy schema`);
  }
  if (/required_before_signed_status missing/.test(text) === false && script === "scripts/release/verify-cloud-signing-policy.sh") {
    fail(`${script} must require explicit blockers before signed status is allowed`);
  }
  if (/boundary_verified/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must record local image boundary verification in image manifests`);
  }
  if (/dockerfile.*sha256/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must record per-component Dockerfile digests in image manifests`);
  }
  if (/path_stats_json/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must record per-component build context file inventories in image manifests`);
  }
  if (/image_size_bytes/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must record built Docker image sizes in image manifests`);
  }
  if (/release-size-report\.json/.test(text) === false && script === "scripts/release/build-buc-release.sh") {
    fail(`${script} must include a release payload size report in cloud release archives`);
  }
  if (/bucephalus-worker-runner/.test(text) === false && script === "scripts/release/build-buc-release.sh") {
    fail(`${script} must build the narrow Cloud worker runner binary`);
  }
  if (/bucephalus-worker-runner/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must stage the narrow worker runner binary for worker images`);
  }
  if (/ln -sf \/usr\/local\/bin\/bucephalus-worker-runner \/usr\/local\/bin\/bucephalus/.test(text) === false && script === "bucephalus-cloud/images/Dockerfile.worker") {
    fail(`${script} must route the default Cloud worker core command to the narrow worker runner binary`);
  }
  if (/artifacts\.size_report/.test(text) === false && script === "scripts/release/verify-buc-release.sh") {
    fail(`${script} must verify the release payload size report`);
  }
  if (/artifacts\.worker_runner_binary/.test(text) === false && script === "scripts/release/verify-buc-release.sh") {
    fail(`${script} must verify the narrow Cloud worker runner binary`);
  }
  if (/boundary_verified must be true/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must require local image boundary verification evidence`);
  }
  if (/GCP Artifact Registry component repository/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must require pushed manifests to target GCP Artifact Registry component repositories`);
  }
  if (/image_id must be a sha256 image id/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must require Docker sha256 image IDs`);
  }
  if (/metadata_file must be/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must reject local filesystem paths in image metadata evidence`);
  }
  if (/metadata_file containerimage\.digest does not match manifest/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must verify image metadata files when they are present`);
  }
  if (/\.iid does not match manifest image_id/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must verify image iid files when they are present`);
  }
  if (/\.boundary\.iid does not match manifest boundary_image_id/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must verify boundary iid files when they are present`);
  }
  if (/tag_ref must use image_repository/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must tie image tag refs to their component repository`);
  }
  if (/boundary_image_ref must be the pushed tag_ref boundary check/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must tie pushed boundary image refs to pushed tag refs`);
  }
  if (/image_context\.sha256 must be a lowercase sha256 digest/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must require image build-context digest evidence`);
  }
  if (/dockerfile\.sha256 must be a lowercase sha256 digest/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must require per-component Dockerfile digest evidence`);
  }
  if (/build_context\.files must match file_count/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must verify per-component build context file inventories`);
  }
  if (/must not include the Rust core binary/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must keep the Rust core binary out of non-worker images`);
  }
  if (/must not include the full Rust CLI binary/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must keep the full Rust CLI binary out of worker image contexts`);
  }
  if (/image_size_bytes must be absent, null, or a positive integer/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must validate image size evidence`);
  }
  if (/builder\.github_sha must match release\.git_sha/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must tie GitHub Actions image builder identity to the release git sha`);
  }
  if (/source_release\.git_sha must match release\.git_sha/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must record source release identity for artifact-driven image publication`);
  }
  if (/local builder must not claim/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must reject local image manifests that claim GitHub Actions identity fields`);
  }
  if (/release\.manifest_sha256 does not match release manifest/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must tie image manifests to the release manifest when a release is provided`);
  }
  if (/image_context\.sha256 does not match release \.dockerignore/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must tie image manifests to the release Docker context guard when a release is provided`);
  }
  if (/dockerfile\.sha256 does not match release Dockerfile/.test(text) === false && script === "scripts/release/verify-cloud-image-build-manifest.sh") {
    fail(`${script} must tie image manifests to release Dockerfiles when a release is provided`);
  }
  if (/const isGithubActions = process\.env\.GITHUB_ACTIONS === "true"/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must only write GitHub image builder fields for GitHub Actions runs`);
  }
  if (!(/verify-cloud-image-build-manifest\.sh" "\$\{MANIFEST_PATH\}" --release "\$\{RELEASE_INPUT\}"/.test(text) || /verify_manifest_args=\("\$\{MANIFEST_PATH\}" --release "\$\{RELEASE_INPUT\}"\)/.test(text)) && script === "scripts/release/build-cloud-images.sh") {
    fail(`${script} must verify generated image build manifests against the release input`);
  }
  if (/pushed=true/.test(text) === false && script === "scripts/release/write-gcp-image-tfvars.sh") {
    fail(`${script} must refuse local image manifests for deploy tfvars`);
  }
  if (/verify-gcp-image-tfvars\.sh/.test(text) === false && script === "scripts/release/write-gcp-image-tfvars.sh") {
    fail(`${script} must verify generated deploy tfvars`);
  }
  if (/unexpected tfvars variable/.test(text) === false && script === "scripts/release/verify-gcp-image-tfvars.sh") {
    fail(`${script} must reject deploy tfvars with unexpected variables`);
  }
  if (/does not match image manifest immutable ref/.test(text) === false && script === "scripts/release/verify-gcp-image-tfvars.sh") {
    fail(`${script} must tie deploy tfvars to the pushed image manifest`);
  }
  if (/deploy tfvars image repositories must share one GCP Artifact Registry family/.test(text) === false && script === "scripts/release/verify-gcp-image-tfvars.sh") {
    fail(`${script} must keep deploy tfvars within one GCP Artifact Registry repository family`);
  }
  if (/worker_image_digest/.test(text) === false && script === "scripts/release/verify-gcp-image-tfvars.sh") {
    fail(`${script} must require the worker image digest as a deploy input`);
  }
  if (/must use the \$\{component\} image_repository/.test(text) === false && script === "scripts/release/write-gcp-image-tfvars.sh") {
    fail(`${script} must write only component repository digest refs`);
  }
  if (/promotion evidence requires a pushed image manifest/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must reject local image manifests as promotion evidence`);
  }
  if (/does not match between manifest and provenance/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must tie image manifest and image-build provenance together`);
  }
  if (/image provenance image_context does not match image manifest/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must tie image context digest evidence between manifest and provenance`);
  }
  if (/dockerfile does not match between manifest and provenance/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must tie Dockerfile digest evidence between manifest and provenance`);
  }
  if (/promotion tfvars image repositories must share one GCP Artifact Registry family/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must keep promotion tfvars within one GCP Artifact Registry repository family`);
  }
  if (/\["worker_image_digest", "worker"\]/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must require the worker image digest as a promotion input`);
  }
  if (/does not match image provenance immutable ref/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must tie promotion tfvars to image-build provenance immutable refs`);
  }
  if (/verify-gcp-image-promotion-evidence\.sh/.test(text) === false && script === "scripts/release/write-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must verify the complete pushed-image promotion evidence before indexing it`);
  }
  if (/bucephalus_cloud_image_promotion_evidence_index_v1/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must validate the image promotion evidence index schema`);
  }
  if (/index_sha256 does not match index content/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must verify the image promotion evidence index self digest`);
  }
  if (/evidence must contain exactly image_manifest, image_provenance, and tfvars/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must reject unexpected image promotion evidence entries`);
  }
  if (/repository_family must be a GCP Artifact Registry repository family/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must require an indexed GCP Artifact Registry repository family`);
  }
  if (/must use repository_family and end with the component repository/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must tie deploy image repositories to the indexed repository family`);
  }
  if (/\["api", "pool-controller", "migrations", "worker"\]/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must index the worker image alongside the control-plane images`);
  }
  if (/\(api\|pool-controller\|migrations\|worker\)/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must accept worker component repositories in indexed GCP image evidence`);
  }
  if (/image promotion evidence index deploy images must share one GCP Artifact Registry family/.test(text) === false && script === "scripts/release/write-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must write only one deploy repository family into the promotion evidence index`);
  }
  if (/verify-gcp-image-promotion-evidence\.sh/.test(text) === false && script === "scripts/release/verify-cloud-image-promotion-evidence-index.sh") {
    fail(`${script} must recheck the complete promotion evidence when referenced files are present`);
  }
  if (/signature\.status must be unsigned/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must reject provenance that claims signing before a signing boundary exists`);
  }
  if (/boundary_verified must be true/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must require image boundary evidence in image build provenance`);
  }
  if (/metadata_file must be/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must reject local filesystem paths in image provenance metadata evidence`);
  }
  if (/tag_ref must use image_repository/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must tie image provenance tag refs to their component repository`);
  }
  if (/GCP Artifact Registry component repository/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must require pushed provenance to target GCP Artifact Registry component repositories`);
  }
  if (/checkArtifactPath\(provenance\.image_build\.manifest_path, "image_build\.manifest_path"\)/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must reject absolute image manifest paths in image build provenance`);
  }
  if (/builder\.github_sha must match release\.git_sha/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must tie GitHub Actions builder identity to the release git sha`);
  }
  if (/source_release\.git_sha must match release\.git_sha/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must record source release identity for artifact-driven image provenance`);
  }
  if (/release\.manifest_sha256 does not match release manifest/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must tie provenance manifest digests to the release manifest when a release is provided`);
  }
  if (/release\.archive_sha256 does not match release archive/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must tie provenance archive digests to the release archive when a release is provided`);
  }
  if (/local builder must not claim/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must reject local provenance that claims GitHub Actions identity fields`);
  }
  if (/materials\.lockfiles\.\$\{name\}\.path/.test(text) === false && script === "scripts/release/verify-cloud-release-provenance.sh") {
    fail(`${script} must validate material paths in Cloud release provenance`);
  }
  if (/artifactPath\(imageManifestPath, "image_build\.manifest_path"\)/.test(text) === false && script === "scripts/release/write-cloud-release-provenance.sh") {
    fail(`${script} must write artifact-local image manifest paths in image build provenance`);
  }
  if (/verify-cloud-release-provenance\.sh" "\$\{OUT_PATH\}" --release "\$\{RELEASE_INPUT\}"/.test(text) === false && script === "scripts/release/write-cloud-release-provenance.sh") {
    fail(`${script} must verify generated provenance against the release input`);
  }
  if (/const isGithubActions = process\.env\.GITHUB_ACTIONS === "true"/.test(text) === false && script === "scripts/release/write-cloud-release-provenance.sh") {
    fail(`${script} must only write GitHub builder fields for GitHub Actions runs`);
  }
  if (/signature\.status must be unsigned/.test(text) === false && script === "scripts/release/verify-core-release-provenance.sh") {
    fail(`${script} must reject core provenance that claims signing before a signing boundary exists`);
  }
  if (/builder\.github_sha must match release\.git_sha/.test(text) === false && script === "scripts/release/verify-core-release-provenance.sh") {
    fail(`${script} must tie GitHub Actions builder identity to the core release git sha`);
  }
  if (/release\.manifest_sha256 does not match release manifest/.test(text) === false && script === "scripts/release/verify-core-release-provenance.sh") {
    fail(`${script} must tie core provenance manifest digests to the release manifest when a release is provided`);
  }
  if (/release\.archive_sha256 does not match release archive/.test(text) === false && script === "scripts/release/verify-core-release-provenance.sh") {
    fail(`${script} must tie core provenance archive digests to the release archive when a release is provided`);
  }
  if (/local builder must not claim/.test(text) === false && script === "scripts/release/verify-core-release-provenance.sh") {
    fail(`${script} must reject local core provenance that claims GitHub Actions identity fields`);
  }
  if (/const isGithubActions = process\.env\.GITHUB_ACTIONS === "true"/.test(text) === false && script === "scripts/release/write-core-release-provenance.sh") {
    fail(`${script} must only write GitHub builder fields for GitHub Actions runs`);
  }
  if (/verify-core-release-provenance\.sh" "\$\{OUT_PATH\}" --release "\$\{RELEASE_INPUT\}"/.test(text) === false && script === "scripts/release/write-core-release-provenance.sh") {
    fail(`${script} must verify generated core provenance against the release input`);
  }
  if (/malformed bundled checksum line/.test(text) === false && (script === "scripts/release/verify-core-release.sh" || script === "scripts/release/verify-buc-release.sh")) {
    fail(`${script} must reject malformed bundled SHA256SUMS records`);
  }
  if (/SHA256SUMS is missing bundled file/.test(text) === false && (script === "scripts/release/verify-core-release.sh" || script === "scripts/release/verify-buc-release.sh")) {
    fail(`${script} must require SHA256SUMS to cover every bundled file`);
  }
  if (/SHA256SUMS references unexpected file/.test(text) === false && (script === "scripts/release/verify-core-release.sh" || script === "scripts/release/verify-buc-release.sh")) {
    fail(`${script} must reject SHA256SUMS records for files outside the bundle`);
  }
  if (/bucephalus_release_asset_index_v1/.test(text) === false && script === "scripts/release/verify-release-asset-index.sh") {
    fail(`${script} must validate the release asset index schema`);
  }
  if (/verify-release-asset-index\.sh/.test(text) === false && script === "scripts/release/write-release-asset-index.sh") {
    fail(`${script} must verify the release asset index after writing it`);
  }
  if (/verify-core-release-provenance\.sh" "\$\{provenance\}" --release "\$\{archive\}"/.test(text) === false && script === "scripts/release/write-release-asset-index.sh") {
    fail(`${script} must verify core release provenance against each indexed archive`);
  }
  if (/verify-cloud-release-provenance\.sh" "\$\{provenance\}" --release "\$\{archive\}"/.test(text) === false && script === "scripts/release/write-release-asset-index.sh") {
    fail(`${script} must verify cloud release provenance against each indexed archive`);
  }
  if (/missing required \$\{kind\} release asset target/.test(text) === false && script === "scripts/release/write-release-asset-index.sh") {
    fail(`${script} must reject partial release target matrices when required targets are provided`);
  }
  if (/missing required \$\{kind\} release asset target/.test(text) === false && script === "scripts/release/verify-release-asset-index.sh") {
    fail(`${script} must verify required release target matrices`);
  }
  if (/must be a stable artifact-local path/.test(text) === false && script === "scripts/release/verify-release-asset-index.sh") {
    fail(`${script} must reject absolute or traversal paths in release asset indexes`);
  }
  if (/must be a stable artifact-local path/.test(text) === false && script === "scripts/release/write-release-asset-index.sh") {
    fail(`${script} must write only artifact-local release asset index paths`);
  }
}

if (!/malformed checksum file/.test(installScriptText) || !/\$expected  \$asset/.test(installScriptText)) {
  fail(`${installScriptPath} must verify exact single-record archive checksum files`);
}

for (const dockerfile of listFiles("bucephalus-cloud/images")) {
  if (!dockerfile.includes("/Dockerfile.")) {
    continue;
  }
  const text = read(dockerfile);
  if (!text.includes("FROM ${BUCEPHALUS_BUN_BASE_IMAGE}")) {
    fail(`${dockerfile} must use the digest-pinned base image build arg`);
  }
  // Release identity is build provenance, not runtime configuration: every
  // component must self-report its release so version skew is observable.
  const allowedEnvLines = new Set([
    'ENV BUCEPHALUS_RELEASE_VERSION="${BUCEPHALUS_RELEASE_VERSION}"',
    'ENV BUCEPHALUS_RELEASE_GIT_SHA="${BUCEPHALUS_RELEASE_GIT_SHA}"',
  ]);
  for (const line of text.split("\n")) {
    if (/^ENV\s+/.test(line) && !allowedEnvLines.has(line.trim())) {
      fail(`${dockerfile} must not bake runtime configuration with ENV (got: ${line.trim()})`);
    }
  }
  for (const required of allowedEnvLines) {
    if (!text.includes(required)) {
      fail(`${dockerfile} must bake release identity for skew detection: ${required}`);
    }
  }
  for (const forbidden of [
    /DATABASE_URL/,
    /BUCEPHALUS_CLOUD_WORKER_TOKEN/,
    /GOOGLE_APPLICATION_CREDENTIALS/,
    /TAILSCALE_AUTHKEY/,
    /\.env(?:\.example)?\b/,
    /\blatest\b/,
  ]) {
    if (forbidden.test(text)) {
      fail(`${dockerfile} contains forbidden image-boundary content matching ${forbidden}`);
    }
  }
}

const baseImagePolicy = JSON.parse(read("bucephalus-cloud/images/base-image-policy.json"));
if (baseImagePolicy.schema_version !== "bucephalus_cloud_base_image_policy_v1") {
  fail("bucephalus-cloud/images/base-image-policy.json must use bucephalus_cloud_base_image_policy_v1");
}
if (!baseImagePolicy.requirements?.pushed_images_require_approved_base) {
  fail("base image policy must require approved bases for pushed image publication");
}
for (const entry of [
  ...(baseImagePolicy.approved_base_images ?? []),
  ...(baseImagePolicy.candidate_base_images ?? []),
]) {
  if (typeof entry.image !== "string" || !/@sha256:[a-f0-9]{64}$/.test(entry.image) || /\blatest\b/.test(entry.image)) {
    fail("base image policy entries must be digest-addressed and non-latest");
  }
  if (typeof entry.source_tag !== "string" || entry.source_tag.includes("@sha256:") || /\blatest\b/.test(entry.source_tag)) {
    fail("base image policy entries must record a non-latest source tag");
  }
  if (entry.registry_response?.docker_content_digest !== entry.image.split("@")[1]) {
    fail("base image policy entries must tie registry response digest to image ref");
  }
  const platforms = new Set((entry.platforms ?? []).map((platform) => `${platform.os}/${platform.architecture}`));
  for (const requiredPlatform of ["linux/amd64", "linux/arm64"]) {
    if (!platforms.has(requiredPlatform)) {
      fail(`base image policy entries must record ${requiredPlatform} platform evidence`);
    }
  }
}

const signingPolicy = JSON.parse(read("docs/specs/CLOUD_PATH2_SIGNING_POLICY.json"));
if (signingPolicy.schema_version !== "bucephalus_cloud_path2_signing_policy_v1") {
  fail("docs/specs/CLOUD_PATH2_SIGNING_POLICY.json must use bucephalus_cloud_path2_signing_policy_v1");
}
if (signingPolicy.policy_status !== "unsigned_until_signing_boundary_configured") {
  fail("signing policy must keep provenance unsigned until a signing boundary is configured");
}
if (!signingPolicy.allowed_unsigned_schemas?.includes("bucephalus_cloud_image_promotion_evidence_index_v1")) {
  fail("signing policy must explicitly allow the unsigned image promotion evidence index schema");
}
for (const status of ["signed", "verified", "keyless", "cosign"]) {
  if (!signingPolicy.forbidden_signature_statuses?.includes(status)) {
    fail(`signing policy must forbid premature signature status: ${status}`);
  }
}

const allowedDeployProviderPayloads = new Set([
  "bucephalus-cloud/deploy/provider/gcp/gce-provider-common.js",
  "bucephalus-cloud/deploy/provider/gcp/provision-runner-vm.js",
  "bucephalus-cloud/deploy/provider/gcp/reap-runner-vm.js",
]);
const deployFiles = listFiles("bucephalus-cloud/deploy");
for (const file of deployFiles) {
  if (!file.endsWith(".md") && !allowedDeployProviderPayloads.has(file)) {
    fail(`retired non-Markdown deploy payload is present: ${file}`);
  }
}

const provisionRunnerVmText = read("bucephalus-cloud/deploy/provider/gcp/provision-runner-vm.js");
if (!provisionRunnerVmText.includes("projects/cos-cloud/global/images/family/cos-stable")) {
  fail("GCE runner provisioning must default to Container-Optimized OS with Docker preinstalled");
}
if (/python3/.test(provisionRunnerVmText)) {
  fail("GCE runner startup must not require python3 on the host boot image");
}
if (!provisionRunnerVmText.includes("ensure_host_dependencies")) {
  fail("GCE runner startup must assert the boot image provides curl, docker, and iptables");
}
if (/apt-get/.test(provisionRunnerVmText)) {
  fail("GCE runner startup must not install distro packages; the boot image provides the host contract");
}
if (!provisionRunnerVmText.includes('"google-logging-enabled"')) {
  fail("GCE runner provisioning must enable COS logging so worker container logs reach Cloud Logging");
}
if (!/const workerImageFallback = optionalEnv\("BUCEPHALUS_GCP_RUNNER_IMAGE", ""\);[\s\S]*workerImageForRequest/.test(provisionRunnerVmText)) {
  fail("GCE runner provisioning must take worker image from request state and keep BUCEPHALUS_GCP_RUNNER_IMAGE as a fallback only");
}
if (!provisionRunnerVmText.includes('onHostMaintenance: "MIGRATE"')) {
  fail("GCE runner provisioning must use onHostMaintenance=MIGRATE for default non-preemptible E2 runners");
}
if (provisionRunnerVmText.includes("/opt/bucephalus")) {
  fail("GCE runner startup must not write under /opt/bucephalus because COS has a read-only root filesystem");
}
if (!provisionRunnerVmText.includes("export DOCKER_CONFIG=/var/lib/bucephalus/docker-config")) {
  fail("GCE runner startup must use a writable Docker config path on COS");
}
if (!provisionRunnerVmText.includes("install -d -m 0700 -o 1000 -g 1000 /var/lib/bucephalus/docker-config")) {
  fail("GCE runner startup must create Docker config with worker ownership");
}
if (!provisionRunnerVmText.includes("chown -R 1000:1000 /var/lib/bucephalus/docker-config")) {
  fail("GCE runner startup must hand Docker auth config ownership to the worker after docker login");
}
if (!provisionRunnerVmText.includes("BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH=metadata")) {
  fail("GCE runner workers must resolve GCP Secret Manager refs through metadata auth");
}
if (!provisionRunnerVmText.includes("BUCEPHALUS_ACCOUNT_ID=cloud-runner")) {
  fail("GCE runner workers must set a stable Core account id for headless execution");
}
if (!provisionRunnerVmText.includes("USER=bucephalus") || !provisionRunnerVmText.includes("HOME=/var/lib/bucephalus")) {
  fail("GCE runner workers must set headless USER and HOME for Core compatibility");
}
if (!provisionRunnerVmText.includes("DOCKER_CONFIG=/var/lib/bucephalus/docker-config")) {
  fail("GCE runner workers must receive the writable Docker config for registry pre-pulls");
}
if (provisionRunnerVmText.includes("BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD") || provisionRunnerVmText.includes("/usr/local/bin/gcloud:ro")) {
  fail("GCE runner workers must not depend on a bind-mounted gcloud executable for secret resolution");
}
if (provisionRunnerVmText.includes("/usr/local/bin/bucephalus-cloud-network-policy:ro")) {
  fail("GCE runner workers must use the worker image network policy client, not a bind-mounted executable");
}
if (!provisionRunnerVmText.includes("nohup bash /var/lib/bucephalus/bin/network-policy-daemon")) {
  fail("GCE runner startup must invoke the host network policy daemon through bash for COS stateful paths");
}
if (provisionRunnerVmText.includes("getent ahostsv4") && !provisionRunnerVmText.includes("command -v getent")) {
  fail("GCE runner network policy daemon must not assume getent exists on COS");
}
if (!provisionRunnerVmText.includes("https://dns.google/resolve?name=$host&type=A")) {
  fail("GCE runner network policy daemon must include a curl-based DNS fallback for COS");
}
if (provisionRunnerVmText.includes("(\\\\.[0-9]+){3}")) {
  fail("GCE runner network policy daemon must avoid awk interval regex syntax on COS");
}
if (!gcpVariablesText.includes("projects/cos-cloud/global/images/family/cos-stable")) {
  fail(`${gcpVariablesPath} runner_gce_boot_image must default to Container-Optimized OS`);
}

const poolControllerDockerfileText = read("bucephalus-cloud/images/Dockerfile.pool-controller");
if (!poolControllerDockerfileText.includes("bucephalus-cloud/deploy/provider/gcp")) {
  fail("pool-controller image must include GCP provider command payloads used by command secrets");
}
if (!/await prePullRunImages\(config, claim\);\s*await applyRuntimeNetworkPolicy\(config, claim, materialized\);/.test(workerText)) {
  fail("Cloud worker must pre-pull package images before applying runtime network policy");
}
if (!workerText.includes("X-Registry-Auth")) {
  fail("Cloud worker Docker pre-pull must pass registry auth from Docker config");
}
const buildCloudImagesText = read("scripts/release/build-cloud-images.sh");
if (!buildCloudImagesText.includes("bucephalus-cloud/deploy/provider/gcp")) {
  fail("cloud image build context must include GCP provider command payloads for pool-controller");
}

if (failures.length > 0) {
  console.error("Cloud release boundary policy failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log("cloud release boundary policy passed");
JS

resolved_version="$(scripts/release/resolve-release-version.sh | awk -F= '$1 == "version" { print $2 }')"
if [[ ! "${resolved_version}" =~ ^[0-9]+[.][0-9]+[.][0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "release version resolver did not return a semver-like version: ${resolved_version}" >&2
  exit 1
fi

ROOT_DIR="${ROOT_DIR}" bun "${VERIFY_JS}"
