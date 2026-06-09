#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

sha256_file() {
  local file="$1"
  local digest
  if command -v sha256sum >/dev/null 2>&1; then
    read -r digest _ < <(sha256sum "${file}")
  elif command -v shasum >/dev/null 2>&1; then
    read -r digest _ < <(shasum -a 256 "${file}")
  else
    echo "sha256sum or shasum is required" >&2
    exit 2
  fi
  printf '%s' "${digest}"
}

target_triple() {
  local os arch os_part arch_part
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-gnu" ;;
    *)
      echo "unsupported OS for installer verifier: ${os}" >&2
      exit 2
      ;;
  esac
  case "${arch}" in
    arm64|aarch64) arch_part="aarch64" ;;
    x86_64|amd64) arch_part="x86_64" ;;
    *)
      echo "unsupported architecture for installer verifier: ${arch}" >&2
      exit 2
      ;;
  esac
  printf '%s-%s' "${arch_part}" "${os_part}"
}

write_release_fixture() {
  local release_dir="$1"
  mkdir -p "${release_dir}"
  for bin in bucephalus bucephalus-cloud bucephalus-modal-launcher; do
    {
      printf '%s\n' '#!/bin/sh'
      printf '%s\n' 'case "${1:-}" in'
      printf '%s\n' '  --version) printf "%s\n" "bucephalus 0.0.0-test" ;;'
      printf '%s\n' '  *) printf "%s\n" "fixture binary" ;;'
      printf '%s\n' 'esac'
    } > "${release_dir}/${bin}"
    chmod 0755 "${release_dir}/${bin}"
  done
  printf '%s\n' 'fixture readme' > "${release_dir}/README.md"
  printf '%s\n' 'fixture license' > "${release_dir}/LICENSE"
  printf '%s\n' 'fixture installer' > "${release_dir}/install.sh"
  printf '%s\n' '{"schema_version":"bucephalus_core_release_v1"}' > "${release_dir}/release-manifest.json"
  : > "${release_dir}/SHA256SUMS"
}

write_archive_checksum() {
  local archive="$1"
  local asset="$2"
  printf '%s  %s\n' "$(sha256_file "${archive}")" "${asset}" > "${archive}.sha256"
}

make_valid_dist() {
  local dist="$1"
  local asset="$2"
  local release_dir="${dist}/release"
  mkdir -p "${dist}"
  write_release_fixture "${release_dir}"
  tar -czf "${dist}/${asset}" -C "${release_dir}" \
    bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh README.md LICENSE release-manifest.json SHA256SUMS
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

make_unexpected_member_dist() {
  local dist="$1"
  local asset="$2"
  local release_dir="${dist}/release"
  mkdir -p "${dist}"
  write_release_fixture "${release_dir}"
  mkdir -p "${release_dir}/private/customer-a"
  printf '%s\n' 'OPENAI_API_KEY=raw-secret' > "${release_dir}/private/customer-a/prod-openai-secrets.env"
  tar -czf "${dist}/${asset}" -C "${release_dir}" \
    bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh private/customer-a/prod-openai-secrets.env LICENSE release-manifest.json SHA256SUMS
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

make_duplicate_member_dist() {
  local dist="$1"
  local asset="$2"
  local release_dir="${dist}/release"
  mkdir -p "${dist}"
  write_release_fixture "${release_dir}"
  tar -czf "${dist}/${asset}" -C "${release_dir}" \
    bucephalus bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh LICENSE release-manifest.json SHA256SUMS
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

make_unsafe_member_dist() {
  local dist="$1"
  local asset="$2"
  local release_dir="${dist}/release"
  mkdir -p "${dist}"
  write_release_fixture "${release_dir}"
  printf '%s\n' 'unsafe' > "${release_dir}/private\\token.env"
  tar -czf "${dist}/${asset}" -C "${release_dir}" \
    bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh 'private\token.env' LICENSE release-manifest.json SHA256SUMS
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

make_symlink_member_dist() {
  local dist="$1"
  local asset="$2"
  local release_dir="${dist}/release"
  mkdir -p "${dist}"
  write_release_fixture "${release_dir}"
  rm -f "${release_dir}/README.md"
  ln -s /etc/passwd "${release_dir}/README.md"
  tar -czf "${dist}/${asset}" -C "${release_dir}" \
    bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh README.md LICENSE release-manifest.json SHA256SUMS
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

make_core_traversal_archive() {
  local dist="$1"
  local asset="$2"
  local release_dir="${dist}/release"
  mkdir -p "${dist}"
  write_release_fixture "${release_dir}"
  if tar --version 2>/dev/null | grep -qi "GNU tar"; then
    tar -czf "${dist}/${asset}" --transform='s#^README\.md$#../outside-core-marker#' -C "${release_dir}" \
      bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh README.md LICENSE release-manifest.json SHA256SUMS
  else
    tar -czf "${dist}/${asset}" -C "${release_dir}" -s '#^README\.md$#../outside-core-marker#' \
      bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh README.md LICENSE release-manifest.json SHA256SUMS
  fi
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

make_cloud_traversal_archive() {
  local dist="$1"
  local asset="$2"
  local release_name="bucephalus-0.0.0-test-x86_64-unknown-linux-gnu"
  local release_dir="${dist}/${release_name}"
  mkdir -p "${release_dir}/bin"
  printf '%s\n' '#!/bin/sh' 'printf "%s\n" "fixture bucephalus"' > "${release_dir}/bin/bucephalus"
  chmod 0755 "${release_dir}/bin/bucephalus"
  printf '%s\n' '{"schema_version":"bucephalus_release_v1"}' > "${release_dir}/release-manifest.json"
  printf '%s\n' 'placeholder' > "${release_dir}/SHA256SUMS"
  if tar --version 2>/dev/null | grep -qi "GNU tar"; then
    tar -czf "${dist}/${asset}" --transform="s#^${release_name}/release-manifest\\.json\$#${release_name}/../outside-cloud-marker#" -C "${dist}" \
      "${release_name}"
  else
    tar -czf "${dist}/${asset}" -C "${dist}" -s "#^${release_name}/release-manifest\\.json\$#${release_name}/../outside-cloud-marker#" \
      "${release_name}"
  fi
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

make_malformed_checksum_dist() {
  local dist="$1"
  local asset="$2"
  make_valid_dist "${dist}" "${asset}"
  printf '%s  %s\n' "not-a-digest" "${asset}" > "${dist}/${asset}.sha256"
}

make_setup_failing_dist() {
  local dist="$1"
  local asset="$2"
  local release_dir="${dist}/release"
  mkdir -p "${dist}"
  write_release_fixture "${release_dir}"
  {
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'case "${1:-}" in'
    printf '%s\n' '  --version) printf "%s\n" "bucephalus 0.0.0-test" ;;'
    printf '%s\n' '  setup) printf "%s\n" "setup failed: daemon socket unavailable" >&2; exit 42 ;;'
    printf '%s\n' '  *) printf "%s\n" "fixture binary" ;;'
    printf '%s\n' 'esac'
  } > "${release_dir}/bucephalus"
  chmod 0755 "${release_dir}/bucephalus"
  tar -czf "${dist}/${asset}" -C "${release_dir}" \
    bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh README.md LICENSE release-manifest.json SHA256SUMS
  write_archive_checksum "${dist}/${asset}" "${asset}"
}

run_installer() {
  local dist="$1"
  local install_dir="$2"
  BUCEPHALUS_BASE_URL="file://${dist}" \
    BUCEPHALUS_INSTALL_DIR="${install_dir}" \
    BUCEPHALUS_NO_MODIFY_PATH=1 \
    BUCEPHALUS_SETUP=0 \
    sh "${ROOT_DIR}/scripts/install.sh"
}

run_installer_with_setup() {
  local dist="$1"
  local install_dir="$2"
  BUCEPHALUS_BASE_URL="file://${dist}" \
    BUCEPHALUS_INSTALL_DIR="${install_dir}" \
    BUCEPHALUS_NO_MODIFY_PATH=1 \
    BUCEPHALUS_SETUP=1 \
    sh "${ROOT_DIR}/scripts/install.sh"
}

run_installer_with_profile_edit() {
  local dist="$1"
  local install_dir="$2"
  local home_dir="$3"
  BUCEPHALUS_BASE_URL="file://${dist}" \
    BUCEPHALUS_INSTALL_DIR="${install_dir}" \
    BUCEPHALUS_SETUP=0 \
    HOME="${home_dir}" \
    SHELL=/bin/bash \
    sh "${ROOT_DIR}/scripts/install.sh"
}

path_contains_entry() {
  local expected="$1"
  local old_ifs entry
  old_ifs="${IFS}"
  IFS=:
  for entry in ${PATH}; do
    if [[ "${entry}" == "${expected}" ]]; then
      IFS="${old_ifs}"
      return 0
    fi
  done
  IFS="${old_ifs}"
  return 1
}

expect_installer_failure() {
  local label="$1"
  local dist="$2"
  local expected="$3"
  local log="${WORK_DIR}/${label}.log"
  local install_dir="${WORK_DIR}/${label}-install"
  if run_installer "${dist}" "${install_dir}" > "${log}" 2>&1; then
    echo "installer unexpectedly accepted ${label} archive" >&2
    cat "${log}" >&2
    exit 1
  fi
  if ! grep -Fq "${expected}" "${log}"; then
    echo "installer failure for ${label} did not include expected message: ${expected}" >&2
    cat "${log}" >&2
    exit 1
  fi
  if grep -Fq "file://${dist}" "${log}"; then
    echo "installer failure for ${label} leaked a local mirror path" >&2
    cat "${log}" >&2
    exit 1
  fi
}

require_command curl
require_command bun
require_command grep
require_command install
require_command mktemp
require_command tar

if grep -Fq '.bucephalus-install-staging.$$' "${ROOT_DIR}/scripts/install.sh"; then
  echo "installer must not use a predictable process-id staging directory" >&2
  exit 1
fi
if ! grep -Fq 'mktemp -d "${install_dir}/.bucephalus-install-staging.XXXXXX"' "${ROOT_DIR}/scripts/install.sh"; then
  echo "installer must create staging with mktemp inside the install directory" >&2
  exit 1
fi
if ! grep -Fq 'chmod 700 "$staging_dir"' "${ROOT_DIR}/scripts/install.sh"; then
  echo "installer must make the staging directory private before copying artifacts" >&2
  exit 1
fi
if ! grep -Fq '[ -L "$rc" ]' "${ROOT_DIR}/scripts/install.sh" || ! grep -Fq '[ -L "$fish_file" ]' "${ROOT_DIR}/scripts/install.sh"; then
  echo "installer must not follow symlinked shell profile files while editing PATH" >&2
  exit 1
fi
if ! grep -Fq '[ -L "$rc_dir" ]' "${ROOT_DIR}/scripts/install.sh" || ! grep -Fq '[ -L "$fish_conf" ]' "${ROOT_DIR}/scripts/install.sh"; then
  echo "installer must not write through symlinked shell profile directories while editing PATH" >&2
  exit 1
fi
if ! grep -Fq 'Skipping shell profile $(profile_ref "$rc") because it is a symlink.' "${ROOT_DIR}/scripts/install.sh"; then
  echo "installer must report symlinked shell profile skips through public profile refs" >&2
  exit 1
fi
if ! grep -Fq 'Skipping shell profile $(profile_ref "$rc") because its directory is a symlink.' "${ROOT_DIR}/scripts/install.sh"; then
  echo "installer must report symlinked shell profile directory skips through public profile refs" >&2
  exit 1
fi

ASSET="bucephalus-$(target_triple).tar.gz"

core_traversal_dist="${WORK_DIR}/core-traversal"
core_traversal_tmp="${WORK_DIR}/core-traversal-tmp"
core_traversal_log="${WORK_DIR}/core-traversal.log"
mkdir -p "${core_traversal_tmp}"
make_core_traversal_archive "${core_traversal_dist}" "${ASSET}"
if TMPDIR="${core_traversal_tmp}" "${ROOT_DIR}/scripts/release/verify-core-release.sh" "${core_traversal_dist}/${ASSET}" > "${core_traversal_log}" 2>&1; then
  echo "core release verifier unexpectedly accepted a traversal archive" >&2
  cat "${core_traversal_log}" >&2
  exit 1
fi
if [[ -e "${core_traversal_tmp}/outside-core-marker" ]]; then
  echo "core release verifier extracted a traversal member outside its work directory" >&2
  cat "${core_traversal_log}" >&2
  exit 1
fi
if ! grep -Fq "unsafe core release archive member path" "${core_traversal_log}"; then
  echo "core release verifier did not reject traversal before extraction with a clear error" >&2
  cat "${core_traversal_log}" >&2
  exit 1
fi
if grep -Fq "${core_traversal_tmp}" "${core_traversal_log}" || grep -Fq "outside-core-marker" "${core_traversal_log}"; then
  echo "core release verifier leaked traversal archive internals in its failure output" >&2
  cat "${core_traversal_log}" >&2
  exit 1
fi

core_provenance_traversal_tmp="${WORK_DIR}/core-provenance-traversal-tmp"
core_provenance_traversal_log="${WORK_DIR}/core-provenance-traversal.log"
core_provenance_fixture="${WORK_DIR}/core-provenance-placeholder.json"
mkdir -p "${core_provenance_traversal_tmp}"
printf '%s\n' '{}' > "${core_provenance_fixture}"
if TMPDIR="${core_provenance_traversal_tmp}" "${ROOT_DIR}/scripts/release/verify-core-release-provenance.sh" "${core_provenance_fixture}" --release "${core_traversal_dist}/${ASSET}" > "${core_provenance_traversal_log}" 2>&1; then
  echo "core provenance verifier unexpectedly accepted a traversal release archive" >&2
  cat "${core_provenance_traversal_log}" >&2
  exit 1
fi
if [[ -e "${core_provenance_traversal_tmp}/outside-core-marker" ]]; then
  echo "core provenance verifier extracted a traversal member outside its work directory" >&2
  cat "${core_provenance_traversal_log}" >&2
  exit 1
fi
if ! grep -Fq "unsafe core release archive member path" "${core_provenance_traversal_log}"; then
  echo "core provenance verifier did not reject traversal before extraction with a clear error" >&2
  cat "${core_provenance_traversal_log}" >&2
  exit 1
fi
if grep -Fq "${core_provenance_traversal_tmp}" "${core_provenance_traversal_log}" || grep -Fq "outside-core-marker" "${core_provenance_traversal_log}"; then
  echo "core provenance verifier leaked traversal archive internals in its failure output" >&2
  cat "${core_provenance_traversal_log}" >&2
  exit 1
fi

cloud_traversal_asset="bucephalus-0.0.0-test-x86_64-unknown-linux-gnu.tar.gz"
cloud_traversal_dist="${WORK_DIR}/cloud-traversal"
cloud_traversal_tmp="${WORK_DIR}/cloud-traversal-tmp"
cloud_traversal_log="${WORK_DIR}/cloud-traversal.log"
mkdir -p "${cloud_traversal_dist}" "${cloud_traversal_tmp}"
make_cloud_traversal_archive "${cloud_traversal_dist}" "${cloud_traversal_asset}"
if TMPDIR="${cloud_traversal_tmp}" "${ROOT_DIR}/scripts/release/verify-buc-release.sh" "${cloud_traversal_dist}/${cloud_traversal_asset}" > "${cloud_traversal_log}" 2>&1; then
  echo "Cloud release verifier unexpectedly accepted a traversal archive" >&2
  cat "${cloud_traversal_log}" >&2
  exit 1
fi
if [[ -e "${cloud_traversal_tmp}/outside-cloud-marker" ]]; then
  echo "Cloud release verifier extracted a traversal member outside its work directory" >&2
  cat "${cloud_traversal_log}" >&2
  exit 1
fi
if ! grep -Fq "unsafe cloud release archive member path" "${cloud_traversal_log}"; then
  echo "Cloud release verifier did not reject traversal before extraction with a clear error" >&2
  cat "${cloud_traversal_log}" >&2
  exit 1
fi
if grep -Fq "${cloud_traversal_tmp}" "${cloud_traversal_log}" || grep -Fq "outside-cloud-marker" "${cloud_traversal_log}"; then
  echo "Cloud release verifier leaked traversal archive internals in its failure output" >&2
  cat "${cloud_traversal_log}" >&2
  exit 1
fi

cloud_provenance_traversal_tmp="${WORK_DIR}/cloud-provenance-traversal-tmp"
cloud_provenance_traversal_log="${WORK_DIR}/cloud-provenance-traversal.log"
cloud_provenance_fixture="${WORK_DIR}/cloud-provenance-placeholder.json"
mkdir -p "${cloud_provenance_traversal_tmp}"
printf '%s\n' '{}' > "${cloud_provenance_fixture}"
if TMPDIR="${cloud_provenance_traversal_tmp}" "${ROOT_DIR}/scripts/release/verify-cloud-release-provenance.sh" "${cloud_provenance_fixture}" --release "${cloud_traversal_dist}/${cloud_traversal_asset}" > "${cloud_provenance_traversal_log}" 2>&1; then
  echo "Cloud provenance verifier unexpectedly accepted a traversal release archive" >&2
  cat "${cloud_provenance_traversal_log}" >&2
  exit 1
fi
if [[ -e "${cloud_provenance_traversal_tmp}/outside-cloud-marker" ]]; then
  echo "Cloud provenance verifier extracted a traversal member outside its work directory" >&2
  cat "${cloud_provenance_traversal_log}" >&2
  exit 1
fi
if ! grep -Fq "unsafe cloud release archive member path" "${cloud_provenance_traversal_log}"; then
  echo "Cloud provenance verifier did not reject traversal before extraction with a clear error" >&2
  cat "${cloud_provenance_traversal_log}" >&2
  exit 1
fi
if grep -Fq "${cloud_provenance_traversal_tmp}" "${cloud_provenance_traversal_log}" || grep -Fq "outside-cloud-marker" "${cloud_provenance_traversal_log}"; then
  echo "Cloud provenance verifier leaked traversal archive internals in its failure output" >&2
  cat "${cloud_provenance_traversal_log}" >&2
  exit 1
fi

valid_dist="${WORK_DIR}/valid"
make_valid_dist "${valid_dist}" "${ASSET}"
run_installer "${valid_dist}" "${WORK_DIR}/valid-install" > "${WORK_DIR}/valid.log" 2>&1
test -x "${WORK_DIR}/valid-install/bucephalus"
test -x "${WORK_DIR}/valid-install/bucephalus-cloud"
test -x "${WORK_DIR}/valid-install/bucephalus-modal-launcher"
test -f "${WORK_DIR}/valid-install/bucephalus-install.sh"
if compgen -G "${WORK_DIR}/valid-install/.bucephalus-install-staging.*" >/dev/null; then
  echo "installer left staging directories behind after a successful install" >&2
  find "${WORK_DIR}/valid-install" -maxdepth 1 -name '.bucephalus-install-staging.*' >&2
  exit 1
fi
if ! grep -Fq "Downloading file://[REDACTED:local-path]" "${WORK_DIR}/valid.log"; then
  echo "installer did not redact local mirror paths in download output" >&2
  cat "${WORK_DIR}/valid.log" >&2
  exit 1
fi
if grep -Fq "Downloading file://${valid_dist}" "${WORK_DIR}/valid.log"; then
  echo "installer leaked a local mirror path in download output" >&2
  cat "${WORK_DIR}/valid.log" >&2
  exit 1
fi

missing_archive_dist="${WORK_DIR}/missing-archive"
missing_archive_log="${WORK_DIR}/missing-archive.log"
mkdir -p "${missing_archive_dist}"
if run_installer "${missing_archive_dist}" "${WORK_DIR}/missing-archive-install" > "${missing_archive_log}" 2>&1; then
  echo "installer unexpectedly succeeded with a missing local release archive" >&2
  cat "${missing_archive_log}" >&2
  exit 1
fi
if ! grep -Fq "failed to download release archive: file://[REDACTED:local-path]" "${missing_archive_log}"; then
  echo "installer did not report missing local release archives through a redacted URL" >&2
  cat "${missing_archive_log}" >&2
  exit 1
fi
if grep -Fq "${missing_archive_dist}" "${missing_archive_log}" || grep -Fq "file://${missing_archive_dist}" "${missing_archive_log}"; then
  echo "installer leaked the local mirror path on archive download failure" >&2
  cat "${missing_archive_log}" >&2
  exit 1
fi

missing_checksum_dist="${WORK_DIR}/missing-checksum-file"
missing_checksum_log="${WORK_DIR}/missing-checksum-file.log"
make_valid_dist "${missing_checksum_dist}" "${ASSET}"
rm -f "${missing_checksum_dist}/${ASSET}.sha256"
if run_installer "${missing_checksum_dist}" "${WORK_DIR}/missing-checksum-install" > "${missing_checksum_log}" 2>&1; then
  echo "installer unexpectedly succeeded with a missing local checksum file" >&2
  cat "${missing_checksum_log}" >&2
  exit 1
fi
if ! grep -Fq "failed to download checksum file: file://[REDACTED:local-path]" "${missing_checksum_log}"; then
  echo "installer did not report missing local checksum files through a redacted URL" >&2
  cat "${missing_checksum_log}" >&2
  exit 1
fi
if grep -Fq "${missing_checksum_dist}" "${missing_checksum_log}" || grep -Fq "file://${missing_checksum_dist}" "${missing_checksum_log}"; then
  echo "installer leaked the local mirror path on checksum download failure" >&2
  cat "${missing_checksum_log}" >&2
  exit 1
fi

help_log="${WORK_DIR}/help.log"
BUCEPHALUS_REPO="owner/private?token=raw-help-token" \
  sh "${ROOT_DIR}/scripts/install.sh" --help > "${help_log}" 2>&1
if ! grep -Fq "nishiokj/Bucephalus" "${help_log}"; then
  echo "installer help did not render the public default repo" >&2
  cat "${help_log}" >&2
  exit 1
fi
if grep -Fq "raw-help-token" "${help_log}" || grep -Fq "owner/private?token" "${help_log}"; then
  echo "installer help leaked BUCEPHALUS_REPO from the caller environment" >&2
  cat "${help_log}" >&2
  exit 1
fi

unknown_arg_log="${WORK_DIR}/unknown-arg.log"
if sh "${ROOT_DIR}/scripts/install.sh" "token=raw-arg-token" > "${unknown_arg_log}" 2>&1; then
  echo "installer unexpectedly accepted an unknown argument" >&2
  cat "${unknown_arg_log}" >&2
  exit 1
fi
if ! grep -Fq "unknown installer argument" "${unknown_arg_log}"; then
  echo "installer did not report unknown arguments with curated copy" >&2
  cat "${unknown_arg_log}" >&2
  exit 1
fi
if grep -Fq "raw-arg-token" "${unknown_arg_log}" || grep -Fq "token=" "${unknown_arg_log}"; then
  echo "installer unknown-argument error leaked the raw argument" >&2
  cat "${unknown_arg_log}" >&2
  exit 1
fi

invalid_base_log="${WORK_DIR}/invalid-base-url.log"
if BUCEPHALUS_BASE_URL="file://${valid_dist}?token=raw-base-token#frag" \
  BUCEPHALUS_INSTALL_DIR="${WORK_DIR}/invalid-base-install" \
  sh "${ROOT_DIR}/scripts/install.sh" > "${invalid_base_log}" 2>&1; then
  echo "installer unexpectedly accepted a query-bearing BUCEPHALUS_BASE_URL" >&2
  cat "${invalid_base_log}" >&2
  exit 1
fi
if ! grep -Fq "invalid BUCEPHALUS_BASE_URL" "${invalid_base_log}"; then
  echo "installer did not reject query-bearing BUCEPHALUS_BASE_URL with a clear error" >&2
  cat "${invalid_base_log}" >&2
  exit 1
fi
if grep -Fq "${valid_dist}" "${invalid_base_log}" || grep -Fq "raw-base-token" "${invalid_base_log}" || grep -Fq "?token=" "${invalid_base_log}" || grep -Fq "#frag" "${invalid_base_log}"; then
  echo "installer invalid-base-url error leaked caller mirror details" >&2
  cat "${invalid_base_log}" >&2
  exit 1
fi

invalid_credential_base_log="${WORK_DIR}/invalid-credential-base-url.log"
if BUCEPHALUS_BASE_URL="https://user:raw-base-secret@example.com/releases" \
  BUCEPHALUS_INSTALL_DIR="${WORK_DIR}/invalid-credential-base-install" \
  sh "${ROOT_DIR}/scripts/install.sh" > "${invalid_credential_base_log}" 2>&1; then
  echo "installer unexpectedly accepted a credential-bearing BUCEPHALUS_BASE_URL" >&2
  cat "${invalid_credential_base_log}" >&2
  exit 1
fi
if ! grep -Fq "invalid BUCEPHALUS_BASE_URL" "${invalid_credential_base_log}"; then
  echo "installer did not reject credential-bearing BUCEPHALUS_BASE_URL with a clear error" >&2
  cat "${invalid_credential_base_log}" >&2
  exit 1
fi
if grep -Fq "raw-base-secret" "${invalid_credential_base_log}" || grep -Fq "user:" "${invalid_credential_base_log}"; then
  echo "installer invalid credential base URL error leaked URL credentials" >&2
  cat "${invalid_credential_base_log}" >&2
  exit 1
fi

invalid_repo_log="${WORK_DIR}/invalid-repo.log"
if BUCEPHALUS_REPO="owner/private?token=raw-repo-token" \
  BUCEPHALUS_INSTALL_DIR="${WORK_DIR}/invalid-repo-install" \
  sh "${ROOT_DIR}/scripts/install.sh" > "${invalid_repo_log}" 2>&1; then
  echo "installer unexpectedly accepted an unsafe BUCEPHALUS_REPO" >&2
  cat "${invalid_repo_log}" >&2
  exit 1
fi
if ! grep -Fq "invalid BUCEPHALUS_REPO" "${invalid_repo_log}"; then
  echo "installer did not reject unsafe BUCEPHALUS_REPO with a clear error" >&2
  cat "${invalid_repo_log}" >&2
  exit 1
fi
if grep -Fq "raw-repo-token" "${invalid_repo_log}" || grep -Fq "owner/private?token" "${invalid_repo_log}"; then
  echo "installer invalid repo error leaked the raw repo value" >&2
  cat "${invalid_repo_log}" >&2
  exit 1
fi

invalid_version_log="${WORK_DIR}/invalid-version.log"
if BUCEPHALUS_VERSION="1.2.3?token=raw-version-token" \
  BUCEPHALUS_INSTALL_DIR="${WORK_DIR}/invalid-version-install" \
  sh "${ROOT_DIR}/scripts/install.sh" > "${invalid_version_log}" 2>&1; then
  echo "installer unexpectedly accepted an unsafe BUCEPHALUS_VERSION" >&2
  cat "${invalid_version_log}" >&2
  exit 1
fi
if ! grep -Fq "invalid BUCEPHALUS_VERSION" "${invalid_version_log}"; then
  echo "installer did not reject unsafe BUCEPHALUS_VERSION with a clear error" >&2
  cat "${invalid_version_log}" >&2
  exit 1
fi
if grep -Fq "raw-version-token" "${invalid_version_log}" || grep -Fq "1.2.3?token" "${invalid_version_log}"; then
  echo "installer invalid version error leaked the raw version value" >&2
  cat "${invalid_version_log}" >&2
  exit 1
fi

staging_observer="${WORK_DIR}/staging-observer"
staging_observed="${WORK_DIR}/staging-observed.txt"
staging_wrapper_dir="${WORK_DIR}/staging-wrapper"
real_install="$(command -v install)"
mkdir -p "${staging_wrapper_dir}"
cat > "${staging_wrapper_dir}/install" <<'EOF'
#!/bin/sh
staging="$(find "${BUCEPHALUS_INSTALL_DIR}" -maxdepth 1 -type d -name '.bucephalus-install-staging.*' | head -n 1)"
if [ -n "$staging" ]; then
  mode="$(stat -c '%a' "$staging" 2>/dev/null || stat -f '%Lp' "$staging")"
  printf '%s\n' "$staging $mode" > "${BUCEPHALUS_STAGING_OBSERVED}"
fi
exec "${REAL_INSTALL}" "$@"
EOF
chmod 0755 "${staging_wrapper_dir}/install"
make_valid_dist "${staging_observer}" "${ASSET}"
(
  export PATH="${staging_wrapper_dir}:${PATH}"
  export REAL_INSTALL="${real_install}"
  export BUCEPHALUS_STAGING_OBSERVED="${staging_observed}"
  run_installer "${staging_observer}" "${WORK_DIR}/staging-observer-install"
) > "${WORK_DIR}/staging-observer.log" 2>&1
if [[ ! -f "${staging_observed}" ]]; then
  echo "installer verifier did not observe the staging directory during version check" >&2
  cat "${WORK_DIR}/staging-observer.log" >&2
  exit 1
fi
read -r observed_staging observed_mode < "${staging_observed}"
if [[ "${observed_mode}" != "700" ]]; then
  echo "installer staging directory was not private while artifacts were staged: ${observed_mode}" >&2
  cat "${staging_observed}" >&2
  exit 1
fi
case "${observed_staging}" in
  "${WORK_DIR}/staging-observer-install/.bucephalus-install-staging."??????) ;;
  *)
    echo "installer staging directory did not use mktemp-style random suffix" >&2
    cat "${staging_observed}" >&2
    exit 1
    ;;
esac
if compgen -G "${WORK_DIR}/staging-observer-install/.bucephalus-install-staging.*" >/dev/null; then
  echo "installer left staging directories behind after staging observer install" >&2
  find "${WORK_DIR}/staging-observer-install" -maxdepth 1 -name '.bucephalus-install-staging.*' >&2
  exit 1
fi

non_file_install="${WORK_DIR}/non-file-install"
mkdir -p "${non_file_install}/bucephalus-cloud"
{
  printf '%s\n' '#!/bin/sh'
  printf '%s\n' 'printf "%s\n" "old bucephalus"'
} > "${non_file_install}/bucephalus"
chmod 0755 "${non_file_install}/bucephalus"
if run_installer "${valid_dist}" "${non_file_install}" > "${WORK_DIR}/non-file.log" 2>&1; then
  echo "installer unexpectedly accepted a directory where bucephalus-cloud should be installed" >&2
  cat "${WORK_DIR}/non-file.log" >&2
  exit 1
fi
if ! grep -Fq "install target exists but is a directory" "${WORK_DIR}/non-file.log"; then
  echo "installer did not explain the non-file install target" >&2
  cat "${WORK_DIR}/non-file.log" >&2
  exit 1
fi
if ! grep -Fq "target_ref: install-dir://bucephalus-cloud" "${WORK_DIR}/non-file.log"; then
  echo "installer did not identify the non-file target with a public install ref" >&2
  cat "${WORK_DIR}/non-file.log" >&2
  exit 1
fi
if grep -Fq "${non_file_install}" "${WORK_DIR}/non-file.log"; then
  echo "installer leaked the local install directory while reporting a non-file target" >&2
  cat "${WORK_DIR}/non-file.log" >&2
  exit 1
fi
if [[ "$("${non_file_install}/bucephalus")" != "old bucephalus" ]]; then
  echo "installer modified an existing binary before rejecting the non-file target" >&2
  cat "${WORK_DIR}/non-file.log" >&2
  exit 1
fi
if compgen -G "${non_file_install}/.bucephalus-install-staging.*" >/dev/null; then
  echo "installer left staging directories behind after rejecting a non-file target" >&2
  find "${non_file_install}" -maxdepth 1 -name '.bucephalus-install-staging.*' >&2
  exit 1
fi

profile_home="${WORK_DIR}/profile-home"
profile_injection_marker="${WORK_DIR}/profile-injection-ran"
profile_install_dir="${WORK_DIR}/profile install/\$(touch ${profile_injection_marker})/bin"
run_installer_with_profile_edit "${valid_dist}" "${profile_install_dir}" "${profile_home}" > "${WORK_DIR}/profile.log" 2>&1
test -x "${profile_install_dir}/bucephalus"
if ! grep -Fq "Updated shell profile shell-profile://bashrc" "${WORK_DIR}/profile.log"; then
  echo "installer did not report bashrc profile edits with a public profile ref" >&2
  cat "${WORK_DIR}/profile.log" >&2
  exit 1
fi
if ! grep -Fq "Updated shell profile shell-profile://bash-profile" "${WORK_DIR}/profile.log"; then
  echo "installer did not report bash_profile profile edits with a public profile ref" >&2
  cat "${WORK_DIR}/profile.log" >&2
  exit 1
fi
if grep -Fq "${profile_home}" "${WORK_DIR}/profile.log"; then
  echo "installer leaked the shell profile home path in profile-edit output" >&2
  cat "${WORK_DIR}/profile.log" >&2
  exit 1
fi
if [[ -e "${profile_injection_marker}" ]]; then
  echo "installer executed profile injection text during install" >&2
  cat "${WORK_DIR}/profile.log" >&2
  exit 1
fi
PATH="/usr/bin:/bin"
# shellcheck source=/dev/null
. "${profile_home}/.bashrc"
if [[ -e "${profile_injection_marker}" ]]; then
  echo "installer wrote an unsafe shell profile PATH snippet" >&2
  cat "${profile_home}/.bashrc" >&2
  exit 1
fi
if ! path_contains_entry "${profile_install_dir}"; then
  echo "installer profile snippet did not add the exact install directory to PATH" >&2
  cat "${profile_home}/.bashrc" >&2
  exit 1
fi

stale_profile_home="${WORK_DIR}/stale-profile-home"
stale_old_install="${WORK_DIR}/old-install/bin"
stale_new_install="${WORK_DIR}/new install/bin"
mkdir -p "${stale_profile_home}"
{
  printf '%s\n' "# added by bucephalus installer"
  printf '%s\n' "export PATH='${stale_old_install}':\$PATH"
} > "${stale_profile_home}/.bashrc"
cp "${stale_profile_home}/.bashrc" "${stale_profile_home}/.bash_profile"
run_installer_with_profile_edit "${valid_dist}" "${stale_new_install}" "${stale_profile_home}" > "${WORK_DIR}/stale-profile.log" 2>&1
test -x "${stale_new_install}/bucephalus"
if ! grep -Fq "Updated shell profile shell-profile://bashrc" "${WORK_DIR}/stale-profile.log"; then
  echo "installer did not update stale bashrc profile for a changed install directory" >&2
  cat "${WORK_DIR}/stale-profile.log" >&2
  exit 1
fi
if ! grep -Fq "Updated shell profile shell-profile://bash-profile" "${WORK_DIR}/stale-profile.log"; then
  echo "installer did not update stale bash_profile for a changed install directory" >&2
  cat "${WORK_DIR}/stale-profile.log" >&2
  exit 1
fi
if ! grep -Fq "export PATH='${stale_new_install}':\$PATH" "${stale_profile_home}/.bashrc"; then
  echo "installer stale profile update did not add the new install directory" >&2
  cat "${stale_profile_home}/.bashrc" >&2
  exit 1
fi
PATH="/usr/bin:/bin"
# shellcheck source=/dev/null
. "${stale_profile_home}/.bashrc"
if ! path_contains_entry "${stale_new_install}"; then
  echo "installer stale profile snippet did not make the new install directory active" >&2
  cat "${stale_profile_home}/.bashrc" >&2
  exit 1
fi
if grep -Fq "${stale_profile_home}" "${WORK_DIR}/stale-profile.log"; then
  echo "installer leaked stale shell profile home path in profile-edit output" >&2
  cat "${WORK_DIR}/stale-profile.log" >&2
  exit 1
fi

symlink_profile_home="${WORK_DIR}/symlink-profile-home"
symlink_profile_install="${WORK_DIR}/symlink-profile-install/bin"
symlink_profile_targets="${WORK_DIR}/symlink-profile-targets"
mkdir -p "${symlink_profile_home}" "${symlink_profile_targets}"
printf '%s\n' "existing bashrc" > "${symlink_profile_targets}/bashrc-target"
printf '%s\n' "existing bash profile" > "${symlink_profile_targets}/bash-profile-target"
ln -s "${symlink_profile_targets}/bashrc-target" "${symlink_profile_home}/.bashrc"
ln -s "${symlink_profile_targets}/bash-profile-target" "${symlink_profile_home}/.bash_profile"
run_installer_with_profile_edit "${valid_dist}" "${symlink_profile_install}" "${symlink_profile_home}" > "${WORK_DIR}/symlink-profile.log" 2>&1
test -x "${symlink_profile_install}/bucephalus"
if ! grep -Fq "Skipping shell profile shell-profile://bashrc because it is a symlink." "${WORK_DIR}/symlink-profile.log"; then
  echo "installer did not skip symlinked bashrc with a public profile ref" >&2
  cat "${WORK_DIR}/symlink-profile.log" >&2
  exit 1
fi
if ! grep -Fq "Skipping shell profile shell-profile://bash-profile because it is a symlink." "${WORK_DIR}/symlink-profile.log"; then
  echo "installer did not skip symlinked bash_profile with a public profile ref" >&2
  cat "${WORK_DIR}/symlink-profile.log" >&2
  exit 1
fi
if grep -Fq "${symlink_profile_home}" "${WORK_DIR}/symlink-profile.log" || grep -Fq "${symlink_profile_targets}" "${WORK_DIR}/symlink-profile.log"; then
  echo "installer leaked symlinked shell profile paths in profile-edit output" >&2
  cat "${WORK_DIR}/symlink-profile.log" >&2
  exit 1
fi
if grep -Fq "# added by bucephalus installer" "${symlink_profile_targets}/bashrc-target" || grep -Fq "${symlink_profile_install}" "${symlink_profile_targets}/bashrc-target"; then
  echo "installer followed and modified a symlinked bashrc target" >&2
  cat "${symlink_profile_targets}/bashrc-target" >&2
  exit 1
fi
if grep -Fq "# added by bucephalus installer" "${symlink_profile_targets}/bash-profile-target" || grep -Fq "${symlink_profile_install}" "${symlink_profile_targets}/bash-profile-target"; then
  echo "installer followed and modified a symlinked bash_profile target" >&2
  cat "${symlink_profile_targets}/bash-profile-target" >&2
  exit 1
fi
if ! grep -Fq "Next: run '${symlink_profile_install}/bucephalus' setup" "${WORK_DIR}/symlink-profile.log"; then
  echo "installer did not provide absolute-path setup guidance after skipping symlinked shell profiles" >&2
  cat "${WORK_DIR}/symlink-profile.log" >&2
  exit 1
fi

symlink_profile_dir_home="${WORK_DIR}/symlink-profile-dir-home"
symlink_profile_dir_config="${WORK_DIR}/symlink-profile-dir-config"
symlink_profile_dir_target="${WORK_DIR}/symlink-profile-dir-target"
symlink_profile_dir_install="${WORK_DIR}/symlink-profile-dir-install/bin"
mkdir -p "${symlink_profile_dir_home}" "${symlink_profile_dir_config}/fish" "${symlink_profile_dir_target}"
printf '%s\n' "existing fish config" > "${symlink_profile_dir_target}/bucephalus.fish"
ln -s "${symlink_profile_dir_target}" "${symlink_profile_dir_config}/fish/conf.d"
BUCEPHALUS_BASE_URL="file://${valid_dist}" \
  BUCEPHALUS_INSTALL_DIR="${symlink_profile_dir_install}" \
  BUCEPHALUS_SETUP=0 \
  HOME="${symlink_profile_dir_home}" \
  XDG_CONFIG_HOME="${symlink_profile_dir_config}" \
  SHELL=/bin/fish \
  sh "${ROOT_DIR}/scripts/install.sh" > "${WORK_DIR}/symlink-profile-dir.log" 2>&1
test -x "${symlink_profile_dir_install}/bucephalus"
if ! grep -Fq "Skipping shell profile shell-profile://fish/conf.d/bucephalus.fish because its directory is a symlink." "${WORK_DIR}/symlink-profile-dir.log"; then
  echo "installer did not skip symlinked fish profile directory with a public profile ref" >&2
  cat "${WORK_DIR}/symlink-profile-dir.log" >&2
  exit 1
fi
if grep -Fq "${symlink_profile_dir_home}" "${WORK_DIR}/symlink-profile-dir.log" || grep -Fq "${symlink_profile_dir_config}" "${WORK_DIR}/symlink-profile-dir.log" || grep -Fq "${symlink_profile_dir_target}" "${WORK_DIR}/symlink-profile-dir.log"; then
  echo "installer leaked symlinked shell profile directory paths in profile-edit output" >&2
  cat "${WORK_DIR}/symlink-profile-dir.log" >&2
  exit 1
fi
if grep -Fq "# added by bucephalus installer" "${symlink_profile_dir_target}/bucephalus.fish" || grep -Fq "${symlink_profile_dir_install}" "${symlink_profile_dir_target}/bucephalus.fish"; then
  echo "installer followed and modified a symlinked fish profile directory target" >&2
  cat "${symlink_profile_dir_target}/bucephalus.fish" >&2
  exit 1
fi
if ! grep -Fq "Next: run '${symlink_profile_dir_install}/bucephalus' setup" "${WORK_DIR}/symlink-profile-dir.log"; then
  echo "installer did not provide absolute-path setup guidance after skipping a symlinked profile directory" >&2
  cat "${WORK_DIR}/symlink-profile-dir.log" >&2
  exit 1
fi

colon_home="${WORK_DIR}/colon-home"
colon_install_dir="${WORK_DIR}/colon:install/bin"
run_installer_with_profile_edit "${valid_dist}" "${colon_install_dir}" "${colon_home}" > "${WORK_DIR}/colon.log" 2>&1
test -x "${colon_install_dir}/bucephalus"
if [[ -e "${colon_home}/.bashrc" || -e "${colon_home}/.bash_profile" ]]; then
  echo "installer modified shell profiles for a colon-delimited install directory" >&2
  cat "${WORK_DIR}/colon.log" >&2
  exit 1
fi
if ! grep -Fq "Skipping shell profile edits because '${colon_install_dir}' cannot be represented as a single PATH entry." "${WORK_DIR}/colon.log"; then
  echo "installer did not explain why colon-delimited install directories cannot be added to PATH" >&2
  cat "${WORK_DIR}/colon.log" >&2
  exit 1
fi
if ! grep -Fq "Next: run '${colon_install_dir}/bucephalus' setup" "${WORK_DIR}/colon.log"; then
  echo "installer did not give absolute-path setup guidance for a colon-delimited install directory" >&2
  cat "${WORK_DIR}/colon.log" >&2
  exit 1
fi

control_home="${WORK_DIR}/control-home"
control_install_dir="${WORK_DIR}/control install/"$'line\nbreak'"/bin"
run_installer_with_profile_edit "${valid_dist}" "${control_install_dir}" "${control_home}" > "${WORK_DIR}/control.log" 2>&1
test -x "${control_install_dir}/bucephalus"
if [[ -e "${control_home}/.bashrc" || -e "${control_home}/.bash_profile" ]]; then
  echo "installer modified shell profiles for a control-character install directory" >&2
  cat "${WORK_DIR}/control.log" >&2
  exit 1
fi
if ! grep -Fq "control install/line?break/bin" "${WORK_DIR}/control.log"; then
  echo "installer did not render control characters in install paths safely" >&2
  cat "${WORK_DIR}/control.log" >&2
  exit 1
fi
if grep -Eq '^break/bin' "${WORK_DIR}/control.log"; then
  echo "installer leaked a raw newline from the install directory into guidance" >&2
  cat "${WORK_DIR}/control.log" >&2
  exit 1
fi
if ! grep -Fq "Next: run the installed bucephalus binary with the setup argument" "${WORK_DIR}/control.log"; then
  echo "installer printed unsafe copy/paste setup guidance for a control-character install directory" >&2
  cat "${WORK_DIR}/control.log" >&2
  exit 1
fi

unexpected_dist="${WORK_DIR}/unexpected"
make_unexpected_member_dist "${unexpected_dist}" "${ASSET}"
expect_installer_failure "unexpected" "${unexpected_dist}" "member_ref: archive-member://redacted"
if grep -Eq "private/customer-a|prod-openai-secrets|OPENAI_API_KEY|raw-secret" "${WORK_DIR}/unexpected.log"; then
  echo "installer unexpected-member failure leaked secret-like archive member details" >&2
  cat "${WORK_DIR}/unexpected.log" >&2
  exit 1
fi

duplicate_dist="${WORK_DIR}/duplicate"
make_duplicate_member_dist "${duplicate_dist}" "${ASSET}"
expect_installer_failure "duplicate" "${duplicate_dist}" "archive contains duplicate member: bucephalus"

unsafe_dist="${WORK_DIR}/unsafe"
make_unsafe_member_dist "${unsafe_dist}" "${ASSET}"
expect_installer_failure "unsafe" "${unsafe_dist}" "member_ref: archive-member://redacted"
if grep -Eq "private\\\\token|token.env" "${WORK_DIR}/unsafe.log"; then
  echo "installer unsafe-member failure leaked raw archive member details" >&2
  cat "${WORK_DIR}/unsafe.log" >&2
  exit 1
fi

symlink_dist="${WORK_DIR}/symlink"
make_symlink_member_dist "${symlink_dist}" "${ASSET}"
expect_installer_failure "symlink" "${symlink_dist}" "archive members must be regular files only"

malformed_checksum_dist="${WORK_DIR}/malformed-checksum"
make_malformed_checksum_dist "${malformed_checksum_dist}" "${ASSET}"
expect_installer_failure "malformed-checksum" "${malformed_checksum_dist}" "malformed checksum digest in file://[REDACTED:local-path]"

setup_failing_dist="${WORK_DIR}/setup-failing"
setup_failing_install="${WORK_DIR}/setup-failing-install"
make_setup_failing_dist "${setup_failing_dist}" "${ASSET}"
if run_installer_with_setup "${setup_failing_dist}" "${setup_failing_install}" > "${WORK_DIR}/setup-failing.log" 2>&1; then
  echo "installer unexpectedly succeeded when post-install setup failed" >&2
  cat "${WORK_DIR}/setup-failing.log" >&2
  exit 1
fi
test -x "${setup_failing_install}/bucephalus"
test -x "${setup_failing_install}/bucephalus-cloud"
test -x "${setup_failing_install}/bucephalus-modal-launcher"
test -f "${setup_failing_install}/bucephalus-install.sh"
if compgen -G "${setup_failing_install}/.bucephalus-install-staging.*" >/dev/null; then
  echo "installer left staging directories behind after post-install setup failed" >&2
  find "${setup_failing_install}" -maxdepth 1 -name '.bucephalus-install-staging.*' >&2
  exit 1
fi
if ! grep -Fq "Bucephalus was installed, but post-install setup did not complete." "${WORK_DIR}/setup-failing.log"; then
  echo "installer did not separate successful install from setup failure" >&2
  cat "${WORK_DIR}/setup-failing.log" >&2
  exit 1
fi
if ! grep -Fq "binary_ref: install-dir://bucephalus" "${WORK_DIR}/setup-failing.log"; then
  echo "installer setup failure did not include a public binary ref" >&2
  cat "${WORK_DIR}/setup-failing.log" >&2
  exit 1
fi
if ! grep -Fq "Next: run '${setup_failing_install}/bucephalus' setup after fixing the setup error above." "${WORK_DIR}/setup-failing.log"; then
  echo "installer setup failure did not include actionable retry guidance" >&2
  cat "${WORK_DIR}/setup-failing.log" >&2
  exit 1
fi
if grep -Fq "file://${setup_failing_dist}" "${WORK_DIR}/setup-failing.log"; then
  echo "installer setup failure leaked a local mirror path" >&2
  cat "${WORK_DIR}/setup-failing.log" >&2
  exit 1
fi

echo "installer archive boundary checks passed"
