default:
    @just --list

# Install the daemon and CLI from source
install:
    cargo install --path client/crates/cli
