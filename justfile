default:
    @just --list

# Install the daemon and CLI from source
install:
    cargo install --path client/crates/cli

# Run the full unprivileged test suite (no root required).
# The root-gated real-TUN e2e (`real_tun_overlay`) skips cleanly here.
test:
    cargo test --workspace

# Run the root-gated real-TUN end-to-end overlay test.
# Requires root/CAP_NET_ADMIN to create the TUN device, so it runs under sudo.
# Without root the test skips cleanly; use `just test` for everyday work.
e2e:
    sudo -E cargo test -p devenv-tunnel-daemon --test overlay_e2e real_tun_overlay -- --nocapture
