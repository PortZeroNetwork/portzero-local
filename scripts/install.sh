#!/bin/sh
# devenv installer.
#
# Downloads the latest `devenv` + `devenv-tunnel` binaries from this repo's
# GitHub Releases and installs them. Intended to be run via:
#
#   curl -fsSL https://devenv.tools/install.sh | sh
#
# POSIX sh (not bash) on purpose — it must run under whatever `sh` the pipe
# above provides. Override the install dir with DEVENV_INSTALL_DIR.
#
# Release assets are produced by .github/workflows/release.yml as
# `devenv-tunnel-<target>.tar.gz`, which extracts to
# `devenv-tunnel-<target>/{devenv,devenv-tunnel}`.
set -eu

REPO="LoumTechnologies/devenv-tunnel"
BASE="https://github.com/${REPO}/releases/latest/download"
BIN_DIR="${DEVENV_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *)
    echo "error: unsupported OS '$os'." >&2
    echo "On Windows, download the .zip from https://github.com/${REPO}/releases/latest" >&2
    exit 1
    ;;
esac

case "$arch" in
  x86_64 | amd64)  arch_part="x86_64" ;;
  arm64 | aarch64) arch_part="aarch64" ;;
  *)
    echo "error: unsupported architecture '$arch'." >&2
    exit 1
    ;;
esac

target="${arch_part}-${os_part}"
asset="devenv-tunnel-${target}.tar.gz"
url="${BASE}/${asset}"

echo "Installing devenv (${target}) from GitHub Releases..."

tmp="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf \"$tmp\"" EXIT INT TERM

if ! curl -fsSL "$url" -o "$tmp/$asset"; then
  echo "error: failed to download $url" >&2
  echo "Check the latest release at https://github.com/${REPO}/releases/latest" >&2
  exit 1
fi

tar -xzf "$tmp/$asset" -C "$tmp"
src="$tmp/devenv-tunnel-${target}"

mkdir -p "$BIN_DIR"
install -m 0755 "$src/devenv" "$BIN_DIR/devenv"
install -m 0755 "$src/devenv-tunnel" "$BIN_DIR/devenv-tunnel"

echo "Installed devenv + devenv-tunnel to $BIN_DIR"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Note: $BIN_DIR is not on your PATH — add it to use 'devenv' directly." ;;
esac

"$BIN_DIR/devenv-tunnel" --version || true
