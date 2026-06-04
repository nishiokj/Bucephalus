#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
POLICY="${ROOT_DIR}/docs/specs/CLOUD_PATH2_SIGNING_POLICY.json"

usage() {
  cat <<'USAGE'
Usage: scripts/release/verify-cloud-signing-policy.sh [--policy <path>]

Validates the Path 2 signing policy. Current release/provenance records must
remain explicitly unsigned until a real registry/release/deploy signing boundary
and verifier exist.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --policy)
      POLICY="${2:-}"
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

if [[ ! -f "${POLICY}" ]]; then
  echo "signing policy does not exist: ${POLICY}" >&2
  exit 2
fi

if ! command -v bun >/dev/null 2>&1; then
  echo "required command not found: bun" >&2
  exit 2
fi

POLICY="${POLICY}" bun -e '
const policy = JSON.parse(await Bun.file(process.env.POLICY).text());

function fail(message) {
  console.error(message);
  process.exit(1);
}

if (policy.schema_version !== "bucephalus_cloud_path2_signing_policy_v1") {
  fail("schema_version must be bucephalus_cloud_path2_signing_policy_v1");
}
if (policy.policy_status !== "unsigned_until_signing_boundary_configured") {
  fail("policy_status must keep records unsigned until signing is configured");
}

const requiredUnsigned = new Set([
  "bucephalus_core_release_provenance_v1",
  "bucephalus_cloud_release_provenance_v1",
  "bucephalus_release_asset_index_v1",
  "bucephalus_cloud_image_promotion_evidence_index_v1",
]);
if (!Array.isArray(policy.allowed_unsigned_schemas)) {
  fail("allowed_unsigned_schemas must be an array");
}
const allowedUnsigned = new Set(policy.allowed_unsigned_schemas);
for (const schema of requiredUnsigned) {
  if (!allowedUnsigned.has(schema)) {
    fail(`allowed_unsigned_schemas missing ${schema}`);
  }
}

const forbidden = new Set(policy.forbidden_signature_statuses ?? []);
for (const status of ["signed", "verified", "keyless", "cosign"]) {
  if (!forbidden.has(status)) {
    fail(`forbidden_signature_statuses must include ${status}`);
  }
}
if (forbidden.has("unsigned")) {
  fail("forbidden_signature_statuses must not include unsigned while unsigned records are allowed");
}

const blockers = new Set(policy.required_before_signed_status ?? []);
for (const blocker of [
  "registry_or_release_signing_identity_declared",
  "issuer_and_subject_policy_declared",
  "certificate_or_transparency_log_verifier_declared",
  "signature_material_attached_as_release_or_registry_artifact",
  "signed_attestation_verifier_runs_in_ci",
  "deploy_promotion_verifies_signed_attestation",
]) {
  if (!blockers.has(blocker)) {
    fail(`required_before_signed_status missing ${blocker}`);
  }
}

if (policy.requirements?.unsigned_records_must_self_hash !== true) {
  fail("unsigned records must continue to self-hash");
}
if (policy.requirements?.unsigned_records_must_not_claim_attestation !== true) {
  fail("unsigned records must not claim attestation");
}
if (policy.requirements?.future_signed_records_must_be_verified_before_promotion !== true) {
  fail("future signed records must be verified before promotion");
}

console.log(`verified signing policy ${process.env.POLICY}`);
'
