set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

default:
    @just --list

# Install the daemon and CLI from source, grant privileges, and start.
#
# Linux:  grants CAP_NET_ADMIN (one sudo prompt), installs a systemd user
#         service for autostart, then starts the daemon.
# macOS:  installs a root LaunchDaemon (one sudo prompt) that starts
#         immediately and on every boot. No separate `portzero start` needed.
# Windows: installs a scheduled task for autostart. Must be run from an
# Administrator terminal; instructions are printed if not elevated.
[script('powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File')]
[windows]
install:
    & ".\scripts\windows-install.ps1"

[unix]
install:
    #!/usr/bin/env bash
    set -euo pipefail
    has_cmd() { command -v "$1" >/dev/null 2>&1; }
    install_linux_certutil() {
      if has_cmd certutil; then
        return 0
      fi

      echo "→ Installing NSS certutil so browsers trust *.portzero.local..."
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

      echo "warning: Could not install NSS certutil automatically." >&2
      echo "warning: Browsers with NSS stores may not trust *.portzero.local until you install libnss3-tools (Debian/Ubuntu) or nss-tools (Fedora/RHEL)." >&2
      return 1
    }

    cargo install --path client/crates/cli
    cargo_bin="$HOME/.cargo/bin/portzero"
    active="$(command -v portzero 2>/dev/null || true)"
    if [ -n "$active" ] && [ "$active" != "$cargo_bin" ]; then
        echo "Copying $cargo_bin -> $active"
        rm -f "$active" && cp "$cargo_bin" "$active"
    fi

    OS="$(uname -s)"
    case "$OS" in
      Linux*)
        echo "→ Granting CAP_NET_ADMIN + CAP_NET_BIND_SERVICE..."
        echo "  (CAP_NET_ADMIN creates the TUN device; CAP_NET_BIND_SERVICE lets the"
        echo "   embedded DNS server bind 10.254.0.1:53 so *.portzero.local resolves.)"
        sudo setcap 'cap_net_admin,cap_net_bind_service+eip' "$cargo_bin"
        install_linux_certutil || true
        echo "→ Generating CA certificate..."
        portzero trust generate
        echo "→ Installing CA certificate to system trust store..."
        sudo HOME="$HOME" "$cargo_bin" trust install
        echo "→ Installing systemd user service for autostart..."
        portzero autostart enable
        # Pin the bare dashboard name in /etc/hosts. nss-mdns (the
        # `mdns4_minimal [NOTFOUND=return]` entry in /etc/nsswitch.conf) claims
        # 2-label *.local names like `portzero.local` and halts the lookup before
        # systemd-resolved is consulted, so the dashboard name would never resolve
        # even though the overlay DNS server answers it. The management dashboard
        # lives at a fixed VIP (10.254.0.2), so a static hosts entry — resolved by
        # `files`, ahead of mdns — is the robust fix. Multi-label service names
        # (e.g. app.portzero.local) are unaffected and resolve via the overlay DNS.
        echo "→ Pinning portzero.local in /etc/hosts (works around nss-mdns)..."
        if ! grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts >/dev/null
        fi
        ;;
      Darwin*)
        echo "→ Generating CA certificate for *.portzero.local HTTPS..."
        "$cargo_bin" trust generate
        echo "→ Installing CA certificate to system keychain..."
        # Pass HOME explicitly: sudo resets HOME to /var/root but the cert is in
        # the user's Library/Application Support/PortZero/ directory.
        sudo HOME="$HOME" "$cargo_bin" trust install
        echo "→ Installing root LaunchDaemon (required for utun/TUN access on macOS)..."
        # SUDO_USER is set by sudo; install_launchd reads it to pin HOME in the
        # plist so the daemon's state files land in the user's home rather than
        # /var/root — this makes 'portzero status' work without sudo.
        sudo "$cargo_bin" autostart enable
        # Pin portzero.local in /etc/hosts. macOS mDNSResponder claims authority
        # for all *.local names and intercepts them before /etc/resolver/ is
        # consulted, so portzero.local never reaches our embedded DNS server.
        # The management dashboard lives at a fixed VIP (10.254.0.2); a hosts
        # entry is resolved by the 'files' source before mDNS and fixes this.
        # Multi-label service names (e.g. app.portzero.local) resolve fine via
        # the /etc/resolver/portzero.local scoped resolver.
        echo "→ Pinning portzero.local in /etc/hosts (works around mDNS interception)..."
        if ! grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts >/dev/null
        fi
        echo "→ Daemon installed and started via LaunchDaemon."
        ;;
      MINGW*|MSYS*|CYGWIN*)
        echo "→ Installing scheduled task for autostart..."
        portzero autostart enable || {
          echo ""
          echo "  Failed — re-run 'just install' from an Administrator terminal,"
          echo "  or run these commands as Administrator:"
          echo "    portzero autostart enable"
          echo "    portzero start"
          exit 1
        }
        portzero start
        ;;
      *)
        echo "Unknown platform. Run 'portzero autostart enable' and 'portzero start' manually."
        ;;
    esac
    echo ""
    echo "✓ portzero installed. Open http://portzero.local in your browser."

# Uninstall the CLI, stop the daemon, and remove the autostart service.
uninstall:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "→ Stopping daemon and removing autostart service..."
    portzero autostart disable 2>/dev/null || true
    portzero stop 2>/dev/null || true

    cargo_bin="$HOME/.cargo/bin/portzero"
    OS="$(uname -s)"
    case "$OS" in
      Linux*)
        echo "→ Removing CA certificate from system trust store..."
        sudo HOME="$HOME" "$cargo_bin" trust uninstall 2>/dev/null || true
        if [ -f "$cargo_bin" ]; then
          echo "→ Removing capabilities (CAP_NET_ADMIN, CAP_NET_BIND_SERVICE)..."
          sudo setcap -r "$cargo_bin" 2>/dev/null || true
        fi
        if grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo "→ Removing portzero.local pin from /etc/hosts..."
          sudo sed -i '/# portzero-local/d' /etc/hosts
        fi
        ;;
      Darwin*)
        echo "→ Removing root LaunchDaemon..."
        sudo "$cargo_bin" autostart disable 2>/dev/null || true
        echo "→ Removing CA certificate from system keychain..."
        sudo HOME="$HOME" "$cargo_bin" trust uninstall 2>/dev/null || true
        if grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo "→ Removing portzero.local pin from /etc/hosts..."
          sudo sed -i '' '/# portzero-local/d' /etc/hosts
        fi
        ;;
    esac

    cargo uninstall portzero-cli 2>/dev/null || true
    active="$(command -v portzero 2>/dev/null || true)"
    if [ -n "$active" ]; then
        echo "→ Removing $active"
        rm -f "$active"
    fi
    echo "✓ portzero uninstalled."

# Run the full unprivileged test suite (no root required).
# The root-gated real-TUN e2e (`real_tun_overlay`) skips cleanly here.
test:
    cargo test --workspace

# Run the root-gated real-TUN end-to-end overlay test.
# Requires root/CAP_NET_ADMIN to create the TUN device, so it runs under sudo.
# Without root the test skips cleanly; use `just test` for everyday work.
e2e:
    sudo -E cargo test -p portzero-daemon --test overlay_e2e real_tun_overlay -- --nocapture
