#!/bin/sh
set -eu

repo="${BUCEPHALUS_REPO:-nishiokj/Bucephalus}"
version="${BUCEPHALUS_VERSION:-latest}"
install_dir="${BUCEPHALUS_INSTALL_DIR:-$HOME/.local/bin}"
base_url="${BUCEPHALUS_BASE_URL:-}"

usage() {
  printf '%s\n' "Usage: curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | sh"
  printf '%s\n' ""
  printf '%s\n' "Environment:"
  printf '%s\n' "  BUCEPHALUS_VERSION       Release version or tag. Defaults to latest."
  printf '%s\n' "  BUCEPHALUS_INSTALL_DIR   Install directory. Defaults to \$HOME/.local/bin."
  printf '%s\n' "  BUCEPHALUS_REPO          GitHub owner/repo. Defaults to nishiokj/Bucephalus."
  printf '%s\n' "  BUCEPHALUS_SETUP         Set to 1 to run 'bucephalus setup' after install."
  printf '%s\n' "  BUCEPHALUS_NO_MODIFY_PATH Set to 1 to skip editing shell profiles for PATH."
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  "")
    ;;
  *)
    printf '%s\n' "unknown installer argument" >&2
    usage >&2
    exit 2
    ;;
esac

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf '%s\n' "required command not found: $1" >&2
    exit 2
  fi
}

need curl
need grep
need sed
need tar
need install
need tr

quote_posix() {
  printf "'"
  printf '%s' "$1" | sed "s/'/'\\\\''/g"
  printf "'"
}

quote_fish() {
  printf "'"
  printf '%s' "$1" | sed "s/['\\\\]/\\\\&/g"
  printf "'"
}

quote_display_path() {
  printable="$(printf '%s' "$1" | LC_ALL=C tr '[:cntrl:]' '?')"
  quote_posix "$printable"
}

profile_ref() {
  profile_name="${1##*/}"
  case "$profile_name" in
    .zshrc) printf '%s' "shell-profile://zshrc" ;;
    .zprofile) printf '%s' "shell-profile://zprofile" ;;
    .bashrc) printf '%s' "shell-profile://bashrc" ;;
    .bash_profile) printf '%s' "shell-profile://bash-profile" ;;
    .profile) printf '%s' "shell-profile://profile" ;;
    bucephalus.fish) printf '%s' "shell-profile://fish/conf.d/bucephalus.fish" ;;
    *) printf '%s' "shell-profile://custom" ;;
  esac
}

install_target_ref() {
  case "$1" in
    bucephalus) printf '%s' "install-dir://bucephalus" ;;
    bucephalus-cloud) printf '%s' "install-dir://bucephalus-cloud" ;;
    bucephalus-modal-launcher) printf '%s' "install-dir://bucephalus-modal-launcher" ;;
    bucephalus-install.sh) printf '%s' "install-dir://bucephalus-install.sh" ;;
    *) printf '%s' "install-dir://unknown" ;;
  esac
}

archive_member_ref() {
  raw="$1"
  lower="$(printf '%s' "$raw" | LC_ALL=C tr '[:upper:]' '[:lower:]')"
  case "$raw" in
    ""|/*|*"/../"*|../*|*"/.."|*"\\"*)
      printf '%s' "archive-member://redacted"
      return 0
      ;;
  esac
  case "$lower" in
    *secret*|*token*|*password*|*credential*|*api_key*|*private*|.env|*.env|*/.env|*/.env/*)
      printf '%s' "archive-member://redacted"
      return 0
      ;;
  esac
  public="$(printf '%s' "$raw" | LC_ALL=C sed -e 's#[^A-Za-z0-9._/-]#_#g' -e 's#//*#/#g' -e 's#^/*##' -e 's#/*$##')"
  if [ -z "$public" ]; then
    public="member"
  fi
  printf '%s' "archive-member://${public}"
}

has_control_chars() {
  printable="$(printf '%s' "$1" | LC_ALL=C tr '[:cntrl:]' '?')"
  [ "$printable" != "$1" ]
}

display_url() {
  case "$1" in
    file://*)
      printf '%s' "file://[REDACTED:local-path]"
      return 0
      ;;
  esac

  public="$(printf '%s' "$1" | sed -e 's/[?#].*$//' -e 's#^\(https\{0,1\}://\)[^/]*@#\1#')"
  if [ "$public" != "$1" ]; then
    printf '%s' "${public} [redacted URL credentials/query]"
  else
    printf '%s' "$public"
  fi
}

validate_repo() {
  if has_control_chars "$repo"; then
    printf '%s\n' "invalid BUCEPHALUS_REPO: expected owner/repo using letters, numbers, dots, underscores, or hyphens" >&2
    exit 2
  fi
  case "$repo" in
    ""|/*|*/|*/*/*|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_./-]*)
      printf '%s\n' "invalid BUCEPHALUS_REPO: expected owner/repo using letters, numbers, dots, underscores, or hyphens" >&2
      exit 2
      ;;
    */*) ;;
    *)
      printf '%s\n' "invalid BUCEPHALUS_REPO: expected owner/repo using letters, numbers, dots, underscores, or hyphens" >&2
      exit 2
      ;;
  esac
}

validate_version() {
  if [ "$version" = "latest" ]; then
    return 0
  fi
  if has_control_chars "$version"; then
    printf '%s\n' "invalid BUCEPHALUS_VERSION: expected latest or a release tag using letters, numbers, dots, underscores, or hyphens" >&2
    exit 2
  fi
  case "$version" in
    ""|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-]*)
      printf '%s\n' "invalid BUCEPHALUS_VERSION: expected latest or a release tag using letters, numbers, dots, underscores, or hyphens" >&2
      exit 2
      ;;
  esac
}

validate_base_url() {
  if [ -z "$base_url" ]; then
    return 0
  fi
  if has_control_chars "$base_url"; then
    printf '%s\n' "invalid BUCEPHALUS_BASE_URL: expected an https:// or file:// base URL without credentials, query, or fragment" >&2
    exit 2
  fi
  case "$base_url" in
    *\?*|*#*)
      printf '%s\n' "invalid BUCEPHALUS_BASE_URL: expected an https:// or file:// base URL without credentials, query, or fragment" >&2
      exit 2
      ;;
    https://*)
      authority="${base_url#https://}"
      authority="${authority%%/*}"
      case "$authority" in
        *@*)
          printf '%s\n' "invalid BUCEPHALUS_BASE_URL: expected an https:// or file:// base URL without credentials, query, or fragment" >&2
          exit 2
          ;;
      esac
      ;;
    file://*) ;;
    *)
      printf '%s\n' "invalid BUCEPHALUS_BASE_URL: expected an https:// or file:// base URL without credentials, query, or fragment" >&2
      exit 2
      ;;
  esac
}

validate_repo
validate_version
validate_base_url

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) os_part="apple-darwin" ;;
  Linux) os_part="unknown-linux-gnu" ;;
  *)
    printf '%s\n' "unsupported OS: $os" >&2
    exit 2
    ;;
esac

case "$arch" in
  arm64|aarch64) arch_part="aarch64" ;;
  x86_64|amd64) arch_part="x86_64" ;;
  *)
    printf '%s\n' "unsupported architecture: $arch" >&2
    exit 2
    ;;
esac

target="${arch_part}-${os_part}"
asset="bucephalus-${target}.tar.gz"

if [ -n "$base_url" ]; then
  archive_url="${base_url%/}/${asset}"
elif [ "$version" = "latest" ]; then
  archive_url="https://github.com/${repo}/releases/latest/download/${asset}"
else
  tag="$version"
  case "$tag" in
    v*) ;;
    *) tag="v${tag}" ;;
  esac
  archive_url="https://github.com/${repo}/releases/download/${tag}/${asset}"
fi
checksum_url="${archive_url}.sha256"

tmp_dir="$(mktemp -d)"
staging_dir=""
cleanup() {
  rm -rf "$tmp_dir"
  if [ -n "$staging_dir" ]; then
    rm -rf "$staging_dir"
  fi
}
trap cleanup EXIT HUP INT TERM

archive_path="${tmp_dir}/${asset}"
checksum_path="${archive_path}.sha256"
curl_error_path="${tmp_dir}/curl-error.txt"

download_file() {
  url="$1"
  out="$2"
  label="$3"
  if curl $curl_args -o "$out" "$url" 2>"$curl_error_path"; then
    return 0
  else
    status=$?
  fi
  printf '%s\n' "failed to download ${label}: $(display_url "$url")" >&2
  printf '%s\n' "curl_exit_status: ${status}" >&2
  exit "$status"
}

printf '%s\n' "Downloading $(display_url "$archive_url")"
case "$archive_url" in
  file://*) curl_args="-fsSL" ;;
  *) curl_args="-fsSL --proto =https --tlsv1.2" ;;
esac
download_file "$archive_url" "$archive_path" "release archive"
download_file "$checksum_url" "$checksum_path" "checksum file"

checksum_line_count="$(sed -n '$=' "$checksum_path")"
checksum_line="$(sed -n '1p' "$checksum_path")"
expected="${checksum_line%%  *}"
checksum_name="${checksum_line#*  }"

case "$expected" in
  ""|*[!0123456789abcdef]*)
    printf '%s\n' "malformed checksum digest in $(display_url "$checksum_url")" >&2
    exit 2
    ;;
esac

if [ "${checksum_line_count:-0}" != "1" ] || [ "${#expected}" -ne 64 ] || [ "$checksum_name" != "$asset" ] || [ "$checksum_line" != "$expected  $asset" ]; then
  printf '%s\n' "malformed checksum file: $(display_url "$checksum_url")" >&2
  exit 2
fi

if command -v sha256sum >/dev/null 2>&1; then
  read -r actual _ <<EOF
$(sha256sum "$archive_path")
EOF
elif command -v shasum >/dev/null 2>&1; then
  read -r actual _ <<EOF
$(shasum -a 256 "$archive_path")
EOF
else
  printf '%s\n' "sha256sum or shasum is required" >&2
  exit 2
fi

if [ "$actual" != "$expected" ]; then
  printf '%s\n' "checksum mismatch for ${asset}" >&2
  printf '%s\n' "expected: $expected" >&2
  printf '%s\n' "actual:   $actual" >&2
  exit 1
fi

archive_members="${tmp_dir}/archive-members.txt"
archive_listing="${tmp_dir}/archive-listing.txt"
tar -tzf "$archive_path" > "$archive_members"
tar -tvzf "$archive_path" > "$archive_listing"

expected_member_count=8
actual_member_count="$(sed -n '$=' "$archive_members")"
if [ "${actual_member_count:-0}" != "$expected_member_count" ]; then
  printf '%s\n' "archive must contain exactly ${expected_member_count} files; found ${actual_member_count:-0}" >&2
  exit 1
fi

while IFS= read -r member || [ -n "$member" ]; do
  case "$member" in
    ""|/*|*"/../"*|../*|*"/.."|*"\\"*)
      printf '%s\n' "unsafe archive member path" >&2
      printf '%s\n' "member_ref: $(archive_member_ref "$member")" >&2
      exit 1
      ;;
    bucephalus|bucephalus-cloud|bucephalus-modal-launcher|install.sh|README.md|LICENSE|release-manifest.json|SHA256SUMS)
      ;;
    *)
      printf '%s\n' "unexpected archive member" >&2
      printf '%s\n' "member_ref: $(archive_member_ref "$member")" >&2
      exit 1
      ;;
  esac
done < "$archive_members"

while IFS= read -r listing || [ -n "$listing" ]; do
  case "$listing" in
    -*) ;;
    *)
      printf '%s\n' "archive members must be regular files only" >&2
      exit 1
      ;;
  esac
done < "$archive_listing"

for expected_member in bucephalus bucephalus-cloud bucephalus-modal-launcher install.sh README.md LICENSE release-manifest.json SHA256SUMS; do
  member_count=0
  while IFS= read -r member || [ -n "$member" ]; do
    if [ "$member" = "$expected_member" ]; then
      member_count=$((member_count + 1))
    fi
  done < "$archive_members"
  if [ "$member_count" -eq 0 ]; then
    printf '%s\n' "archive is missing expected member: $expected_member" >&2
    exit 1
  fi
  if [ "$member_count" -gt 1 ]; then
    printf '%s\n' "archive contains duplicate member: $expected_member" >&2
    exit 1
  fi
done

tar -xzf "$archive_path" -C "$tmp_dir"
if [ ! -x "${tmp_dir}/bucephalus" ]; then
  printf '%s\n' "archive did not contain executable bucephalus" >&2
  exit 1
fi
if [ ! -x "${tmp_dir}/bucephalus-modal-launcher" ]; then
  printf '%s\n' "archive did not contain executable bucephalus-modal-launcher" >&2
  exit 1
fi
if [ ! -x "${tmp_dir}/bucephalus-cloud" ]; then
  printf '%s\n' "archive did not contain executable bucephalus-cloud" >&2
  exit 1
fi

mkdir -p "$install_dir"
for install_name in bucephalus bucephalus-cloud bucephalus-modal-launcher bucephalus-install.sh; do
  install_path="${install_dir}/${install_name}"
  if [ -d "$install_path" ]; then
    printf '%s\n' "install target exists but is a directory" >&2
    printf '%s\n' "target_ref: $(install_target_ref "$install_name")" >&2
    printf '%s\n' "Remove that directory or choose a different BUCEPHALUS_INSTALL_DIR, then rerun the installer." >&2
    exit 1
  fi
  if [ -e "$install_path" ] && [ ! -f "$install_path" ]; then
    printf '%s\n' "install target exists but is not a regular file" >&2
    printf '%s\n' "target_ref: $(install_target_ref "$install_name")" >&2
    printf '%s\n' "Move that path aside or choose a different BUCEPHALUS_INSTALL_DIR, then rerun the installer." >&2
    exit 1
  fi
  if [ -L "$install_path" ] && [ ! -e "$install_path" ]; then
    printf '%s\n' "install target is a broken symlink" >&2
    printf '%s\n' "target_ref: $(install_target_ref "$install_name")" >&2
    printf '%s\n' "Repair or remove that symlink, then rerun the installer." >&2
    exit 1
  fi
done

staging_dir="$(mktemp -d "${install_dir}/.bucephalus-install-staging.XXXXXX")"
chmod 700 "$staging_dir"
install -m 0755 "${tmp_dir}/bucephalus" "${staging_dir}/bucephalus"
install -m 0755 "${tmp_dir}/bucephalus-cloud" "${staging_dir}/bucephalus-cloud"
install -m 0755 "${tmp_dir}/bucephalus-modal-launcher" "${staging_dir}/bucephalus-modal-launcher"
install -m 0644 "${tmp_dir}/install.sh" "${staging_dir}/bucephalus-install.sh"

mv -f "${staging_dir}/bucephalus" "${install_dir}/bucephalus"
mv -f "${staging_dir}/bucephalus-cloud" "${install_dir}/bucephalus-cloud"
mv -f "${staging_dir}/bucephalus-modal-launcher" "${install_dir}/bucephalus-modal-launcher"
mv -f "${staging_dir}/bucephalus-install.sh" "${install_dir}/bucephalus-install.sh"
rm -rf "$staging_dir"
staging_dir=""

printf '%s\n' "Installed bucephalus, bucephalus-cloud, bucephalus-modal-launcher, and bucephalus-install.sh to $(quote_display_path "$install_dir")"
"${install_dir}/bucephalus" --version

# Idempotently append an export line to a POSIX shell profile. Re-runs skip the
# exact install directory, but a changed BUCEPHALUS_INSTALL_DIR gets its own
# PATH entry instead of silently leaving the old install location active.
path_marker="# added by bucephalus installer"
append_posix() {
  rc="$1"
  rc_dir="${rc%/*}"
  if [ "$rc_dir" != "$rc" ]; then
    mkdir -p "$rc_dir"
    if [ -L "$rc_dir" ]; then
      printf '%s\n' "Skipping shell profile $(profile_ref "$rc") because its directory is a symlink."
      return 0
    fi
  fi
  if [ -L "$rc" ]; then
    printf '%s\n' "Skipping shell profile $(profile_ref "$rc") because it is a symlink."
    return 0
  fi
  [ -f "$rc" ] || : >"$rc"
  quoted_install_dir="$(quote_posix "$install_dir")"
  path_line="export PATH=${quoted_install_dir}:\$PATH"
  if grep -qF "$path_line" "$rc" 2>/dev/null; then
    modified_path=1
    return 0
  fi
  {
    printf '\n%s\n' "$path_marker"
    printf '%s\n' "$path_line"
  } >>"$rc"
  printf '%s\n' "Updated shell profile $(profile_ref "$rc")"
  modified_path=1
}

# Whether the bare command already resolves on PATH this session.
on_path=0
path_entry_supported=1
path_has_control_chars=0
if has_control_chars "$install_dir"; then
  path_has_control_chars=1
fi
case "$install_dir" in
  *:*) path_entry_supported=0 ;;
esac
if [ "$path_has_control_chars" -eq 1 ]; then
  path_entry_supported=0
fi
if [ "$path_entry_supported" -eq 1 ]; then
  old_ifs="$IFS"
  IFS=:
  for path_entry in $PATH; do
    if [ "$path_entry" = "$install_dir" ]; then
      on_path=1
      break
    fi
  done
  IFS="$old_ifs"
fi

modified_path=0
case "${BUCEPHALUS_NO_MODIFY_PATH:-0}" in
  1|true|TRUE|yes|YES) ;;
  *)
    if [ "$path_entry_supported" -eq 0 ]; then
      printf '\n%s\n' "Skipping shell profile edits because $(quote_display_path "$install_dir") cannot be represented as a single PATH entry."
    elif [ "$on_path" -eq 0 ]; then
      shell_name="$(basename "${SHELL:-sh}")"
      case "$shell_name" in
        zsh)
          append_posix "${ZDOTDIR:-$HOME}/.zshrc"
          append_posix "${ZDOTDIR:-$HOME}/.zprofile"
          ;;
        bash)
          append_posix "${HOME}/.bashrc"
          append_posix "${HOME}/.bash_profile"
          ;;
        fish)
          fish_conf="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d"
          mkdir -p "$fish_conf"
          fish_file="${fish_conf}/bucephalus.fish"
          if [ -L "$fish_conf" ]; then
            printf '%s\n' "Skipping shell profile $(profile_ref "$fish_file") because its directory is a symlink."
          elif [ -L "$fish_file" ]; then
            printf '%s\n' "Skipping shell profile $(profile_ref "$fish_file") because it is a symlink."
          else
            quoted_install_dir="$(quote_fish "$install_dir")"
            fish_path_line="fish_add_path -- ${quoted_install_dir}"
            if grep -qF "$fish_path_line" "$fish_file" 2>/dev/null; then
              modified_path=1
            else
              {
                printf '%s\n' "$path_marker"
                printf '%s\n' "$fish_path_line"
              } >>"$fish_file"
              printf '%s\n' "Updated shell profile $(profile_ref "$fish_file")"
              modified_path=1
            fi
          fi
          ;;
        *)
          append_posix "${HOME}/.profile"
          ;;
      esac
    fi
    ;;
esac

# Bare command in guidance only when it will actually resolve in a fresh shell.
if [ "$on_path" -eq 1 ] || [ "$modified_path" -eq 1 ]; then
  setup_cmd="bucephalus setup"
else
  if [ "$path_has_control_chars" -eq 1 ]; then
    setup_cmd="the installed bucephalus binary with the setup argument"
  else
    setup_cmd="$(quote_posix "${install_dir}/bucephalus") setup"
  fi
  if [ "$path_entry_supported" -eq 1 ]; then
    printf '\n%s\n' "$(quote_display_path "$install_dir") is not on your PATH. Add it manually:"
    printf '  export PATH=%s:$PATH\n' "$(quote_posix "$install_dir")"
  else
    printf '\n%s\n' "Use the installed binary by absolute path because the install directory cannot be added to PATH safely."
  fi
fi

if [ "$modified_path" -eq 1 ]; then
  printf '\n%s\n' "Restart your shell or 'source' your profile to start using bucephalus."
fi

case "${BUCEPHALUS_SETUP:-0}" in
  1|true|TRUE|yes|YES)
    if "${install_dir}/bucephalus" setup; then
      :
    else
      setup_status=$?
      printf '\n%s\n' "Bucephalus was installed, but post-install setup did not complete." >&2
      printf '%s\n' "binary_ref: $(install_target_ref bucephalus)" >&2
      printf '%s\n' "Next: run ${setup_cmd} after fixing the setup error above." >&2
      exit "$setup_status"
    fi
    ;;
  *)
    printf '\n%s\n' "Next: run ${setup_cmd} to install the Tier-1 daemon and MCP registration."
    ;;
esac
