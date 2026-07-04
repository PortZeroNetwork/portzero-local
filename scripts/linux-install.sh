#!/bin/sh
set -e

# portzero installer — the single source of truth.
#
# Usage: curl -fsSL https://portzero.net/install.sh | sh
#   (portzero.net/install.sh serves this file, published as a GitHub
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

find_cmd() {
    if command -v "$1" >/dev/null 2>&1; then
        command -v "$1"
        return 0
    fi

    # Normal users often do not have sbin directories on PATH, but Linux
    # capability tools commonly live there.
    for dir in /usr/sbin /sbin /usr/bin /bin; do
        if [ -x "$dir/$1" ]; then
            printf '%s\n' "$dir/$1"
            return 0
        fi
    done

    return 1
}
has_cmd() { find_cmd "$1" >/dev/null 2>&1; }

install_linux_certutil() {
    if has_cmd certutil; then
        return 0
    fi

    info "Installing NSS certutil so browsers trust *.portzero.local..."
    if has_cmd apt-get; then
        sudo env DEBIAN_FRONTEND=noninteractive apt-get update >/dev/null 2>&1 \
            && sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y libnss3-tools >/dev/null 2>&1 \
            && return 0
    elif has_cmd dnf; then
        sudo dnf install -y nss-tools >/dev/null 2>&1 && return 0
    elif has_cmd yum; then
        sudo yum install -y nss-tools >/dev/null 2>&1 && return 0
    elif has_cmd zypper; then
        sudo zypper --non-interactive install mozilla-nss-tools >/dev/null 2>&1 && return 0
    elif has_cmd pacman; then
        sudo pacman -S --noconfirm --needed nss >/dev/null 2>&1 && return 0
    elif has_cmd apk; then
        sudo apk add nss-tools >/dev/null 2>&1 && return 0
    fi

    warn "Could not install NSS certutil automatically."
    warn "Browsers with NSS stores may not trust *.portzero.local until you install libnss3-tools (Debian/Ubuntu) or nss-tools (Fedora/RHEL)."
    return 1
}

install_linux_resolved_polkit_rule() {
    if [ "$(uname -s)" != "Linux" ]; then
        return 0
    fi
    if [ ! -d /etc/polkit-1/rules.d ]; then
        warn "polkit rules directory not found; systemd-resolved may prompt for DNS setup at login."
        return 1
    fi

    user="$(id -un)"
    case "$user" in
        *[!A-Za-z0-9._-]*|'')
            warn "Cannot install polkit rule for unsupported username: $user"
            return 1
            ;;
    esac

    rule_tmp="$tmp/50-portzero-resolved.rules"
    cat > "$rule_tmp" << RULE
// Managed by portzero installer.
// Allows the portzero user service for $user to attach scoped DNS settings
// to its TUN link without interactive authentication on every login.
polkit.addRule(function(action, subject) {
    if (subject.user !== "$user") {
        return polkit.Result.NOT_HANDLED;
    }

    if (action.id === "org.freedesktop.resolve1.set-dns-servers" ||
        action.id === "org.freedesktop.resolve1.set-domains" ||
        action.id === "org.freedesktop.resolve1.revert") {
        return polkit.Result.YES;
    }

    return polkit.Result.NOT_HANDLED;
});
RULE

    if sudo install -m 0644 "$rule_tmp" /etc/polkit-1/rules.d/50-portzero-resolved.rules 2>/dev/null; then
        info "Installed polkit rule for systemd-resolved scoped DNS setup"
    else
        warn "Could not install systemd-resolved polkit rule (sudo failed or was denied)."
        warn "Without it, *.portzero.local DNS may require an interactive auth prompt after login."
        return 1
    fi
}

open_dashboard() {
    url="http://portzero.local"

    if opener="$(find_cmd xdg-open 2>/dev/null)"; then
        "$opener" "$url" >/dev/null 2>&1 && return 0
    fi
    if opener="$(find_cmd gio 2>/dev/null)"; then
        "$opener" open "$url" >/dev/null 2>&1 && return 0
    fi
    for opener in gnome-open kde-open5 kde-open sensible-browser; do
        if opener_path="$(find_cmd "$opener" 2>/dev/null)"; then
            "$opener_path" "$url" >/dev/null 2>&1 && return 0
        fi
    done

    warn "Could not open $url automatically."
    return 1
}

start_portzero() {
    bin_path="$1"

    if has_cmd systemctl && [ -f "${HOME}/.config/systemd/user/portzero-daemon.service" ]; then
        if systemctl --user enable --now portzero-daemon.service >/dev/null 2>&1; then
            info "Started systemd user service: portzero-daemon.service"
            return 0
        fi
        if systemctl --user restart portzero-daemon.service >/dev/null 2>&1; then
            info "Restarted systemd user service: portzero-daemon.service"
            return 0
        fi
        warn "Could not start portzero via systemd user service; falling back to direct start."
    fi

    "$bin_path" start --no-browser
}

dashboard_probe() {
    url="http://portzero.local/status.json"

    if curl_path="$(find_cmd curl 2>/dev/null)"; then
        "$curl_path" --noproxy '*' -fsS --max-time 2 "$url" >/dev/null 2>&1
        return $?
    fi
    if wget_path="$(find_cmd wget 2>/dev/null)"; then
        "$wget_path" -q -T 2 -O /dev/null "$url" >/dev/null 2>&1
        return $?
    fi

    return 1
}

wait_for_dashboard() {
    info "Waiting for http://portzero.local..."
    i=0
    while [ "$i" -lt 30 ]; do
        if dashboard_probe; then
            info "Dashboard is reachable at http://portzero.local"
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done

    warn "portzero started, but http://portzero.local is not reachable yet."
    warn "Run 'portzero status' for diagnostics, or inspect ~/.portzero/daemon/daemon.log."
    return 1
}

install_dashboard_hosts_pin() {
    expected='10.254.0.2 portzero.local # portzero-local'

    if grep -Fxq "$expected" /etc/hosts 2>/dev/null; then
        return 0
    fi

    info "Pinning portzero.local in /etc/hosts (works around nss-mdns)..."
    tmp_hosts="$tmp/hosts"
    if [ -r /etc/hosts ]; then
        # Replace stale managed entries while preserving user-managed hosts lines.
        grep -v '# portzero-local' /etc/hosts > "$tmp_hosts" || true
    else
        : > "$tmp_hosts"
    fi
    printf '%s\n' "$expected" >> "$tmp_hosts"

    if sudo install -m 0644 "$tmp_hosts" /etc/hosts 2>/dev/null; then
        return 0
    fi

    warn "Could not pin portzero.local in /etc/hosts; the dashboard name may not resolve."
    warn "Run later: echo '$expected' | sudo tee -a /etc/hosts"
    return 1
}

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

portzero_bin_dir="${HOME}/.portzero/bin"
uninstall_helper="${portzero_bin_dir}/portzero-uninstall"
uninstall_url="${RELEASES_URL}/latest/download/linux-uninstall.sh"
info "Downloading ${uninstall_url}"
if has_cmd curl; then
    if curl -fsSL --retry 3 --retry-delay 2 -o "$tmp/linux-uninstall.sh" "$uninstall_url"; then
        mkdir -p "$portzero_bin_dir"
        install -m 0755 "$tmp/linux-uninstall.sh" "$uninstall_helper" 2>/dev/null ||
            warn "Could not install uninstall helper to $uninstall_helper"
    else
        warn "Could not download uninstall helper from ${uninstall_url}"
    fi
elif has_cmd wget; then
    if wget -q --tries=3 -O "$tmp/linux-uninstall.sh" "$uninstall_url"; then
        mkdir -p "$portzero_bin_dir"
        install -m 0755 "$tmp/linux-uninstall.sh" "$uninstall_helper" 2>/dev/null ||
            warn "Could not install uninstall helper to $uninstall_helper"
    else
        warn "Could not download uninstall helper from ${uninstall_url}"
    fi
fi
if [ -x "$uninstall_helper" ]; then
    info "Installed uninstall helper to $uninstall_helper"
fi

# --- Linux post-install: CAP_NET_ADMIN + systemd user unit ---
if [ "$(uname -s)" = "Linux" ]; then
    bin_path="$install_dir/portzero"

    echo ""
    info "Setting up Linux daemon capabilities..."

    # Grant CAP_NET_ADMIN (create TUN devices + modify routing tables) and
    # CAP_NET_BIND_SERVICE (let the embedded DNS server bind 10.254.0.1:53 so
    # *.portzero.local resolves) without running as root. Without the bind cap
    # the DNS server exits with "Permission denied" and no name resolves.
    if setcap_path="$(find_cmd setcap 2>/dev/null)"; then
        if sudo "$setcap_path" 'cap_net_admin,cap_net_bind_service+eip' "$bin_path" 2>/dev/null; then
            info "CAP_NET_ADMIN + CAP_NET_BIND_SERVICE granted to $bin_path"
            if getcap_path="$(find_cmd getcap 2>/dev/null)"; then
                caps="$("$getcap_path" "$bin_path" 2>/dev/null || true)"
                case "$caps" in
                    *cap_net_admin*cap_net_bind_service*|*cap_net_bind_service*cap_net_admin*) ;;
                    *)
                        warn "Could not verify Linux capabilities on $bin_path."
                        warn "If http://portzero.local does not load, run:"
                        warn "  sudo $setcap_path 'cap_net_admin,cap_net_bind_service+eip' $bin_path"
                        ;;
                esac
            fi
        else
            warn "Could not set capabilities (sudo failed or was denied)."
            warn "The local overlay (TUN + routing + DNS) requires root or:"
            warn "  sudo $setcap_path 'cap_net_admin,cap_net_bind_service+eip' $bin_path"
        fi
    else
        warn "setcap not found — install libcap2-bin (Debian/Ubuntu) or libcap (Fedora/RHEL),"
        warn "then run: sudo setcap 'cap_net_admin,cap_net_bind_service+eip' $bin_path"
    fi

    info "Installing systemd-resolved polkit rule..."
    install_linux_resolved_polkit_rule || true

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
    fi

    # Generate a local CA and install it into the system trust store so browsers
    # accept *.portzero.local over HTTPS without certificate warnings.
    info "Setting up local CA certificate..."
    install_linux_certutil || true
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
    install_dashboard_hosts_pin || true

    info "Starting portzero..."
    if start_portzero "$bin_path"; then
        wait_for_dashboard || true
        open_dashboard || true
    else
        warn "Could not start portzero automatically. Run later: portzero start"
    fi
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
info "Local tunnels (*.portzero.local) governed by the PolyForm Shield License:"
info "  https://github.com/PortZeroNetwork/portzero-local/blob/develop/LICENSE"
info "Cloud features governed by https://portzero.net/terms"

echo ""
echo "Uninstall: ${BOLD}${uninstall_helper}${RESET}"
echo "Next: ${BOLD}portzero login${RESET}  when you want cloud tunnels"
