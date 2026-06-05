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

mkdir -p "$install_dir"
install -m 0755 "${tmp_dir}/bucephalus" "${install_dir}/bucephalus"
install -m 0755 "${tmp_dir}/bucephalus-modal-launcher" "${install_dir}/bucephalus-modal-launcher"

printf '%s\n' "Installed bucephalus to ${install_dir}/bucephalus"
printf '%s\n' "Installed Modal launcher to ${install_dir}/bucephalus-modal-launcher"
case ":$PATH:" in
  *":${install_dir}:"*) ;;
  *) printf '%s\n' "Add ${install_dir} to PATH if bucephalus is not found." ;;
esac
"${install_dir}/bucephalus" --version
case "${BUCEPHALUS_SETUP:-0}" in
  1|true|TRUE|yes|YES)
    "${install_dir}/bucephalus" setup
    ;;
  *)
    printf '%s\n' "Run '${install_dir}/bucephalus setup' to install the Tier-1 daemon service and MCP registration."
    ;;
esac
