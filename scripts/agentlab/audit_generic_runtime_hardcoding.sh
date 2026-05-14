#!/usr/bin/env bash
set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
src="$root/rust/crates/lab-runner/src"

if rg -n 'swebench|sweb|bench_v0|official_swebench|swebench_testbed' "$src" \
  -g '*.rs' \
  -g '!tests.rs'; then
  echo "benchmark-specific strings are not allowed in generic runner source" >&2
  exit 1
fi

if rg -n 'rex-events|should_append_rex|is_rex|\brex\b|rex\.js' "$src" \
  -g '*.rs' \
  -g '!tests.rs'; then
  echo "agent-specific strings are not allowed in generic runner source" >&2
  exit 1
fi
