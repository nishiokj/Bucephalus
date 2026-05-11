#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LAB="${LAB:-$ROOT_DIR/scripts/lab-fresh.sh}"
BUILD_DIR="${BUILD_DIR:-$ROOT_DIR/.lab/builds/docs-golden-demo}"
RUN_FULL="${RUN_FULL:-0}"
DOCS_COMMAND_TIMEOUT_SECONDS="${DOCS_COMMAND_TIMEOUT_SECONDS:-90}"

run_with_timeout() {
  python3 - "$DOCS_COMMAND_TIMEOUT_SECONDS" "$@" <<'PY'
import subprocess
import sys

timeout = int(sys.argv[1])
args = sys.argv[2:]
try:
    completed = subprocess.run(args, timeout=timeout)
except subprocess.TimeoutExpired:
    print(f"[docs-golden] command timed out after {timeout}s: {' '.join(args)}", file=sys.stderr)
    raise SystemExit(124)
raise SystemExit(completed.returncode)
PY
}

capture_with_timeout() {
  python3 - "$DOCS_COMMAND_TIMEOUT_SECONDS" "$@" <<'PY'
import subprocess
import sys

timeout = int(sys.argv[1])
args = sys.argv[2:]
try:
    completed = subprocess.run(args, timeout=timeout, text=True, capture_output=True)
except subprocess.TimeoutExpired:
    print(f"[docs-golden] command timed out after {timeout}s: {' '.join(args)}", file=sys.stderr)
    raise SystemExit(124)
if completed.stderr:
    print(completed.stderr, file=sys.stderr, end="")
if completed.stdout:
    print(completed.stdout, end="")
raise SystemExit(completed.returncode)
PY
}

if [[ ! -x "$LAB" && "$LAB" == "$ROOT_DIR/rust/target/release/lab" ]]; then
  echo "[docs-golden] building lab" >&2
  (cd "$ROOT_DIR/rust" && cargo build --bin lab --release)
fi

rm -rf "$BUILD_DIR"

echo "[docs-golden] build"
run_with_timeout "$LAB" build "$ROOT_DIR/demos/experiment.yaml" --out "$BUILD_DIR" --json

echo "[docs-golden] preflight"
preflight_json="$(capture_with_timeout "$LAB" preflight "$BUILD_DIR" --json)"
printf '%s\n' "$preflight_json"
preflight_ok="$(python3 - "$preflight_json" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])
print("1" if payload.get("ok") is True else "0")
PY
)"

if [[ "$preflight_ok" != "1" && "${ALLOW_PREFLIGHT_FAILURE:-0}" != "1" ]]; then
  echo "[docs-golden] preflight failed; fix Docker/image/env issues or set ALLOW_PREFLIGHT_FAILURE=1 for docs-only smoke" >&2
  exit 2
fi

if [[ "$RUN_FULL" == "1" ]]; then
  echo "[docs-golden] run"
  run_with_timeout "$LAB" run "$BUILD_DIR" --materialize full --json
else
  echo "[docs-golden] skipping full run; set RUN_FULL=1 to execute trials"
fi
