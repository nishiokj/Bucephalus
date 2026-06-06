#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export BUCEPHALUS_MIN_FREE_BYTES="${BUCEPHALUS_MIN_FREE_BYTES:-1073741824}"

echo "== Cloud dependencies =="
(
  cd "${ROOT_DIR}/bucephalus-cloud"
  bun install --frozen-lockfile
)

echo "== Cloud release boundary policy =="
"${ROOT_DIR}/scripts/ci/verify-cloud-release-boundary.sh"
"${ROOT_DIR}/scripts/release/verify-cloud-signing-policy.sh"

echo "== Rust format =="
cargo fmt --check --all --manifest-path "${ROOT_DIR}/Cargo.toml"

echo "== Rust tests =="
cargo test --workspace --manifest-path "${ROOT_DIR}/Cargo.toml"

echo "== Cloud typecheck =="
(
  cd "${ROOT_DIR}/bucephalus-cloud"
  bun run typecheck
)

echo "== Cloud tests =="
(
  cd "${ROOT_DIR}/bucephalus-cloud"
  bun test
)

echo "== OpenAPI parse =="
(
  cd "${ROOT_DIR}/bucephalus-cloud"
  bun run validate:openapi
)

if [[ -n "${DATABASE_URL:-}" ]]; then
  echo "== Cloud migration integration tests =="
  (
    cd "${ROOT_DIR}/bucephalus-cloud"
    bun run test:migrations
  )
elif [[ "${CI:-}" == "true" || "${BUCEPHALUS_REQUIRE_MIGRATION_TESTS:-}" == "true" ]]; then
  echo "DATABASE_URL is required for Cloud migration integration tests in CI" >&2
  exit 1
else
  echo "== Cloud migration integration tests skipped: DATABASE_URL is not set =="
fi
