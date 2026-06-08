#!/bin/sh
set -eu

repo="${BUCEPHALUS_REPO:-nishiokj/Bucephalus}"
version="${BUCEPHALUS_VERSION:-latest}"
install_dir="${BUCEPHALUS_INSTALL_DIR:-$HOME/.local/bin}"
base_url="${BUCEPHALUS_BASE_URL:-}"

usage() {
  printf '%s\n' "Usage: curl -fsSL https://raw.githubusercontent.com/${repo}/main/scripts/install.sh | sh"
  printf '%s\n' ""
  printf '%s\n' "Environment:"
  printf '%s\n' "  BUCEPHALUS_VERSION       Release version or tag. Defaults to latest."
  printf '%s\n' "  BUCEPHALUS_INSTALL_DIR   Install directory. Defaults to \$HOME/.local/bin."
  printf '%s\n' "  BUCEPHALUS_REPO          GitHub owner/repo. Defaults to ${repo}."
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
    printf '%s\n' "unknown argument: $1" >&2
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
need sed
need tar
need install

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
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

archive_path="${tmp_dir}/${asset}"
checksum_path="${archive_path}.sha256"

printf '%s\n' "Downloading ${archive_url}"
case "$archive_url" in
  file://*) curl_args="-fsSL" ;;
  *) curl_args="-fsSL --proto =https --tlsv1.2" ;;
esac
curl $curl_args -o "$archive_path" "$archive_url"
curl $curl_args -o "$checksum_path" "$checksum_url"

checksum_line_count="$(sed -n '$=' "$checksum_path")"
checksum_line="$(sed -n '1p' "$checksum_path")"
expected="${checksum_line%%  *}"
checksum_name="${checksum_line#*  }"

case "$expected" in
  ""|*[!0123456789abcdef]*)
    printf '%s\n' "malformed checksum digest in $checksum_url" >&2
    exit 2
    ;;
esac

if [ "${checksum_line_count:-0}" != "1" ] || [ "${#expected}" -ne 64 ] || [ "$checksum_name" != "$asset" ] || [ "$checksum_line" != "$expected  $asset" ]; then
  printf '%s\n' "malformed checksum file: $checksum_url" >&2
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
install -m 0755 "${tmp_dir}/bucephalus" "${install_dir}/bucephalus"
install -m 0755 "${tmp_dir}/bucephalus-cloud" "${install_dir}/bucephalus-cloud"
install -m 0755 "${tmp_dir}/bucephalus-modal-launcher" "${install_dir}/bucephalus-modal-launcher"

printf '%s\n' "Installed bucephalus, bucephalus-cloud, and bucephalus-modal-launcher to ${install_dir}"
"${install_dir}/bucephalus" --version

# Idempotently append an export line to a POSIX shell profile. The sentinel
# comment lets re-runs detect and skip an already-installed block.
path_marker="# added by bucephalus installer"
append_posix() {
  rc="$1"
  [ -f "$rc" ] || : >"$rc"
  if grep -qF "$path_marker" "$rc" 2>/dev/null; then
    return 0
  fi
  {
    printf '\n%s\n' "$path_marker"
    printf '%s\n' "export PATH=\"${install_dir}:\$PATH\""
  } >>"$rc"
  printf '%s\n' "Updated ${rc}"
}

# Whether the bare command already resolves on PATH this session.
on_path=0
case ":$PATH:" in
  *":${install_dir}:"*) on_path=1 ;;
esac

modified_path=0
case "${BUCEPHALUS_NO_MODIFY_PATH:-0}" in
  1|true|TRUE|yes|YES) ;;
  *)
    if [ "$on_path" -eq 0 ]; then
      shell_name="$(basename "${SHELL:-sh}")"
      case "$shell_name" in
        zsh)
          append_posix "${ZDOTDIR:-$HOME}/.zshrc"
          append_posix "${ZDOTDIR:-$HOME}/.zprofile"
          modified_path=1
          ;;
        bash)
          append_posix "${HOME}/.bashrc"
          append_posix "${HOME}/.bash_profile"
          modified_path=1
          ;;
        fish)
          fish_conf="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d"
          mkdir -p "$fish_conf"
          fish_file="${fish_conf}/bucephalus.fish"
          if ! grep -qF "$path_marker" "$fish_file" 2>/dev/null; then
            {
              printf '%s\n' "$path_marker"
              printf '%s\n' "fish_add_path ${install_dir}"
            } >>"$fish_file"
            printf '%s\n' "Updated ${fish_file}"
          fi
          modified_path=1
          ;;
        *)
          append_posix "${HOME}/.profile"
          modified_path=1
          ;;
      esac
    fi
    ;;
esac

# Bare command in guidance only when it will actually resolve in a fresh shell.
if [ "$on_path" -eq 1 ] || [ "$modified_path" -eq 1 ]; then
  bucephalus_cmd="bucephalus"
else
  bucephalus_cmd="${install_dir}/bucephalus"
  printf '\n%s\n' "${install_dir} is not on your PATH. Add it manually:"
  printf '  export PATH="%s:$PATH"\n' "${install_dir}"
fi

if [ "$modified_path" -eq 1 ]; then
  printf '\n%s\n' "Restart your shell or 'source' your profile to start using bucephalus."
fi

case "${BUCEPHALUS_SETUP:-0}" in
  1|true|TRUE|yes|YES)
    "${install_dir}/bucephalus" setup
    ;;
  *)
    printf '\n%s\n' "Next: run '${bucephalus_cmd} setup' to install the Tier-1 daemon and MCP registration."
    ;;
esac
