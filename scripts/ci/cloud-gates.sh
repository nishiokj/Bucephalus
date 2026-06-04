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
cargo fmt --check --manifest-path "${ROOT_DIR}/Cargo.toml"

echo "== Rust tests =="
cargo test --manifest-path "${ROOT_DIR}/Cargo.toml"

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
  echo "== Cloud migrations =="
  (
    cd "${ROOT_DIR}/bucephalus-cloud"
    bun run db:migrate
  )
else
  echo "== Cloud migrations skipped: DATABASE_URL is not set =="
fi
