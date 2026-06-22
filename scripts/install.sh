#!/bin/sh
set -e

# devenv installer — the single source of truth.
#
# Usage: curl -fsSL https://devenv.tools/install.sh | sh
#   (devenv.tools/install.sh is a redirect to this file, published as a GitHub
#    Release asset at:
#      https://github.com/LoumTechnologies/devenv-tunnel/releases/latest/download/install.sh)
#
# Environment variables:
#   DEVENV_TOOLS_INSTALL_DIR  - Override install directory
#   DEVENV_TOOLS_VERSION      - Install a specific version (a git tag, e.g. "v0.0.22")
#   DEVENV_TOOLS_CHANNEL      - Accepted for compatibility; only "latest" exists
#                               on GitHub Releases ("staging" is treated as latest).
#
# Downloads the `devenv` + `devenv-tunnel` binaries from this repo's GitHub
# Releases. POSIX sh on purpose (it runs under whatever `sh` the curl pipe uses).

REPO="LoumTechnologies/devenv-tunnel"
RELEASES_URL="https://github.com/${REPO}/releases"

# --- Colors (only when connected to a terminal) ---

setup_colors() {
    if [ -t 1 ] && [ -t 2 ]; then
        RED='\033[0;31m'
        GREEN='\033[0;32m'
        YELLOW='\033[0;33m'
        BLUE='\033[0;34m'
        BOLD='\033[1m'
        RESET='\033[0m'
    else
        RED=''; GREEN=''; YELLOW=''; BLUE=''; BOLD=''; RESET=''
    fi
}

info()    { printf "${BLUE}info:${RESET} %s\n" "$1"; }
warn()    { printf "${YELLOW}warn:${RESET} %s\n" "$1" >&2; }
error()   { printf "${RED}error:${RESET} %s\n" "$1" >&2; }
success() { printf "${GREEN}${BOLD}%s${RESET}\n" "$1"; }

# --- Platform detection ---

detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "macos" ;;
        MINGW*|MSYS*|CYGWIN*)
            error "Windows is not supported by this installer."
            echo "Download a binary manually from: ${RELEASES_URL}" >&2
            exit 1
            ;;
        *)
            error "Unsupported operating system: $(uname -s)"
            echo "Download a binary manually from: ${RELEASES_URL}" >&2
            exit 1
            ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *)
            error "Unsupported architecture: $(uname -m)"
            echo "devenv supports x86_64 and aarch64/arm64. See ${RELEASES_URL}" >&2
            exit 1
            ;;
    esac
}

# Map OS + arch to the Rust target triple used in release artifacts.
rust_target() {
    case "$1" in
        linux) echo "$2-unknown-linux-gnu" ;;
        macos) echo "$2-apple-darwin" ;;
    esac
}

# --- Download helpers ---

has_cmd() { command -v "$1" >/dev/null 2>&1; }

download() {
    if has_cmd curl; then
        curl -fsSL --retry 3 --retry-delay 2 -o "$2" "$1"
    elif has_cmd wget; then
        wget -q --tries=3 -O "$2" "$1"
    else
        error "Neither curl nor wget found. Install one and try again."
        exit 1
    fi
}

# --- Install directory ---

determine_install_dir() {
    if [ -n "${DEVENV_TOOLS_INSTALL_DIR:-}" ]; then
        echo "$DEVENV_TOOLS_INSTALL_DIR"
    elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        echo "/usr/local/bin"
    else
        echo "${HOME}/.local/bin"
    fi
}

# --- Install a single binary ---

install_binary() {
    _src="$1"; _name="$2"; _install_dir="$3"
    if [ ! -f "$_src" ]; then
        error "Expected binary not found in archive: ${_name}"
        echo "Please report this at: ${RELEASES_URL%/releases}/issues" >&2
        exit 1
    fi
    rm -f "${_install_dir}/${_name}" 2>/dev/null || true
    if ! cp "$_src" "${_install_dir}/${_name}" 2>/dev/null; then
        error "Permission denied: cannot write to ${_install_dir}"
        echo "  1. Run with sudo:  curl -fsSL https://devenv.tools/install.sh | sudo sh" >&2
        echo "  2. Custom dir:     DEVENV_TOOLS_INSTALL_DIR=~/.local/bin curl -fsSL https://devenv.tools/install.sh | sh" >&2
        exit 1
    fi
    chmod +x "${_install_dir}/${_name}"
    info "Installed ${_name} to ${_install_dir}/${_name}"
}

# --- Main ---

main() {
    setup_colors
    printf "${BOLD}devenv installer${RESET}\n\n"

    os=$(detect_os)
    arch=$(detect_arch)
    target=$(rust_target "$os" "$arch")
    info "Detected platform: ${os}/${arch} (${target})"

    # Resolve the download base. GitHub Releases layout:
    #   latest:  /releases/latest/download/<asset>
    #   pinned:  /releases/download/<tag>/<asset>
    if [ -n "${DEVENV_TOOLS_VERSION:-}" ]; then
        info "Installing requested version: ${DEVENV_TOOLS_VERSION}"
        download_base="${RELEASES_URL}/download/${DEVENV_TOOLS_VERSION}"
    else
        channel="${DEVENV_TOOLS_CHANNEL:-latest}"
        case "$channel" in
            latest) ;;
            staging) warn "staging channel is not published on GitHub Releases; installing latest stable." ;;
            *) error "Unsupported channel: ${channel} (only 'latest' is available)"; exit 1 ;;
        esac
        info "Installing latest version"
        download_base="${RELEASES_URL}/latest/download"
    fi

    archive="devenv-tunnel-${target}.tar.gz"
    url="${download_base}/${archive}"
    info "Downloading ${url}"

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    if ! download "$url" "${tmpdir}/${archive}"; then
        error "Download failed."
        echo "  - Check your connection, or that a release exists for ${target}." >&2
        echo "  - Releases: ${RELEASES_URL}" >&2
        exit 1
    fi

    info "Extracting archive"
    tar xzf "${tmpdir}/${archive}" -C "$tmpdir"

    install_dir=$(determine_install_dir)
    if [ ! -d "$install_dir" ]; then
        info "Creating ${install_dir}"
        mkdir -p "$install_dir"
    fi

    # The archive extracts to "devenv-tunnel-${target}/" with both binaries.
    archive_dir="${tmpdir}/devenv-tunnel-${target}"
    install_binary "${archive_dir}/devenv"        "devenv"        "$install_dir"
    install_binary "${archive_dir}/devenv-tunnel" "devenv-tunnel" "$install_dir"

    path_warning=""
    case ":${PATH}:" in
        *":${install_dir}:"*) ;;
        *)
            path_warning=1
            warn "${install_dir} is not in your PATH"
            echo "  Add to your shell profile:  export PATH=\"${install_dir}:\$PATH\"" >&2
            ;;
    esac

    echo ""
    if [ -z "$path_warning" ] && has_cmd devenv; then
        success "devenv installed successfully! ($(devenv --version 2>/dev/null || echo unknown))"
    else
        success "devenv installed successfully!"
    fi

    echo ""
    echo "Next steps:"
    echo "  ${BOLD}devenv tunnel login${RESET}    Log in to your account"
    echo "  ${BOLD}devenv tunnel start${RESET}    Start the tunnel daemon"
    echo "  ${BOLD}devenv --help${RESET}          Show available subcommands"
    echo ""
}

main "$@"
