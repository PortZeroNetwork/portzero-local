#!/bin/sh
set -e

# portzero Linux uninstaller.
#
# Usage:
#   curl -fsSL https://github.com/PortZeroNetwork/portzero-local/releases/latest/download/linux-uninstall.sh | sh
#
# Removes the Linux setup performed by scripts/linux-install.sh where possible.
# Env: PORTZERO_INSTALL_DIR adds an install directory to the binary search path.

if [ "$(uname -s)" != "Linux" ]; then
    echo "error: scripts/linux-uninstall.sh can only be run on Linux." >&2
    exit 1
fi

info() { printf 'info: %s\n' "$1"; }
warn() { printf 'warn: %s\n' "$1" >&2; }

has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

sudo_if_needed() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}

remove_file() {
    path="$1"
    if [ ! -e "$path" ]; then
        return 0
    fi

    info "Removing $path"
    if rm -f "$path" 2>/dev/null; then
        return 0
    fi
    sudo_if_needed rm -f "$path" 2>/dev/null || warn "Could not remove $path"
}

portzero_candidates() {
    if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PORTZERO_INSTALL_DIR%/}/portzero"
    fi
    if has_cmd portzero; then
        command -v portzero
    fi
    printf '%s\n' \
        "${HOME}/.local/bin/portzero" \
        "${HOME}/.cargo/bin/portzero" \
        "/usr/local/bin/portzero" \
        "/usr/bin/portzero" |
        awk 'NF && !seen[$0]++'
}

portzero_uninstall_candidates() {
    if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PORTZERO_INSTALL_DIR%/}/portzero-uninstall"
    fi
    if has_cmd portzero-uninstall; then
        command -v portzero-uninstall
    fi
    printf '%s\n' \
        "${HOME}/.portzero/bin/portzero-uninstall" \
        "${HOME}/.local/bin/portzero-uninstall" \
        "/usr/local/bin/portzero-uninstall" \
        "/usr/bin/portzero-uninstall" |
        awk 'NF && !seen[$0]++'
}

portzero_tray_candidates() {
    if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PORTZERO_INSTALL_DIR%/}/portzero-tray"
    fi
    if has_cmd portzero-tray; then
        command -v portzero-tray
    fi
    printf '%s\n' \
        "${HOME}/.local/bin/portzero-tray" \
        "${HOME}/.cargo/bin/portzero-tray" \
        "/usr/local/bin/portzero-tray" \
        "/usr/bin/portzero-tray" |
        awk 'NF && !seen[$0]++'
}

run_portzero_best_effort() {
    args="$1"
    for candidate in $(portzero_candidates); do
        if [ -x "$candidate" ]; then
            # shellcheck disable=SC2086
            "$candidate" $args >/dev/null 2>&1 && return 0
        fi
    done
    return 1
}

first_existing_portzero() {
    for candidate in $(portzero_candidates); do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

remove_hosts_pin() {
    if ! grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
        return 0
    fi

    info "Removing portzero.local pin from /etc/hosts"
    tmp="${TMPDIR:-/tmp}/portzero-hosts.$$"
    trap 'rm -f "$tmp"' EXIT HUP INT TERM

    grep -v '# portzero-local' /etc/hosts > "$tmp" || true
    sudo_if_needed install -m 0644 "$tmp" /etc/hosts 2>/dev/null ||
        warn "Could not update /etc/hosts"
    rm -f "$tmp"
    trap - EXIT HUP INT TERM
}

info "Stopping the tray companion and removing its autostart entry"
if has_cmd pkill; then
    pkill -x portzero-tray >/dev/null 2>&1 || true
fi
remove_file "${HOME}/.config/autostart/portzero-tray.desktop"

info "Stopping daemon and removing autostart service"
run_portzero_best_effort "autostart disable" || true
run_portzero_best_effort "stop" || true
if has_cmd systemctl; then
    systemctl --user disable --now portzero-daemon.service >/dev/null 2>&1 || true
fi
remove_file "${HOME}/.config/systemd/user/portzero-daemon.service"
if has_cmd systemctl; then
    systemctl --user daemon-reload >/dev/null 2>&1 || true
fi

if bin_path="$(first_existing_portzero 2>/dev/null)"; then
    info "Removing local CA certificate from system trust store"
    sudo_if_needed env HOME="$HOME" "$bin_path" trust uninstall >/dev/null 2>&1 ||
        warn "Could not remove the local CA from every trust store"

    if has_cmd setcap; then
        info "Removing Linux capabilities from $bin_path"
        sudo_if_needed setcap -r "$bin_path" >/dev/null 2>&1 || true
    fi
else
    warn "portzero binary not found; skipping CLI-driven cleanup"
fi

remove_hosts_pin
remove_file /etc/polkit-1/rules.d/50-portzero-resolved.rules

for candidate in $(portzero_candidates); do
    remove_file "$candidate"
done

for candidate in $(portzero_tray_candidates); do
    remove_file "$candidate"
done

for candidate in $(portzero_uninstall_candidates); do
    remove_file "$candidate"
done

info "portzero uninstalled."
