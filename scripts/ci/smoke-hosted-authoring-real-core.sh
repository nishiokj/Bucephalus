#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

require_command cargo
require_command bun

cargo build -p bucephalus-cli --bin bucephalus

BUCEPHALUS_CLOUD_REAL_CORE_SMOKE=1 \
BUCEPHALUS_CLOUD_CORE_CLI="${ROOT_DIR}/target/debug/bucephalus" \
bun test bucephalus-cloud/tests/hostedAuthoringRealCore.test.ts
