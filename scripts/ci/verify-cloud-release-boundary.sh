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
const cloudCiWorkflowPath = ".github/workflows/bucephalus-cloud-ci.yml";
const cloudCiWorkflowText = read(cloudCiWorkflowPath);
const cloudCiWorkflow = YAML.parse(cloudCiWorkflowText);
const cloudGatesPath = "scripts/ci/cloud-gates.sh";
const cloudGatesText = read(cloudGatesPath);
const installScriptPath = "scripts/install.sh";
const installScriptText = read(installScriptPath);
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

if (!deployWorkflowText.includes("actions/download-artifact@v4")) {
  fail(`${deployWorkflowPath} must download pushed image promotion evidence from a release workflow run`);
}
if (!deployWorkflowText.includes("substrate-plan") || !deployWorkflowText.includes("substrate-apply")) {
  fail(`${deployWorkflowPath} must support substrate-only plan/apply before real image digests exist`);
}
if (!deployWorkflowText.includes("Validate digest promotion inputs")) {
  fail(`${deployWorkflowPath} must validate release_run_id and promotion artifact before download`);
}
if (!deployWorkflowText.includes("scripts/release/verify-cloud-image-promotion-evidence-index.sh")) {
  fail(`${deployWorkflowPath} must verify pushed image promotion evidence before Terraform planning`);
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
if (!deployWorkflowText.includes("BUCEPHALUS_CLOUD_SMOKE_USER_TOKEN") || !deployWorkflowText.includes("BUCEPHALUS_CLOUD_SMOKE_WORKER_TOKEN")) {
  fail(`${deployWorkflowPath} must require user and worker smoke identities after apply`);
}
if (!deployWorkflowText.includes("/v1/packages") || !deployWorkflowText.includes("/v1/runner-pools")) {
  fail(`${deployWorkflowPath} must smoke both user and worker API authentication paths`);
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
    "Download pushed image promotion evidence",
    "Validate digest promotion inputs",
    "Locate and verify promotion evidence",
    "Render deploy tfvars",
    "Authenticate to Google Cloud for deployment",
    "Terraform plan selected digest promotion",
    "Terraform apply migration job revision",
    "Run migration job",
    "Terraform apply selected digest promotion",
    "Smoke deployed API",
  ]) {
    if (!deployStepNames.includes(required)) {
      fail(`${deployWorkflowPath} deploy-gcp missing step: ${required}`);
    }
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
const buildCore = releaseJobs["build-core-release"];
if (!buildCore) {
  fail(`${releaseWorkflowPath} must contain build-core-release job`);
} else {
  if (buildCore.permissions?.contents !== "read" || buildCore.permissions?.["id-token"] === "write") {
    fail(`${releaseWorkflowPath} build-core-release must not receive OIDC token write permission`);
  }
  const steps = buildCore.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  const hasBunSetup = steps.some((step) => step.uses === "oven-sh/setup-bun@v2" && step.with?.["bun-version"] === "1.3.14");
  if (!hasBunSetup) {
    fail(`${releaseWorkflowPath} build-core-release must install pinned Bun before running core verifiers`);
  }
  for (const required of [
    "Verify core release archive",
    "Write core release provenance",
  ]) {
    if (!stepNames.includes(required)) {
      fail(`${releaseWorkflowPath} build-core-release missing step: ${required}`);
    }
  }
}
const buildLinux = releaseJobs["build-linux-release"];
if (!buildLinux) {
  fail(`${releaseWorkflowPath} must contain build-linux-release job`);
} else {
  if (buildLinux.permissions?.contents !== "read" || buildLinux.permissions?.["id-token"] !== "write") {
    fail(`${releaseWorkflowPath} build-linux-release must be the release build job with OIDC token write permission`);
  }
  const steps = buildLinux.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
  for (const required of [
    "Authenticate to Google Cloud for image publication",
    "Set up gcloud for image publication",
    "Configure Artifact Registry Docker auth",
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
  const authStep = steps.find((step) => step.name === "Authenticate to Google Cloud for image publication");
  const releaseProvenanceIndex = stepNames.indexOf("Write release provenance");
  const authIndex = stepNames.indexOf("Authenticate to Google Cloud for image publication");
  const imageBuildIndex = stepNames.indexOf("Build and inspect Cloud images");
  if (authIndex <= releaseProvenanceIndex || authIndex >= imageBuildIndex) {
    fail(`${releaseWorkflowPath} must authenticate to GCP after release artifact/provenance creation and before pushed image publication`);
  }
  if (!String(authStep?.if ?? "").includes("inputs.push_images")) {
    fail(`${releaseWorkflowPath} must authenticate to GCP only for pushed image publication`);
  }
  if (authStep?.with?.workload_identity_provider !== "${{ secrets.BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER }}" || authStep?.with?.service_account !== "${{ secrets.BUCEPHALUS_GCP_SERVICE_ACCOUNT }}") {
    fail(`${releaseWorkflowPath} GCP auth must use the declared workload identity and service account secrets`);
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
  if (promotionUploadStep?.with?.name !== "cloud-image-promotion-evidence-${{ matrix.target }}") {
    fail(`${releaseWorkflowPath} image promotion evidence artifact must have a dedicated artifact name`);
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
const publishRelease = releaseJobs["publish-github-release"];
if (!publishRelease) {
  fail(`${releaseWorkflowPath} must contain publish-github-release job`);
} else {
  if (publishRelease.permissions?.contents !== "write" || publishRelease.permissions?.["id-token"] === "write") {
    fail(`${releaseWorkflowPath} publish-github-release must have contents write without OIDC token write permission`);
  }
  const steps = publishRelease.steps ?? [];
  const stepNames = steps.map((step) => step.name).filter(Boolean);
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
    "--required-core-target x86_64-apple-darwin",
    "--required-core-target aarch64-apple-darwin",
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
  "scripts/release/configure-gcp-artifact-registry-auth.sh",
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
  if (/worker image tfvars are not deploy inputs/.test(text) === false && script === "scripts/release/verify-gcp-image-tfvars.sh") {
    fail(`${script} must reject worker image inputs in deploy tfvars`);
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
  if (/worker image tfvars are not promotion inputs/.test(text) === false && script === "scripts/release/verify-gcp-image-promotion-evidence.sh") {
    fail(`${script} must reject worker image inputs in promotion tfvars`);
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

const deployFiles = listFiles("bucephalus-cloud/deploy");
for (const file of deployFiles) {
  if (!file.endsWith(".md")) {
    fail(`retired non-Markdown deploy payload is present: ${file}`);
  }
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
