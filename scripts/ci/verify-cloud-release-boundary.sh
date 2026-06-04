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
const cloudflareUiWorkflowPath = ".github/workflows/bucephalus-cloudflare-ui-deploy.yml";
const cloudflareUiWorkflowText = read(cloudflareUiWorkflowPath);
const cloudflareUiWorkflow = YAML.parse(cloudflareUiWorkflowText);
const cloudUiAssetsWorkflowPath = ".github/workflows/bucephalus-cloud-ui-assets.yml";
const cloudUiAssetsWorkflowText = read(cloudUiAssetsWorkflowPath);
const cloudUiAssetsWorkflow = YAML.parse(cloudUiAssetsWorkflowText);
const cloudCiWorkflowPath = ".github/workflows/bucephalus-cloud-ci.yml";
const cloudCiWorkflowText = read(cloudCiWorkflowPath);
const cloudCiWorkflow = YAML.parse(cloudCiWorkflowText);
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
if (!releaseWorkflowText.includes("push_images")) {
  fail(`${releaseWorkflowPath} must distinguish local image inspection from pushed digest promotion`);
}
if (!releaseWorkflowText.includes("push_images requires build_images=true")) {
  fail(`${releaseWorkflowPath} must fail early when pushed image publication is requested without image builds`);
}
if (!releaseWorkflowText.includes("build_images requires a digest-addressed bun_base_image")) {
  fail(`${releaseWorkflowPath} must fail early when image builds omit the digest-addressed base image`);
}
if (!releaseWorkflowText.includes("scripts/release/build-cloud-ui-assets.sh") || !releaseWorkflowText.includes("scripts/release/verify-cloud-ui-assets.sh")) {
  fail(`${releaseWorkflowPath} must build and verify versioned Cloud UI assets before upload`);
}
const releaseWorkflowInputs = releaseWorkflow.on?.workflow_dispatch?.inputs ?? {};
if (releaseWorkflowInputs.version) {
  fail(`${releaseWorkflowPath} must not expose a primary version input for artifact creation`);
}
if (releaseWorkflowInputs.version_override?.type !== "string" || releaseWorkflowInputs.version_override?.required === true) {
  fail(`${releaseWorkflowPath} must not require operators to type a release version for artifact creation`);
}
if (!releaseWorkflowText.includes("scripts/release/resolve-release-version.sh")) {
  fail(`${releaseWorkflowPath} must resolve artifact versions from tags or tracked package metadata`);
}
for (const inputName of [
  "cloudflare_worker_name",
  "cloudflare_api_base",
  "cloudflare_google_oauth_client_id",
]) {
  if (releaseWorkflowInputs[inputName]) {
    fail(`${releaseWorkflowPath} must not ask operators for ${inputName}; Cloudflare deploy config belongs in the GitHub environment`);
  }
}
if (releaseWorkflowInputs.deploy_cloudflare_ui && (!releaseWorkflowText.includes("BUCEPHALUS_CLOUDFLARE_WORKER_NAME") || !releaseWorkflowText.includes("BUCEPHALUS_CLOUD_API_BASE") || !releaseWorkflowText.includes("BUCEPHALUS_GOOGLE_OAUTH_CLIENT_ID") || !releaseWorkflowText.includes("secrets.CLOUDFLARE_SECRET_ID"))) {
  fail(`${releaseWorkflowPath} Cloudflare UI deploy must read config from GitHub environment vars/secrets`);
}
if (releaseWorkflowInputs.deploy_cloudflare_ui && (!releaseWorkflowText.includes("Resolve configured Cloud API base") || !releaseWorkflowText.includes("Discover Cloud API base from GCP") || !releaseWorkflowText.includes("gcloud run services describe"))) {
  fail(`${releaseWorkflowPath} Cloudflare UI deploy must discover the Cloud Run API URL when BUCEPHALUS_CLOUD_API_BASE is not configured`);
}
if (releaseWorkflowInputs.build_public_core_artifacts?.type !== "boolean") {
  fail(`${releaseWorkflowPath} must expose an explicit manual opt-in for public core artifacts`);
}
for (const requiredInput of ["source_release_run_id", "source_release_artifact_name"]) {
  if (releaseWorkflowInputs[requiredInput]?.type !== "string") {
    fail(`${releaseWorkflowPath} must expose ${requiredInput} for artifact-driven image publication`);
  }
}
if (releaseWorkflowInputs.source_release_version?.type !== "string") {
  fail(`${releaseWorkflowPath} must expose source_release_version so image republish can resolve releases without run IDs`);
}
if (!releaseWorkflowText.includes("resolve-cloud-release-artifacts.sh")) {
  fail(`${releaseWorkflowPath} must resolve source release versions through the checked-in release resolver`);
}

if (!deployWorkflowText.includes("actions/download-artifact@v4")) {
  fail(`${deployWorkflowPath} must download pushed image promotion evidence from a release workflow run`);
}
if (!deployWorkflowText.includes("substrate-plan") || !deployWorkflowText.includes("substrate-apply")) {
  fail(`${deployWorkflowPath} must support substrate-only plan/apply before real image digests exist`);
}
if (!deployWorkflowText.includes("Validate digest promotion inputs")) {
  fail(`${deployWorkflowPath} must validate release version or advanced promotion artifact inputs before download`);
}
if (deployWorkflow.on?.workflow_dispatch?.inputs?.release_version?.type !== "string") {
  fail(`${deployWorkflowPath} must expose release_version as the primary deploy selector`);
}
const deployWorkflowInputs = deployWorkflow.on?.workflow_dispatch?.inputs ?? {};
const deployReleaseArtifact = deployWorkflowInputs.release_artifact;
if (deployReleaseArtifact?.type !== "choice") {
  fail(`${deployWorkflowPath} must expose release_artifact as a dropdown selector, not raw URL/SHA inputs`);
}
if (!Array.isArray(deployReleaseArtifact?.options) || !deployReleaseArtifact.options.includes("cloud-image-promotion-evidence-x86_64-linux")) {
  fail(`${deployWorkflowPath} release_artifact dropdown must include the x86_64 Linux pushed-image promotion artifact`);
}
if (!Array.isArray(deployReleaseArtifact?.options) || !deployReleaseArtifact.options.includes("cloud-image-promotion-evidence-from-release")) {
  fail(`${deployWorkflowPath} release_artifact dropdown must include the from-release pushed-image promotion artifact`);
}
for (const inputName of Object.keys(deployWorkflowInputs)) {
  if (/archive.*(url|sha|checksum)|artifact.*(url|sha|checksum)|sha256/i.test(inputName)) {
    fail(`${deployWorkflowPath} must not ask operators for raw release archive URLs or SHA256 inputs (${inputName})`);
  }
}
if (!deployWorkflowText.includes("Resolve release promotion evidence") || !deployWorkflowText.includes("resolve-cloud-release-artifacts.sh")) {
  fail(`${deployWorkflowPath} must resolve release_version to promotion evidence before download`);
}
if (!deployWorkflowText.includes("scripts/release/verify-cloud-image-promotion-evidence-index.sh")) {
  fail(`${deployWorkflowPath} must verify pushed image promotion evidence before Terraform plan/apply`);
}
if (!deployWorkflowText.includes("scripts/deploy/write-gcp-deploy-tfvars.sh")) {
  fail(`${deployWorkflowPath} must render deploy tfvars through the checked deploy input writer`);
}
if (!deployWorkflowText.includes("gcp-image-digests.tfvars")) {
  fail(`${deployWorkflowPath} must consume generated digest tfvars from promotion evidence`);
}
if (!deployWorkflowText.includes("oauth_user_client_id")) {
  fail(`${deployWorkflowPath} must require the user OAuth client ID as a deployment input`);
}
if (!deployWorkflowText.includes("--deploy-control-plane-services")) {
  fail(`${deployWorkflowPath} must explicitly choose substrate-only versus service deployment`);
}
if (/api_image_digest|pool_controller_image_digest|migration_image_digest/.test(JSON.stringify(deployWorkflow.on?.workflow_dispatch?.inputs ?? {}))) {
  fail(`${deployWorkflowPath} must not accept handwritten deploy image digest inputs`);
}
if (!deployWorkflowText.includes("terraform init") || !deployWorkflowText.includes("terraform plan") || !deployWorkflowText.includes("terraform apply")) {
  fail(`${deployWorkflowPath} must run Terraform init, plan, and gated apply`);
}
if (!deployWorkflowText.includes("terraform_backend_bucket") || !deployWorkflowText.includes("terraform_backend_prefix")) {
  fail(`${deployWorkflowPath} must require a remote Terraform backend`);
}
if (!deployWorkflowText.includes("gcloud run jobs execute") || !deployWorkflowText.includes("-migrations")) {
  fail(`${deployWorkflowPath} must run the scoped Cloud Run migration job after apply`);
}
if (!deployWorkflowText.includes("-target=google_cloud_run_v2_job.migrations")) {
  fail(`${deployWorkflowPath} must update the migration job revision before executing migrations`);
}
if (!deployWorkflowText.includes("inputs.terraform_action == 'substrate-plan' || inputs.terraform_action == 'substrate-apply' || inputs.terraform_action == 'api-plan' || inputs.terraform_action == 'pool-plan'")) {
  fail(`${deployWorkflowPath} must not run an unused Terraform pre-plan before digest apply promotions`);
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

const cloudflareUiInputs = cloudflareUiWorkflow.on?.workflow_dispatch?.inputs ?? {};
if (cloudflareUiInputs.github_environment?.type !== "choice") {
  fail(`${cloudflareUiWorkflowPath} must expose GitHub environment as a dropdown selector`);
}
if (cloudflareUiInputs.release_version_override?.type !== "string" || cloudflareUiInputs.release_version_override?.required === true) {
  fail(`${cloudflareUiWorkflowPath} must default to latest Cloud UI assets and expose only an optional version override`);
}
for (const inputName of [
  "release_version",
  "release_run_id",
  "ui_artifact_name",
  "cloudflare_worker_name",
  "cloudflare_account_id",
  "api_base",
  "google_oauth_client_id",
]) {
  if (cloudflareUiInputs[inputName]) {
    fail(`${cloudflareUiWorkflowPath} must not ask operators for ${inputName}; deploy config belongs in the GitHub environment`);
  }
}
if (!cloudflareUiWorkflowText.includes("Resolve Cloud UI assets") || !cloudflareUiWorkflowText.includes("--need ui") || !cloudflareUiWorkflowText.includes("--latest") || !cloudflareUiWorkflowText.includes("release_version_override") || !cloudflareUiWorkflowText.includes("bucephalus-cloud-ui-assets.yml")) {
  fail(`${cloudflareUiWorkflowPath} must resolve latest Cloud UI assets by default before deploy`);
}
if (!cloudflareUiWorkflowText.includes("Resolve configured Cloud API base") || !cloudflareUiWorkflowText.includes("Discover Cloud API base from GCP") || !cloudflareUiWorkflowText.includes("gcloud run services describe")) {
  fail(`${cloudflareUiWorkflowPath} must discover the Cloud Run API URL when BUCEPHALUS_CLOUD_API_BASE is not configured`);
}
if (!cloudflareUiWorkflowText.includes("actions/download-artifact@v4")) {
  fail(`${cloudflareUiWorkflowPath} must download versioned Cloud UI assets from a release workflow run`);
}
if (!cloudflareUiWorkflowText.includes("scripts/release/verify-cloud-ui-assets.sh")) {
  fail(`${cloudflareUiWorkflowPath} must verify Cloud UI assets before Cloudflare deploy`);
}
if (!cloudflareUiWorkflowText.includes("scripts/deploy/deploy-cloudflare-ui.sh")) {
  fail(`${cloudflareUiWorkflowPath} must deploy Cloud UI through the checked Cloudflare deploy script`);
}
if (!cloudflareUiWorkflowText.includes("CLOUDFLARE_API_TOKEN") && !cloudflareUiWorkflowText.includes("CLOUDFLARE_SECRET_KEY")) {
  fail(`${cloudflareUiWorkflowPath} must use a Cloudflare API token secret for CI deploys`);
}
if (!cloudflareUiWorkflowText.includes("secrets.CLOUDFLARE_SECRET_ID")) {
  fail(`${cloudflareUiWorkflowPath} must keep supporting existing CLOUDFLARE_SECRET_ID account-id secrets`);
}
for (const requiredEnv of [
  "BUCEPHALUS_CLOUDFLARE_WORKER_NAME",
  "BUCEPHALUS_CLOUDFLARE_ACCOUNT_ID",
  "BUCEPHALUS_CLOUD_API_BASE",
  "BUCEPHALUS_GOOGLE_OAUTH_CLIENT_ID",
]) {
  if (!cloudflareUiWorkflowText.includes(requiredEnv)) {
    fail(`${cloudflareUiWorkflowPath} must read ${requiredEnv} from GitHub environment configuration`);
  }
}
const cloudUiAssetsInputs = cloudUiAssetsWorkflow.on?.workflow_dispatch?.inputs ?? {};
if (cloudUiAssetsInputs.version) {
  fail(`${cloudUiAssetsWorkflowPath} must not expose a primary version input for artifact creation`);
}
if (cloudUiAssetsInputs.version_override?.type !== "string" || cloudUiAssetsInputs.version_override?.required === true) {
  fail(`${cloudUiAssetsWorkflowPath} must not require operators to type a Cloud UI asset version`);
}
if (!cloudUiAssetsWorkflowText.includes("scripts/release/resolve-release-version.sh")) {
  fail(`${cloudUiAssetsWorkflowPath} must resolve UI asset versions from tags or tracked package metadata`);
}
if (!cloudUiAssetsWorkflowText.includes("scripts/release/build-cloud-ui-assets.sh") || !cloudUiAssetsWorkflowText.includes("scripts/release/verify-cloud-ui-assets.sh")) {
  fail(`${cloudUiAssetsWorkflowPath} must build and verify versioned Cloud UI assets`);
}
if (!cloudUiAssetsWorkflowText.includes("cloud-ui-assets-${{ steps.version.outputs.version }}")) {
  fail(`${cloudUiAssetsWorkflowPath} must upload UI assets under a versioned artifact name`);
}

const permissions = releaseWorkflow.permissions ?? {};
if (permissions.contents !== "read" || permissions["id-token"] !== "none") {
  fail(`${releaseWorkflowPath} top-level permissions must default to contents: read and id-token: none`);
}

const deployPermissions = deployWorkflow.permissions ?? {};
if (deployPermissions.contents !== "read" || deployPermissions.actions !== "read" || deployPermissions["id-token"] !== "none") {
  fail(`${deployWorkflowPath} top-level permissions must default to contents/actions read and id-token: none`);
}
const cloudflarePermissions = cloudflareUiWorkflow.permissions ?? {};
if (cloudflarePermissions.contents !== "read" || cloudflarePermissions.actions !== "read" || cloudflarePermissions["id-token"] !== "none") {
  fail(`${cloudflareUiWorkflowPath} top-level permissions must default to contents/actions read and id-token none`);
}
const cloudUiAssetsPermissions = cloudUiAssetsWorkflow.permissions ?? {};
if (cloudUiAssetsPermissions.contents !== "read" || cloudUiAssetsPermissions["id-token"] !== "none") {
  fail(`${cloudUiAssetsWorkflowPath} top-level permissions must be contents read and id-token none`);
}

const releaseJobs = releaseWorkflow.jobs ?? {};
const deployJobs = deployWorkflow.jobs ?? {};
const cloudflareJobs = cloudflareUiWorkflow.jobs ?? {};
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
    "Resolve release promotion evidence",
    "Download pushed image promotion evidence",
    "Validate digest promotion inputs",
    "Locate and verify promotion evidence",
    "Render deploy tfvars",
    "Authenticate to Google Cloud for deployment",
    "Terraform plan selected action",
    "Terraform apply migration job revision",
    "Run migration job",
    "Terraform apply selected digest promotion",
    "Smoke deployed API",
  ]) {
    if (!deployStepNames.includes(required)) {
      fail(`${deployWorkflowPath} deploy-gcp missing step: ${required}`);
    }
  }
  const deploySteps = deployGcp.steps ?? [];
  const planStep = deploySteps.find((step) => step.name === "Terraform plan selected action");
  const planIf = String(planStep?.if ?? "");
  for (const requiredAction of ["substrate-plan", "substrate-apply", "api-plan", "pool-plan"]) {
    if (!planIf.includes(requiredAction)) {
      fail(`${deployWorkflowPath} Terraform plan step must include ${requiredAction}`);
    }
  }
  for (const skippedAction of ["api-apply", "pool-apply"]) {
    if (planIf.includes(skippedAction)) {
      fail(`${deployWorkflowPath} Terraform plan step must skip ${skippedAction} to avoid repeated apply-time planning`);
    }
  }
  const gcloudDeployStep = deploySteps.find((step) => step.name === "Set up gcloud for migration job");
  if (String(gcloudDeployStep?.if ?? "") !== "${{ inputs.terraform_action == 'api-apply' }}") {
    fail(`${deployWorkflowPath} must install gcloud only for API migration job execution`);
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

const buildCloudUi = releaseJobs["build-cloud-ui-assets"];
if (!buildCloudUi) {
  fail(`${releaseWorkflowPath} must contain build-cloud-ui-assets job`);
} else {
  if (buildCloudUi.permissions?.contents !== "read" || buildCloudUi.permissions?.["id-token"] === "write") {
    fail(`${releaseWorkflowPath} build-cloud-ui-assets must have contents read without OIDC token write permission`);
  }
  const steps = buildCloudUi.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  for (const required of [
    "Install Cloud dependencies",
    "Resolve version",
    "Build Cloud UI assets",
    "Verify Cloud UI assets",
    "Upload Cloud UI assets",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${releaseWorkflowPath} build-cloud-ui-assets missing step: ${required}`);
    }
  }
  const uploadStep = steps.find((step) => step.name === "Upload Cloud UI assets");
  if (uploadStep?.uses !== "actions/upload-artifact@v4" || uploadStep.with?.name !== "cloud-ui-assets-${{ steps.version.outputs.version }}") {
    fail(`${releaseWorkflowPath} Cloud UI assets must upload under a versioned artifact name`);
  }
}

const deployCloudflareUi = cloudflareJobs["deploy-cloudflare-ui"];
if (!deployCloudflareUi) {
  fail(`${cloudflareUiWorkflowPath} must contain deploy-cloudflare-ui job`);
} else {
  if (deployCloudflareUi.permissions?.contents !== "read" || deployCloudflareUi.permissions?.actions !== "read" || deployCloudflareUi.permissions?.["id-token"] !== "write") {
    fail(`${cloudflareUiWorkflowPath} deploy-cloudflare-ui must receive OIDC token write permission only for API URL discovery`);
  }
  const stepNames = (deployCloudflareUi.steps ?? []).map((step) => step.name).filter(Boolean);
  for (const required of [
    "Resolve Cloud UI assets",
    "Validate Cloudflare deploy config",
    "Download Cloud UI assets",
    "Locate Cloud UI assets",
    "Verify Cloud UI assets",
    "Deploy Cloud UI to Cloudflare",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${cloudflareUiWorkflowPath} deploy-cloudflare-ui missing step: ${required}`);
    }
  }
}

const imagePublishJob = releaseJobs["publish-cloud-images-from-release"];
if (!imagePublishJob) {
  fail(`${releaseWorkflowPath} must contain publish-cloud-images-from-release job`);
} else {
  if (imagePublishJob.permissions?.contents !== "read" || imagePublishJob.permissions?.actions !== "read" || imagePublishJob.permissions?.["id-token"] !== "write") {
    fail(`${releaseWorkflowPath} publish-cloud-images-from-release must be read-only except OIDC token write permission for optional image publication`);
  }
  if (!String(imagePublishJob.if ?? "").includes("inputs.source_release_run_id")) {
    fail(`${releaseWorkflowPath} artifact-driven image publication must run only when a source release is selected`);
  }
  if (!String(imagePublishJob.if ?? "").includes("inputs.source_release_version")) {
    fail(`${releaseWorkflowPath} artifact-driven image publication must support source_release_version`);
  }
  const steps = imagePublishJob.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  for (const required of [
    "Resolve source release",
    "Validate source release image publication inputs",
    "Download verified Cloud release artifact",
    "Verify source release artifact",
    "Authenticate to Google Cloud for image publication",
    "Configure Artifact Registry Docker auth",
    "Set up Docker Buildx for image cache",
    "Build and inspect Cloud images",
    "Verify GCP image promotion evidence",
    "Upload Cloud image promotion evidence",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${releaseWorkflowPath} publish-cloud-images-from-release missing step: ${required}`);
    }
  }
  const downloadStep = steps.find((step) => step.name === "Download verified Cloud release artifact");
  if (downloadStep?.uses !== "actions/download-artifact@v4" || !String(downloadStep.with?.["run-id"] ?? "").includes("resolved_source_release.outputs.release_run_id") || !String(downloadStep.with?.name ?? "").includes("resolved_source_release.outputs.release_artifact_name")) {
    fail(`${releaseWorkflowPath} must download the resolved release artifact from the resolved run`);
  }
  const verifySourceStep = steps.find((step) => step.name === "Verify source release artifact");
  const verifySourceRun = String(verifySourceStep?.run ?? "");
  if (!verifySourceRun.includes("verify-buc-release.sh") || !verifySourceRun.includes("verify-cloud-release-provenance.sh") || !verifySourceRun.includes("x86_64-unknown-linux-gnu")) {
    fail(`${releaseWorkflowPath} must verify downloaded Cloud release archive, provenance, and x86_64 Linux target before image publication`);
  }
  const imagePublishText = JSON.stringify(imagePublishJob);
  if (/build-core-release\.sh|build-buc-release\.sh|cloud-gates\.sh/.test(imagePublishText)) {
    fail(`${releaseWorkflowPath} artifact-driven image publication must publish from a verified release artifact without rebuilding Core or Cloud release bundles`);
  }
  const buildxStep = steps.find((step) => step.name === "Set up Docker Buildx for image cache");
  if (buildxStep?.uses !== "docker/setup-buildx-action@v3") {
    fail(`${releaseWorkflowPath} artifact-driven image publication must set up Buildx before image publication`);
  }
  const imageBuildStep = steps.find((step) => step.name === "Build and inspect Cloud images");
  const imageBuildRun = String(imageBuildStep?.run ?? "");
  if (!imageBuildRun.includes("build-cloud-images.sh") || !imageBuildRun.includes("steps.source_release.outputs.release_archive")) {
    fail(`${releaseWorkflowPath} must build images from the verified downloaded release archive`);
  }
  if (!String(imageBuildStep?.env?.BUCEPHALUS_SOURCE_RELEASE_RUN_ID ?? "").includes("resolved_source_release.outputs.release_run_id") || !String(imageBuildStep?.env?.BUCEPHALUS_SOURCE_RELEASE_ARTIFACT_NAME ?? "").includes("resolved_source_release.outputs.release_artifact_name")) {
    fail(`${releaseWorkflowPath} must record source release run and artifact inputs in artifact-driven image manifests`);
  }
  const authStep = steps.find((step) => step.name === "Authenticate to Google Cloud for image publication");
  if (!String(authStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} artifact-driven image publication must authenticate to GCP only for pushed image publication`);
  }
  const promotionUploadStep = steps.find((step) => step.name === "Upload Cloud image promotion evidence");
  if (!String(promotionUploadStep?.with?.name ?? "").includes("steps.source_release.outputs.version")) {
    fail(`${releaseWorkflowPath} must upload artifact-driven promotion evidence under a versioned handoff name`);
  }
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
    fail(`${releaseWorkflowPath} image build step must pass --push only for pushed image publication`);
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
  if (!String(authStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} must authenticate to GCP only for pushed image publication`);
  }
  const resolveAuthStep = steps.find((step) => step.name === "Resolve GCP CI/CD auth secret for image publication");
  if (!String(resolveAuthStep?.run ?? "").includes("resolve-gcp-cicd-secret.sh --mode publish")) {
    fail(`${releaseWorkflowPath} must resolve image publication auth through BUC_CI_CD/legacy OIDC resolver`);
  }
  if (authStep?.with?.workload_identity_provider !== "${{ steps.gcp_publish_auth.outputs.workload_identity_provider }}" || authStep?.with?.service_account !== "${{ steps.gcp_publish_auth.outputs.service_account }}") {
    fail(`${releaseWorkflowPath} GCP auth must use resolved workload identity and service account outputs`);
  }
  const dockerAuthStep = steps.find((step) => step.name === "Configure Artifact Registry Docker auth");
  if (!String(dockerAuthStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} must configure Artifact Registry Docker auth only for pushed image publication`);
  }
  if (!String(dockerAuthStep?.run ?? "").includes("configure-gcp-artifact-registry-auth.sh")) {
    fail(`${releaseWorkflowPath} Docker auth step must use the checked-in Artifact Registry auth script`);
  }
  const tfvarsStep = steps.find((step) => step.name === "Write GCP image tfvars");
  if (!String(tfvarsStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} must write GCP image tfvars only for pushed image manifests`);
  }
  const imageManifestUploadStep = steps.find((step) => step.name === "Upload Cloud image build manifest");
  if (String(imageManifestUploadStep?.with?.path ?? "").includes("gcp-image-digests.tfvars")) {
    fail(`${releaseWorkflowPath} must not require GCP tfvars for local image inspection artifacts`);
  }
  const promotionIndexStep = steps.find((step) => step.name === "Write Cloud image promotion evidence index");
  if (!String(promotionIndexStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} must write the image promotion evidence index only for pushed manifests`);
  }
  const promotionIndexVerifyStep = steps.find((step) => step.name === "Verify Cloud image promotion evidence index");
  if (!String(promotionIndexVerifyStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} must verify the image promotion evidence index only for pushed manifests`);
  }
  const promotionUploadStep = steps.find((step) => step.name === "Upload Cloud image promotion evidence");
  if (!String(promotionUploadStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} must upload image promotion evidence only for pushed manifests`);
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
  if (promotionUploadStep?.with?.name !== "cloud-image-promotion-evidence-${{ steps.version.outputs.version }}-${{ matrix.target }}") {
    fail(`${releaseWorkflowPath} image promotion evidence artifact must have a versioned artifact name`);
  }
  const coreUploadStep = steps.find((step) => step.name === "Upload core release archive");
  if (coreUploadStep?.with?.name !== "core-${{ matrix.target }}") {
    fail(`${releaseWorkflowPath} Linux core archives must still upload as core-target artifacts`);
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
  if (!String(buildMacosCore.if ?? "").includes("github.ref_type == 'tag'") || !String(buildMacosCore.if ?? "").includes("inputs.build_public_core_artifacts")) {
    fail(`${releaseWorkflowPath} build-macos-core-release must run for tags and only by explicit opt-in for manual deploy/image runs`);
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
  if (coreUploadStep?.with?.name !== "core-${{ matrix.target }}") {
    fail(`${releaseWorkflowPath} macOS core archives must upload as core-target artifacts`);
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

const workflowLatestMatches = releaseWorkflowText.match(/(?:^|[^A-Za-z0-9_.-])latest(?:[^A-Za-z0-9_.-]|$)/g) ?? [];
if (workflowLatestMatches.length > 0) {
  fail(`${releaseWorkflowPath} must not use latest as a release/image input`);
}
const deployWorkflowLatestMatches = deployWorkflowText.match(/(?:^|[^A-Za-z0-9_.-])latest(?:[^A-Za-z0-9_.-]|$)/g) ?? [];
if (deployWorkflowLatestMatches.length > 0) {
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
  ["bucephalus-cloud/images/Dockerfile.worker", ["runtime-dist/worker.js", "runtime-dist/secretResolver.js", "bin/bucephalus"]],
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
  "scripts/release/build-cloud-ui-assets.sh",
  "scripts/release/configure-gcp-artifact-registry-auth.sh",
  "scripts/release/resolve-cloud-release-artifacts.sh",
  "scripts/deploy/deploy-cloudflare-ui.sh",
  "scripts/release/verify-cloud-base-image-policy.sh",
  "scripts/release/verify-cloud-ui-assets.sh",
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
  if (/cloud-image-promotion-evidence-\$\{VERSION\}-from-release/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must resolve versioned artifact-driven promotion evidence by release version`);
  }
  if (/bucephalus-\$\{VERSION\}-x86_64-unknown-linux-gnu/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must resolve the versioned x86_64 Linux Cloud release artifact`);
  }
  if (/status=completed/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must inspect completed release workflow runs when resolving releases`);
  }
  if (/cloud-ui-assets-\$\{VERSION\}/.test(text) === false && script === "scripts/release/resolve-cloud-release-artifacts.sh") {
    fail(`${script} must resolve versioned Cloud UI assets by release version`);
  }
  if ((/web:build/.test(text) === false || /bucephalus_cloud_ui_assets_v1/.test(text) === false || /cloudflare_workers_static_assets/.test(text) === false) && script === "scripts/release/build-cloud-ui-assets.sh") {
    fail(`${script} must build the Vite UI and record a Cloudflare static-assets handoff manifest`);
  }
  if ((/SHA256SUMS/.test(text) === false || /dist_tree_sha256/.test(text) === false) && script === "scripts/release/build-cloud-ui-assets.sh") {
    fail(`${script} must checksum the Cloud UI dist tree`);
  }
  if ((/bucephalus_cloud_ui_assets_v1/.test(text) === false || /cloudflare_workers_static_assets/.test(text) === false) && script === "scripts/release/verify-cloud-ui-assets.sh") {
    fail(`${script} must verify Cloud UI asset manifest schema and deploy target`);
  }
  if ((/wrangler/.test(text) === false || /run_worker_first/.test(text) === false || /single-page-application/.test(text) === false || /BUCEPHALUS_API_BASE/.test(text) === false) && script === "scripts/deploy/deploy-cloudflare-ui.sh") {
    fail(`${script} must deploy through Wrangler Workers Static Assets with SPA routing and API-base injection`);
  }
  if (/BUCEPHALUS_USER_TOKEN/.test(text) && script === "scripts/deploy/deploy-cloudflare-ui.sh") {
    fail(`${script} must not inject user tokens into the public Cloud UI shell`);
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
  if (/verify-cloud-image-build-manifest\.sh" "\$\{MANIFEST_PATH\}" --release "\$\{RELEASE_INPUT\}"/.test(text) === false && script === "scripts/release/build-cloud-images.sh") {
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
  if (/^ENV\s+/m.test(text)) {
    fail(`${dockerfile} must not bake runtime configuration with ENV`);
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
if (!provisionRunnerVmText.includes("ensure_host_dependencies") || !provisionRunnerVmText.includes("command -v docker")) {
  fail("GCE runner startup must check for preinstalled Docker before falling back to package installation");
}
if (/apt-get install -y --no-install-recommends ca-certificates curl docker\.io/.test(provisionRunnerVmText)) {
  fail("GCE runner startup must not unconditionally apt-install Docker on every VM boot");
}
if (!gcpVariablesText.includes("projects/cos-cloud/global/images/family/cos-stable")) {
  fail(`${gcpVariablesPath} runner_gce_boot_image must default to Container-Optimized OS`);
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

ROOT_DIR="${ROOT_DIR}" bun "${VERIFY_JS}"
