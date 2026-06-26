default:
    @just --list

# Install the daemon and CLI from source, grant privileges, and start.
#
# Linux:  grants CAP_NET_ADMIN (one sudo prompt), installs a systemd user
#         service for autostart, then starts the daemon.
# macOS:  installs a root LaunchDaemon (one sudo prompt) that starts
#         immediately and on every boot. No separate `portzero start` needed.
# Windows: installs a scheduled task for autostart. Must be run from an
#          Administrator terminal; instructions are printed if not elevated.
install:
    #!/usr/bin/env bash
    set -euo pipefail
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
        echo "→ Granting CAP_NET_ADMIN (allows TUN creation without running as root)..."
        sudo setcap CAP_NET_ADMIN+ep "$cargo_bin"
        echo "→ Generating CA certificate..."
        portzero trust generate
        echo "→ Installing CA certificate to system trust store..."
        sudo -E portzero trust install
        echo "→ Installing systemd user service for autostart..."
        portzero autostart enable
        echo "→ Starting daemon..."
        portzero start
        ;;
      Darwin*)
        echo "→ Installing root LaunchDaemon (required for utun/TUN access on macOS)..."
        sudo portzero autostart enable
        echo "→ Daemon started via LaunchDaemon."
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

    OS="$(uname -s)"
    case "$OS" in
      Linux*)
        echo "→ Removing CA certificate from system trust store..."
        sudo -E portzero trust uninstall 2>/dev/null || true
        cargo_bin="$HOME/.cargo/bin/portzero"
        if [ -f "$cargo_bin" ]; then
          echo "→ Removing CAP_NET_ADMIN capability..."
          sudo setcap -r "$cargo_bin" 2>/dev/null || true
        fi
        ;;
      Darwin*)
        echo "→ Removing root LaunchDaemon..."
        sudo portzero autostart disable 2>/dev/null || true
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
