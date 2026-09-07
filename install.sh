#!/bin/sh
# Installs guvnor from the GitHub releases (macOS + Linux; matches the build
# matrix in .github/workflows/release.yml — no Windows binary is published).
#
#   curl -fsSL https://raw.githubusercontent.com/Cmdv/guvnor/main/install.sh | sh
#
# Env vars (optional):
#   GUVNOR_VERSION      tag to install, e.g. v0.1.0 (default: latest)
#   GUVNOR_INSTALL_DIR  where the binary goes (default: /usr/local/bin, with
#                       sudo if that needs root; falls back to ~/.local/bin)
#
# ponytail: no checksums.txt ships with the release yet, so integrity rests on
# GitHub's TLS + the workflow that built the asset. Add a `sha256sum -c` step
# here if the release job ever starts publishing one.
set -eu

repo="Cmdv/guvnor"
version="${GUVNOR_VERSION:-latest}"

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Darwin) platform="apple-darwin" ;;
  Linux) platform="unknown-linux-musl" ;;
  *) echo "guvnor: unsupported OS: $os (macOS and Linux only)" >&2; exit 1 ;;
esac

case "$arch" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) echo "guvnor: unsupported architecture: $arch" >&2; exit 1 ;;
esac

asset="guvnor-${arch}-${platform}.tar.gz"
if [ "$version" = "latest" ]; then
  url="https://github.com/$repo/releases/latest/download/$asset"
else
  url="https://github.com/$repo/releases/download/$version/$asset"
fi

command -v curl >/dev/null 2>&1 || { echo "guvnor: curl is required" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "guvnor: downloading $asset ($version)" >&2
curl -fsSL "$url" -o "$tmp/guvnor.tar.gz" || {
  echo "guvnor: download failed: $url" >&2
  exit 1
}
tar -xzf "$tmp/guvnor.tar.gz" -C "$tmp"
chmod +x "$tmp/guvnor"

install_dir="${GUVNOR_INSTALL_DIR:-/usr/local/bin}"
mkdir -p "$install_dir" 2>/dev/null || true

if [ -w "$install_dir" ]; then
  mv "$tmp/guvnor" "$install_dir/guvnor"
elif command -v sudo >/dev/null 2>&1; then
  echo "guvnor: sudo needed to write to $install_dir" >&2
  sudo mv "$tmp/guvnor" "$install_dir/guvnor"
else
  install_dir="$HOME/.local/bin"
  mkdir -p "$install_dir"
  mv "$tmp/guvnor" "$install_dir/guvnor"
fi

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) echo "guvnor: $install_dir is not on your PATH — add it to your shell profile" >&2 ;;
esac

echo "guvnor: installed $("$install_dir/guvnor" --version) to $install_dir" >&2
