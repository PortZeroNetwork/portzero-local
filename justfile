default:
    @just --list

# Install the daemon and CLI from source.
# Installs to ~/.cargo/bin, then copies to any other portzero on PATH
# (e.g. ~/.local/bin) so a running daemon picks up the new binary immediately.
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

# Run the full unprivileged test suite (no root required).
# The root-gated real-TUN e2e (`real_tun_overlay`) skips cleanly here.
test:
    cargo test --workspace

# Run the root-gated real-TUN end-to-end overlay test.
# Requires root/CAP_NET_ADMIN to create the TUN device, so it runs under sudo.
# Without root the test skips cleanly; use `just test` for everyday work.
e2e:
    sudo -E cargo test -p portzero-daemon --test overlay_e2e real_tun_overlay -- --nocapture
