#!/bin/sh
set -e

# portzero installer — the single source of truth.
#
# Usage: curl -fsSL https://portzero.cloud/install.sh | sh
#   (portzero.cloud/install.sh redirects to this file, published as a GitHub
#    Release asset at:
#      https://github.com/PortZeroNetwork/portzero-local/releases/latest/download/linux-install.sh)
#
# Downloads the latest portzero binary from GitHub Releases. POSIX sh.
#
# Env: PORTZERO_INSTALL_DIR overrides the install directory.

REPO="PortZeroNetwork/portzero-local"
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

# --- Detect platform ---
case "$(uname -s)" in
    Linux*)  os="linux" ;;
    Darwin*) os="darwin" ;;
    *)
        error "Unsupported OS: $(uname -s)"
        echo "Download a binary manually from ${RELEASES_URL}" >&2
        exit 1
        ;;
esac
case "$(uname -m)" in
    x86_64|amd64)  arch="amd64" ;;
    aarch64|arm64) arch="arm64" ;;
    i686|i386)     arch="x86" ;;
    *)
        error "Unsupported architecture: $(uname -m)"
        echo "portzero supports amd64, arm64, and x86. See ${RELEASES_URL}" >&2
        exit 1
        ;;
esac
target="${os}-${arch}"
archive="portzero-${target}.tar.gz"

# --- Install directory ---
if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
    install_dir="$PORTZERO_INSTALL_DIR"
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
    install_dir="/usr/local/bin"
else
    install_dir="${HOME}/.local/bin"
fi

printf "${BOLD}portzero installer${RESET}\n\n"
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
src="$tmp/portzero-${target}"

mkdir -p "$install_dir"
if ! install -m 0755 "$src/portzero" "$install_dir/portzero" 2>/dev/null; then
    error "Cannot write to ${install_dir}"
    echo "  Run with sudo, or set PORTZERO_INSTALL_DIR to a writable directory." >&2
    exit 1
fi
info "Installed portzero to $install_dir/portzero"

# --- Linux post-install: CAP_NET_ADMIN + systemd user unit ---
if [ "$(uname -s)" = "Linux" ]; then
    bin_path="$install_dir/portzero"

    echo ""
    info "Setting up Linux daemon capabilities..."

    # Grant CAP_NET_ADMIN (create TUN devices + modify routing tables) and
    # CAP_NET_BIND_SERVICE (let the embedded DNS server bind 10.254.0.1:53 so
    # *.portzero.local resolves) without running as root. Without the bind cap
    # the DNS server exits with "Permission denied" and no name resolves.
    if has_cmd setcap; then
        if sudo setcap 'cap_net_admin,cap_net_bind_service+eip' "$bin_path" 2>/dev/null; then
            info "CAP_NET_ADMIN + CAP_NET_BIND_SERVICE granted to $bin_path"
        else
            warn "Could not set capabilities (sudo failed or was denied)."
            warn "The local overlay (TUN + routing + DNS) requires root or:"
            warn "  sudo setcap 'cap_net_admin,cap_net_bind_service+eip' $bin_path"
        fi
    else
        warn "setcap not found — install libcap2-bin (Debian/Ubuntu) or libcap (Fedora/RHEL),"
        warn "then run: sudo setcap 'cap_net_admin,cap_net_bind_service+eip' $bin_path"
    fi

    # Install a systemd user service unit so 'portzero autostart enable' works
    # and the service can be managed with 'systemctl --user'.
    if has_cmd systemctl; then
        unit_dir="${HOME}/.config/systemd/user"
        unit_path="${unit_dir}/portzero-daemon.service"
        mkdir -p "$unit_dir"
        cat > "$unit_path" << UNIT
[Unit]
Description=Port Zero discovery daemon
Documentation=https://portzero.cloud/docs/daemon

[Service]
Type=simple
ExecStart=${bin_path} start --foreground
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
UNIT
        systemctl --user daemon-reload 2>/dev/null || true
        info "Systemd user unit installed: $unit_path"
        info "Enable at login: systemctl --user enable --now portzero-daemon"
    fi

    # Generate a local CA and install it into the system trust store so browsers
    # accept *.portzero.local over HTTPS without certificate warnings.
    info "Setting up local CA certificate..."
    "$bin_path" trust generate >/dev/null 2>&1 || true
    if sudo HOME="$HOME" "$bin_path" trust install 2>/dev/null; then
        info "Local CA installed into the system trust store"
    else
        warn "Could not install local CA (sudo failed or was denied)."
        warn "Run later: portzero trust generate && sudo portzero trust install"
    fi

    # Pin the bare dashboard name in /etc/hosts. nss-mdns (the
    # `mdns4_minimal [NOTFOUND=return]` entry in /etc/nsswitch.conf) claims
    # 2-label *.local names like portzero.local and halts the lookup before
    # systemd-resolved is consulted, so the dashboard name would never resolve
    # even though the overlay DNS server answers it. The dashboard lives at a
    # fixed VIP (10.254.0.2), so a static hosts entry — resolved by `files`,
    # ahead of mdns — is the robust fix. Multi-label service names are unaffected.
    if ! grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
        info "Pinning portzero.local in /etc/hosts (works around nss-mdns)..."
        echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts >/dev/null 2>&1 \
            || warn "Could not pin portzero.local in /etc/hosts; the dashboard name may not resolve."
    fi

    info "Starting portzero and opening http://portzero.local..."
    "$bin_path" start || warn "Could not start portzero automatically. Run later: portzero start"
fi

echo ""
case ":${PATH}:" in
    *":${install_dir}:"*)
        success "portzero installed! ($("$install_dir/portzero" --version 2>/dev/null || echo ok))"
        ;;
    *)
        success "portzero installed to ${install_dir}!"
        warn "${install_dir} is not in your PATH — add: export PATH=\"${install_dir}:\$PATH\""
        ;;
esac

echo ""
echo "Next: ${BOLD}portzero login${RESET}  when you want cloud tunnels"
