#!/usr/bin/env bash
set -euo pipefail

BASE=""
HEAD="HEAD"
OUTPUT_PATH=""

usage() {
  cat <<'USAGE'
Usage: scripts/ci/classify-cloud-candidate-change.sh [--base <ref>] [--head <ref>] [--github-output <path>]

Classifies a main-branch change for the Cloud candidate pipeline.

Outputs:
  build_candidate=true when deployable Cloud images should be rebuilt.
  auto_deploy=true when dev can be updated automatically.
  plan_services=true when the deploy workflow should run plan-only.

High-level policy:
  - runtime/release-bundle changes build images
  - runtime-only changes auto-deploy to dev
  - infra/deploy-boundary changes run plan-only
  - mixed runtime+infra changes build images, then run plan-only with candidate evidence
  - runtime changes bundled with pipeline changes build images, then run plan-only
  - docs/tests/pipeline-only changes do not deploy
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      BASE="${2:-}"
      shift 2
      ;;
    --head)
      HEAD="${2:-}"
      shift 2
      ;;
    --github-output)
      OUTPUT_PATH="${2:-}"
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

if ! command -v git >/dev/null 2>&1; then
  echo "required command not found: git" >&2
  exit 2
fi

if [[ -z "${BASE}" ]]; then
  if git rev-parse --verify "${HEAD}^" >/dev/null 2>&1; then
    BASE="${HEAD}^"
  else
    BASE="$(git hash-object -t tree /dev/null)"
  fi
fi

if ! git rev-parse --verify "${HEAD}" >/dev/null 2>&1; then
  echo "--head does not resolve to a git object: ${HEAD}" >&2
  exit 2
fi
if ! git rev-parse --verify "${BASE}" >/dev/null 2>&1; then
  echo "--base does not resolve to a git object: ${BASE}" >&2
  exit 2
fi

mapfile -t changed_files < <(git diff --name-only "${BASE}" "${HEAD}" -- | sort)

runtime_changed="false"
infra_changed="false"
pipeline_changed="false"
docs_or_tests_changed="false"

runtime_reasons=()
infra_reasons=()
pipeline_reasons=()
docs_reasons=()

add_reason() {
  local kind="$1"
  local file="$2"
  case "${kind}" in
    runtime)
      runtime_changed="true"
      runtime_reasons+=("${file}")
      ;;
    infra)
      infra_changed="true"
      infra_reasons+=("${file}")
      ;;
    pipeline)
      pipeline_changed="true"
      pipeline_reasons+=("${file}")
      ;;
    docs)
      docs_or_tests_changed="true"
      docs_reasons+=("${file}")
      ;;
  esac
}

for file in "${changed_files[@]}"; do
  case "${file}" in
    docs/*|README.md|bucephalus-cloud/README.md|bucephalus-cloud/deploy/*.md|bucephalus-cloud/infra/gcp/*.md|bucephalus-cloud/infra/gcp/environments/*.example|bucephalus-cloud/tests/*|rust/crates/*/tests/*|cookbook/*)
      add_reason docs "${file}"
      ;;
    Cargo.toml|Cargo.lock|rust/*|schemas/*|modal-launcher/*)
      add_reason runtime "${file}"
      ;;
    bucephalus-cloud/src/*|bucephalus-cloud/db/*|bucephalus-cloud/api/openapi/*)
      add_reason runtime "${file}"
      ;;
    bucephalus-cloud/images/*|bucephalus-cloud/deploy/provider/*)
      add_reason runtime "${file}"
      ;;
    bucephalus-cloud/package.json|bucephalus-cloud/package.runtime.json|bucephalus-cloud/bun.lock|bucephalus-cloud/bun.runtime.lock|bucephalus-cloud/tsconfig.json)
      add_reason runtime "${file}"
      ;;
    scripts/release/*|scripts/install.sh)
      add_reason runtime "${file}"
      ;;
    bucephalus-cloud/infra/gcp/*|scripts/deploy/*|.github/workflows/bucephalus-gcp-deploy.yml|.github/workflows/bucephalus-gcp-cleanup.yml)
      add_reason infra "${file}"
      ;;
    .github/workflows/*|scripts/ci/*)
      add_reason pipeline "${file}"
      ;;
    *)
      # Unknown files are treated as runtime-affecting so new source roots do
      # not accidentally skip Cloud validation after being introduced.
      add_reason runtime "${file}"
      ;;
  esac
done

build_candidate="false"
auto_deploy="false"
plan_services="false"
change_class="none"

if [[ "${runtime_changed}" == "true" ]]; then
  build_candidate="true"
  if [[ "${infra_changed}" == "true" && "${pipeline_changed}" == "true" ]]; then
    plan_services="true"
    change_class="mixed-runtime-infra-pipeline"
  elif [[ "${infra_changed}" == "true" ]]; then
    plan_services="true"
    change_class="mixed-runtime-infra"
  elif [[ "${pipeline_changed}" == "true" ]]; then
    plan_services="true"
    change_class="mixed-runtime-pipeline"
  else
    auto_deploy="true"
    change_class="runtime"
  fi
elif [[ "${infra_changed}" == "true" ]]; then
  plan_services="true"
  if [[ "${pipeline_changed}" == "true" ]]; then
    change_class="infra-pipeline"
  else
    change_class="infra"
  fi
elif [[ "${pipeline_changed}" == "true" ]]; then
  change_class="pipeline"
elif [[ "${docs_or_tests_changed}" == "true" ]]; then
  change_class="docs-tests"
fi

join_csv() {
  local IFS=","
  printf '%s' "$*"
}

emit_outputs() {
  echo "base=${BASE}"
  echo "head=${HEAD}"
  echo "changed_files_count=${#changed_files[@]}"
  echo "change_class=${change_class}"
  echo "runtime_changed=${runtime_changed}"
  echo "infra_changed=${infra_changed}"
  echo "pipeline_changed=${pipeline_changed}"
  echo "docs_or_tests_changed=${docs_or_tests_changed}"
  echo "build_candidate=${build_candidate}"
  echo "auto_deploy=${auto_deploy}"
  echo "plan_services=${plan_services}"
  echo "runtime_reasons=$(join_csv "${runtime_reasons[@]}")"
  echo "infra_reasons=$(join_csv "${infra_reasons[@]}")"
  echo "pipeline_reasons=$(join_csv "${pipeline_reasons[@]}")"
  echo "docs_reasons=$(join_csv "${docs_reasons[@]}")"
}

if [[ -n "${OUTPUT_PATH}" ]]; then
  emit_outputs >> "${OUTPUT_PATH}"
else
  emit_outputs
fi
