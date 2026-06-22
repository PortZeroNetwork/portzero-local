#!/bin/sh
set -e

# devenv installer — the single source of truth.
#
# Usage: curl -fsSL https://devenv.tools/install.sh | sh
#   (devenv.tools/install.sh redirects to this file, published as a GitHub
#    Release asset at:
#      https://github.com/LoumTechnologies/devenv-tunnel/releases/latest/download/install.sh)
#
# Downloads the latest `devenv` + `devenv-tunnel` binaries from this repo's
# GitHub Releases. POSIX sh (runs under whatever `sh` the curl pipe uses).
#
# Env: DEVENV_INSTALL_DIR overrides the install directory.

REPO="LoumTechnologies/devenv-tunnel"
RELEASES_URL="https://github.com/${REPO}/releases"

# --- Colors (only on a terminal) ---
if [ -t 1 ] && [ -t 2 ]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'; BLUE='\033[0;34m'; BOLD='\033[1m'; RESET='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; BLUE=''; BOLD=''; RESET=''
fi
info()    { printf "${BLUE}info:${RESET} %s\n" "$1"; }
warn()    { printf "${YELLOW}warn:${RESET} %s\n" "$1" >&2; }
error()   { printf "${RED}error:${RESET} %s\n" "$1" >&2; }
success() { printf "${GREEN}${BOLD}%s${RESET}\n" "$1"; }

has_cmd() { command -v "$1" >/dev/null 2>&1; }

# --- Detect target triple ---
case "$(uname -s)" in
    Linux*)  os="unknown-linux-gnu" ;;
    Darwin*) os="apple-darwin" ;;
    *)
        error "Unsupported OS: $(uname -s)"
        echo "Download a binary manually from ${RELEASES_URL}" >&2
        exit 1
        ;;
esac
case "$(uname -m)" in
    x86_64|amd64)  arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *)
        error "Unsupported architecture: $(uname -m)"
        echo "devenv supports x86_64 and aarch64/arm64. See ${RELEASES_URL}" >&2
        exit 1
        ;;
esac
target="${arch}-${os}"
archive="devenv-tunnel-${target}.tar.gz"

# --- Install directory ---
if [ -n "${DEVENV_INSTALL_DIR:-}" ]; then
    install_dir="$DEVENV_INSTALL_DIR"
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
    install_dir="/usr/local/bin"
else
    install_dir="${HOME}/.local/bin"
fi

printf "${BOLD}devenv installer${RESET}\n\n"
info "Platform: ${target}"

# --- Download ---
url="${RELEASES_URL}/latest/download/${archive}"
info "Downloading ${url}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if has_cmd curl; then
    curl -fsSL --retry 3 --retry-delay 2 -o "$tmp/$archive" "$url" || { error "Download failed (see ${RELEASES_URL})"; exit 1; }
elif has_cmd wget; then
    wget -q --tries=3 -O "$tmp/$archive" "$url" || { error "Download failed (see ${RELEASES_URL})"; exit 1; }
else
    error "Neither curl nor wget found. Install one and try again."
    exit 1
fi

info "Extracting"
tar xzf "$tmp/$archive" -C "$tmp"
# Archive extracts to "devenv-tunnel-${target}/" with both binaries.
src="$tmp/devenv-tunnel-${target}"

mkdir -p "$install_dir"
for bin in devenv devenv-tunnel; do
    if [ ! -f "$src/$bin" ]; then
        error "Binary '$bin' missing from archive; please report at ${RELEASES_URL%/releases}/issues"
        exit 1
    fi
    if ! install -m 0755 "$src/$bin" "$install_dir/$bin" 2>/dev/null; then
        error "Cannot write to ${install_dir}"
        echo "  Run with sudo, or set DEVENV_INSTALL_DIR to a writable directory." >&2
        exit 1
    fi
    info "Installed $bin to $install_dir/$bin"
done

echo ""
case ":${PATH}:" in
    *":${install_dir}:"*)
        success "devenv installed! ($("$install_dir/devenv" --version 2>/dev/null || echo ok))"
        ;;
    *)
        success "devenv installed to ${install_dir}!"
        warn "${install_dir} is not in your PATH — add: export PATH=\"${install_dir}:\$PATH\""
        ;;
esac

echo ""
echo "Next: ${BOLD}devenv tunnel login${RESET}  then  ${BOLD}devenv tunnel start${RESET}"
