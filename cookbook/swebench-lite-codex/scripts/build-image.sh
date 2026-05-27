#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
RECIPE_DIR="$ROOT_DIR/cookbook/swebench-lite-codex"

docker build \
  -f "$RECIPE_DIR/docker/Dockerfile.astropy-12907" \
  -t bucephalus/cookbook-swebench-lite-codex-astropy-12907:local \
  "$RECIPE_DIR"

