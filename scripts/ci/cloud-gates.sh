#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export BUCEPHALUS_MIN_FREE_BYTES="${BUCEPHALUS_MIN_FREE_BYTES:-1073741824}"
BUCEPHALUS_CLOUD_GATE_RUST_TIMEOUT_SECONDS="${BUCEPHALUS_CLOUD_GATE_RUST_TIMEOUT_SECONDS:-1800}"

run_with_timeout() {
  local label="$1"
  shift
  local seconds="${BUCEPHALUS_CLOUD_GATE_RUST_TIMEOUT_SECONDS}"
  if command -v timeout >/dev/null 2>&1; then
    timeout "${seconds}" "$@"
    return
  fi

  "$@" &
  local pid="$!"
  (
    sleep "${seconds}"
    if kill -0 "${pid}" 2>/dev/null; then
      echo "${label} timed out after ${seconds}s" >&2
      pkill -TERM -P "${pid}" 2>/dev/null || true
      kill -TERM "${pid}" 2>/dev/null || true
      sleep 5
      pkill -KILL -P "${pid}" 2>/dev/null || true
      kill -KILL "${pid}" 2>/dev/null || true
    fi
  ) &
  local watchdog="$!"
  local status=0
  wait "${pid}" || status="$?"
  kill "${watchdog}" 2>/dev/null || true
  wait "${watchdog}" 2>/dev/null || true
  return "${status}"
}

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

echo "== Hosted product CLI Rust tests =="
cargo test --manifest-path "${ROOT_DIR}/Cargo.toml" --bin buc

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

echo "== Hosted authoring real Core smoke =="
"${ROOT_DIR}/scripts/ci/smoke-hosted-authoring-real-core.sh"

echo "== Rust workspace tests =="
run_with_timeout "Rust workspace tests" cargo test --workspace --manifest-path "${ROOT_DIR}/Cargo.toml"

echo "== OpenAPI parse =="
(
  cd "${ROOT_DIR}/bucephalus-cloud"
  bun run validate:openapi
)

if [[ -n "${DATABASE_URL:-}" ]]; then
  echo "== Cloud Postgres readiness =="
  (
    cd "${ROOT_DIR}/bucephalus-cloud"
    bun run check:postgres
  )
  echo "== Cloud migration integration tests =="
  (
    cd "${ROOT_DIR}/bucephalus-cloud"
    bun run test:migrations
  )
  echo "== Hosted buc workflow HTTP smoke =="
  "${ROOT_DIR}/scripts/ci/smoke-buc-hosted-workflow.sh"
elif [[ "${CI:-}" == "true" || "${BUCEPHALUS_REQUIRE_MIGRATION_TESTS:-}" == "true" ]]; then
  echo "DATABASE_URL is required for Cloud migration integration tests in CI" >&2
  exit 1
else
  echo "== Cloud migration integration tests skipped: DATABASE_URL is not set =="
fi
