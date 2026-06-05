#!/usr/bin/env bash
set -euo pipefail

VERSION=""
LATEST="false"
REPO="${GITHUB_REPOSITORY:-}"
WORKFLOW="bucephalus-release.yml"
NEED="promotion"
OUTPUT_PATH=""
PROMOTION_ARTIFACT=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/resolve-cloud-release-artifacts.sh (--version <version>|--latest) [--need release|promotion|ui|both] [--promotion-artifact <selector>] [--repo <owner/repo>] [--workflow <file>] [--github-output <path>]

Resolves a user-facing Cloud release version to GitHub Actions run/artifact
plumbing. The resolver prefers versioned promotion evidence artifacts and keeps
run IDs out of deployment inputs.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --latest)
      LATEST="true"
      shift
      ;;
    --need)
      NEED="${2:-}"
      shift 2
      ;;
    --promotion-artifact)
      PROMOTION_ARTIFACT="${2:-}"
      shift 2
      ;;
    --repo)
      REPO="${2:-}"
      shift 2
      ;;
    --workflow)
      WORKFLOW="${2:-}"
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

if [[ -z "${VERSION}" && "${LATEST}" != "true" ]]; then
  echo "--version or --latest is required" >&2
  exit 2
fi
if [[ -n "${VERSION}" && "${LATEST}" == "true" ]]; then
  echo "--version and --latest are mutually exclusive" >&2
  exit 2
fi
if [[ -z "${REPO}" ]]; then
  echo "--repo or GITHUB_REPOSITORY is required" >&2
  exit 2
fi
case "${NEED}" in
  release|promotion|ui|both) ;;
  *)
    echo "--need must be release, promotion, ui, or both" >&2
    exit 2
    ;;
esac
case "${PROMOTION_ARTIFACT}" in
  ""|cloud-image-promotion-evidence-from-release|cloud-image-promotion-evidence-x86_64-linux) ;;
  *)
    echo "--promotion-artifact must be cloud-image-promotion-evidence-from-release or cloud-image-promotion-evidence-x86_64-linux" >&2
    exit 2
    ;;
esac
if ! command -v gh >/dev/null 2>&1; then
  echo "required command not found: gh" >&2
  exit 2
fi

run_ids="$(
  gh api -X GET "repos/${REPO}/actions/workflows/${WORKFLOW}/runs" \
    -f status=completed \
    -f per_page=100 \
    --jq '.workflow_runs | sort_by(.created_at) | reverse | .[] | select(.status == "completed" and .conclusion == "success") | .id'
)"

release_artifact="bucephalus-${VERSION}-x86_64-unknown-linux-gnu"
ui_artifact="cloud-ui-assets-${VERSION}"
promotion_artifacts=(
  "cloud-image-promotion-evidence-${VERSION}-from-release"
  "cloud-image-promotion-evidence-${VERSION}-x86_64-unknown-linux-gnu"
)
case "${PROMOTION_ARTIFACT}" in
  cloud-image-promotion-evidence-from-release)
    promotion_artifacts=("cloud-image-promotion-evidence-${VERSION}-from-release")
    ;;
  cloud-image-promotion-evidence-x86_64-linux)
    promotion_artifacts=("cloud-image-promotion-evidence-${VERSION}-x86_64-unknown-linux-gnu")
    ;;
esac
legacy_promotion_artifacts=(
  "cloud-image-promotion-evidence-x86_64-unknown-linux-gnu"
)
if [[ "${PROMOTION_ARTIFACT}" == "cloud-image-promotion-evidence-from-release" ]]; then
  legacy_promotion_artifacts=()
fi

release_run_id=""
release_artifact_name=""
promotion_run_id=""
promotion_artifact_name=""
ui_run_id=""
ui_artifact_name=""
resolved_version="${VERSION}"

contains_artifact_name() {
  local needle="$1"
  local name
  while IFS= read -r name; do
    if [[ "${name}" == "${needle}" ]]; then
      return 0
    fi
  done
  return 1
}

artifact_by_regex() {
  local regex="$1"
  local name
  while IFS= read -r name; do
    if [[ "${name}" =~ ${regex} ]]; then
      echo "${name}"
      return 0
    fi
  done
  return 1
}

while IFS= read -r run_id; do
  [[ -n "${run_id}" ]] || continue
  artifact_names="$(
    gh api -X GET "repos/${REPO}/actions/runs/${run_id}/artifacts" \
      -f per_page=100 \
      --jq '.artifacts[] | select(.expired | not) | .name'
  )"
  found_release=""
  found_promotion=""
  found_ui=""
  if [[ "${LATEST}" == "true" ]]; then
    found_release="$(artifact_by_regex '^bucephalus-.+-x86_64-unknown-linux-gnu$' <<< "${artifact_names}" || true)"
    found_ui="$(artifact_by_regex '^cloud-ui-assets-.+$' <<< "${artifact_names}" || true)"
    case "${PROMOTION_ARTIFACT}" in
      cloud-image-promotion-evidence-from-release)
        found_promotion="$(artifact_by_regex '^cloud-image-promotion-evidence-.+-from-release$' <<< "${artifact_names}" || true)"
        if [[ -z "${found_promotion}" ]] && contains_artifact_name "cloud-image-promotion-evidence-from-release" <<< "${artifact_names}"; then
          found_promotion="cloud-image-promotion-evidence-from-release"
        fi
        ;;
      cloud-image-promotion-evidence-x86_64-linux)
        found_promotion="$(artifact_by_regex '^cloud-image-promotion-evidence-.+-x86_64-unknown-linux-gnu$' <<< "${artifact_names}" || true)"
        if [[ -z "${found_promotion}" ]] && contains_artifact_name "cloud-image-promotion-evidence-x86_64-unknown-linux-gnu" <<< "${artifact_names}"; then
          found_promotion="cloud-image-promotion-evidence-x86_64-unknown-linux-gnu"
        fi
        ;;
      *)
        found_promotion="$(artifact_by_regex '^cloud-image-promotion-evidence-.+-(from-release|x86_64-unknown-linux-gnu)$' <<< "${artifact_names}" || true)"
        if [[ -z "${found_promotion}" ]] && contains_artifact_name "cloud-image-promotion-evidence-from-release" <<< "${artifact_names}"; then
          found_promotion="cloud-image-promotion-evidence-from-release"
        fi
        if [[ -z "${found_promotion}" ]] && contains_artifact_name "cloud-image-promotion-evidence-x86_64-unknown-linux-gnu" <<< "${artifact_names}"; then
          found_promotion="cloud-image-promotion-evidence-x86_64-unknown-linux-gnu"
        fi
        ;;
    esac
  else
    if contains_artifact_name "${release_artifact}" <<< "${artifact_names}"; then
      found_release="${release_artifact}"
    fi
    if contains_artifact_name "${ui_artifact}" <<< "${artifact_names}"; then
      found_ui="${ui_artifact}"
    fi
    for candidate in "${promotion_artifacts[@]}"; do
      if contains_artifact_name "${candidate}" <<< "${artifact_names}"; then
        found_promotion="${candidate}"
        break
      fi
    done
    if [[ -z "${found_promotion}" && -n "${found_release}" ]]; then
      for candidate in "${legacy_promotion_artifacts[@]}"; do
        if contains_artifact_name "${candidate}" <<< "${artifact_names}"; then
          found_promotion="${candidate}"
          break
        fi
      done
    fi
  fi
  if [[ -z "${release_run_id}" && -n "${found_release}" ]]; then
    release_run_id="${run_id}"
    release_artifact_name="${found_release}"
    if [[ -z "${resolved_version}" ]]; then
      resolved_version="${found_release#bucephalus-}"
      resolved_version="${resolved_version%-x86_64-unknown-linux-gnu}"
    fi
  fi
  if [[ -z "${promotion_run_id}" && -n "${found_promotion}" ]]; then
    promotion_run_id="${run_id}"
    promotion_artifact_name="${found_promotion}"
    if [[ "${LATEST}" == "true" && "${NEED}" == "promotion" && -n "${found_release}" ]]; then
      release_run_id="${run_id}"
      release_artifact_name="${found_release}"
      resolved_version="${found_release#bucephalus-}"
      resolved_version="${resolved_version%-x86_64-unknown-linux-gnu}"
    elif [[ -z "${resolved_version}" ]]; then
      resolved_version="${found_promotion#cloud-image-promotion-evidence-}"
      resolved_version="${resolved_version%-from-release}"
      resolved_version="${resolved_version%-x86_64-unknown-linux-gnu}"
    fi
  fi
  if [[ -z "${ui_run_id}" && -n "${found_ui}" ]]; then
    ui_run_id="${run_id}"
    ui_artifact_name="${found_ui}"
    if [[ -z "${resolved_version}" ]]; then
      resolved_version="${found_ui#cloud-ui-assets-}"
    fi
  fi
  case "${NEED}" in
    release)
      [[ -n "${release_run_id}" ]] && break
      ;;
    promotion)
      [[ -n "${promotion_run_id}" ]] && break
      ;;
    both)
      [[ -n "${release_run_id}" && -n "${promotion_run_id}" ]] && break
      ;;
    ui)
      [[ -n "${ui_run_id}" ]] && break
      ;;
  esac
done <<< "${run_ids}"

if [[ "${NEED}" == "release" || "${NEED}" == "both" ]]; then
  if [[ -z "${release_run_id}" || -z "${release_artifact_name}" ]]; then
    echo "could not find x86_64 Linux Cloud release artifact for version ${VERSION}" >&2
    exit 1
  fi
fi
if [[ "${NEED}" == "promotion" || "${NEED}" == "both" ]]; then
  if [[ -z "${promotion_run_id}" || -z "${promotion_artifact_name}" ]]; then
    echo "could not find pushed image promotion evidence for release version ${VERSION}" >&2
    exit 1
  fi
fi
if [[ "${NEED}" == "ui" ]]; then
  if [[ -z "${ui_run_id}" || -z "${ui_artifact_name}" ]]; then
    echo "could not find Cloud UI assets for release version ${VERSION}" >&2
    exit 1
  fi
fi

emit_outputs() {
  echo "release_version=${resolved_version}"
  echo "release_run_id=${release_run_id}"
  echo "release_artifact_name=${release_artifact_name}"
  echo "promotion_run_id=${promotion_run_id}"
  echo "promotion_artifact_name=${promotion_artifact_name}"
  echo "ui_run_id=${ui_run_id}"
  echo "ui_artifact_name=${ui_artifact_name}"
}

if [[ -n "${OUTPUT_PATH}" ]]; then
  emit_outputs >> "${OUTPUT_PATH}"
else
  emit_outputs
fi
