#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ -z "${DATABASE_URL:-}" && -z "${BUCEPHALUS_CLOUD_HTTP_WORKFLOW_DATABASE_URL:-}" ]]; then
  echo "DATABASE_URL or BUCEPHALUS_CLOUD_HTTP_WORKFLOW_DATABASE_URL is required for the hosted buc workflow smoke" >&2
  exit 1
fi

(
  cd "${ROOT_DIR}/bucephalus-cloud"
  bun run check:postgres
)

cargo build -p bucephalus-cli --bin buc --manifest-path "${ROOT_DIR}/Cargo.toml"

(
  cd "${ROOT_DIR}"
  BUCEPHALUS_CLOUD_HTTP_WORKFLOW_SMOKE=1 \
    BUC_BINARY="${ROOT_DIR}/target/debug/buc" \
    bun test bucephalus-cloud/tests/bucHostedWorkflowSmoke.test.ts
)
