#!/usr/bin/env bash
set -euo pipefail

MODE=""

usage() {
  cat <<'USAGE'
Usage: scripts/deploy/resolve-gcp-cicd-secret.sh --mode publish|deploy

Resolves the GCP GitHub Actions auth inputs from either:
  - BUC_CI_CD single JSON secret, or
  - legacy BUCEPHALUS_GCP_* environment variables.

BUC_CI_CD may be:
  {
    "workload_identity_provider": "projects/.../providers/...",
    "service_account": "name@project.iam.gserviceaccount.com"
  }

or:
  {
    "publish": { "workload_identity_provider": "...", "service_account": "..." },
    "deploy": { "workload_identity_provider": "...", "service_account": "..." }
  }
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      MODE="${2:-}"
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

if [[ "${MODE}" != "publish" && "${MODE}" != "deploy" ]]; then
  usage >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

MODE="${MODE}" bun -e '
import { lstatSync } from "node:fs";

const mode = process.env.MODE;
const raw = process.env.BUC_CI_CD?.trim();

function fail(message) {
  console.error(message);
  process.exit(1);
}

function requireAppendTarget(path, label) {
  let stat;
  try {
    stat = lstatSync(path);
  } catch {
    fail(`${label} does not exist or cannot be inspected`);
  }
  if (stat.isSymbolicLink()) {
    fail(`${label} must not be a symlink`);
  }
  if (!stat.isFile()) {
    fail(`${label} must be a regular file`);
  }
}

async function appendGitHubFile(path, label, text) {
  if (!path) {
    return;
  }
  requireAppendTarget(path, label);
  await Bun.write(path, text, { append: true });
}

function pickFromSecret(secret) {
  const scoped = mode === "publish"
    ? (secret.publish ?? secret.publisher)
    : (secret.deploy ?? secret.deployer);
  const source = scoped && typeof scoped === "object" && !Array.isArray(scoped) ? scoped : secret;
  return {
    workloadIdentityProvider:
      source.workload_identity_provider ??
      source.workloadIdentityProvider ??
      source.provider,
    serviceAccount:
      source.service_account ??
      source.serviceAccount,
  };
}

let values;
if (raw) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    fail("BUC_CI_CD must be JSON with workload_identity_provider and service_account");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    fail("BUC_CI_CD must be a JSON object");
  }
  values = pickFromSecret(parsed);
} else if (mode === "publish") {
  values = {
    workloadIdentityProvider: process.env.BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER,
    serviceAccount: process.env.BUCEPHALUS_GCP_SERVICE_ACCOUNT,
  };
} else {
  values = {
    workloadIdentityProvider:
      process.env.BUCEPHALUS_GCP_DEPLOY_WORKLOAD_IDENTITY_PROVIDER ??
      process.env.BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER,
    serviceAccount:
      process.env.BUCEPHALUS_GCP_DEPLOY_SERVICE_ACCOUNT ??
      process.env.BUCEPHALUS_GCP_SERVICE_ACCOUNT,
  };
}

const provider = String(values.workloadIdentityProvider ?? "").trim();
const account = String(values.serviceAccount ?? "").trim();
if (!/^projects\/[0-9]+\/locations\/global\/workloadIdentityPools\/[A-Za-z0-9_-]+\/providers\/[A-Za-z0-9_-]+$/.test(provider)) {
  fail(`${mode} workload_identity_provider is missing or invalid`);
}
if (!/^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.iam\.gserviceaccount\.com$/.test(account)) {
  fail(`${mode} service_account is missing or invalid`);
}

const output = process.env.GITHUB_OUTPUT;
await appendGitHubFile(output, "GitHub output file", `workload_identity_provider=${provider}\nservice_account=${account}\n`);
const env = process.env.GITHUB_ENV;
await appendGitHubFile(env, "GitHub env file", `BUCEPHALUS_GCP_WORKLOAD_IDENTITY_PROVIDER=${provider}\nBUCEPHALUS_GCP_SERVICE_ACCOUNT=${account}\n`);
console.log(`resolved ${mode} GCP CI/CD identity`);
'
