# Templated PORT_ZERO for Docker Services (with and without Compose)

> **This example demonstrates templated `PORT_ZERO` values.**
>
> The value you provide must be a **full domain name** (including suffix).
> The daemon uses the suffix to decide cloud tunnel vs. local overlay.
>
> Examples:
> - `web-{branch}.tunnel.portzero.cloud` → cloud tunnel
> - `web-{branch}.devenv.local` → local virtual overlay

See the notes below for how the two paths are distinguished.

## What the daemon actually does

The thing you run is the discovery daemon:

```bash
port-zero start          # background (after cargo install --path client/crates/cli)
# or
cargo run -p port-zero-cli --bin port-zero -- start --foreground
```

- It scans processes and Docker containers for `PORT_ZERO`.
- The value **must be a full domain** (no implicit suffixes are added).
- The suffix decides the target:
  - ends with `.devenv.local` → local virtual overlay
  - ends with `.tunnel.portzero.cloud` (or namespaced `... .username.tunnel...`) → cloud tunnel
- The example below shows that the daemon can resolve `{branch}` / `{worktree}` templates for containers by inspecting bind mounts and compose labels on the **host**.

You do **not** install or run anything special from inside `examples/...`. The daemon is a single long-lived process.

## Prerequisites

- Docker installed
- This repository checked out
- The CLI built or installed: `cargo install --path client/crates/cli` (gives you `port-zero` and the `devenv` dispatcher)

## Quick Demo (recommended)

```bash
# 1. Switch to a non-default branch so templating is visible
git checkout -b demo-$(date +%s | tail -c 6)

# 2. In one terminal, start the discovery daemon (local-only mode is fine)
cargo run -p port-zero-cli --bin port-zero -- start --foreground

# 3. In another terminal, start the example service
cd examples/docker-templated-tunnel
docker compose up -d

# 4. Check what the daemon discovered
cargo run -p port-zero-cli --bin port-zero -- status

# You should see a line containing your branch, e.g.:
#   web-demo-123456.tunnel.portzero.cloud   49152   container web
#
# (The port shown is the *host* port Docker assigned because you used -p 0:8080.)
# The full domain (including suffix) came from the PORT_ZERO value.

# 5. Clean up
docker compose down
```

## Full domain names only (suffix decides the target)

`PORT_ZERO` must always contain a full domain name (the suffix is part of the value; nothing is appended implicitly).

- `... .tunnel.portzero.cloud` (including namespaced `foo.username.tunnel.portzero.cloud`) → cloud tunnel route
- `... .devenv.local` (or ending `.local`) → local virtual overlay

To use the local overlay path:

```bash
PORT_ZERO=my-db.devenv.local
PORT_ZERO=db-{branch}.devenv.local
```

This example focuses on templating + Docker discovery. You select the path by what full name you put in the variable.

## Plain `docker run` (no compose)

```bash
git checkout -b another-demo

# Start daemon in another terminal if not already running
# cargo run -p port-zero-cli --bin port-zero -- start --foreground

cd examples/docker-templated-tunnel

docker build -t devenv-demo .

docker run -d \
  --name devenv-demo-web \
  -e PORT_ZERO=plain-{branch}.tunnel.portzero.cloud \
  -p 0:8080 \
  devenv-demo

# Check status again - look for "plain-yourbranch.tunnel.portzero.cloud"
port-zero status   # or the long cargo run ... form

# The PORT_ZERO value contained the full domain name.

docker rm -f devenv-demo-web
```

## How it works

- The container is started with a full template including the suffix, e.g.
  `PORT_ZERO=web-{branch}.tunnel.portzero.cloud` (or `.devenv.local` for overlay).
- The daemon runs `docker inspect` (from the host) and extracts:
  - `com.docker.compose.project.working_dir` (when using compose)
  - Bind mount `Source` paths (for both compose and plain `docker run -v`)
- It walks those host paths to find the git root and resolves the template
  using the same `DomainContext` logic used for native processes.
- The resolved **full domain** is what gets used (no suffixes are added by the daemon).
- For `.tunnel...` names: if logged in, this becomes a public URL at the edge.
- For `.devenv.local` names: routed to the local overlay (when implemented).
- `status` shows tunnel routes that were discovered.

This approach works whether you use docker compose or raw `docker run`.

## As a System Test

You can treat the steps above as a manual system test for the templated
Docker discovery path. After running them you have visually confirmed:

1. A full domain template (with suffix) was passed via `PORT_ZERO`.
2. The host-side daemon resolved `{branch}` using git context from mounts/labels.
3. The resolved full domain appears in `port-zero status`.

The same `PORT_ZERO` mechanism with a `.devenv.local` suffix selects
the local overlay path instead of cloud tunnels.

## Troubleshooting

- Make sure you're on a real git branch (not detached HEAD).
- The daemon must be running while you start the container (`port-zero start` or the cargo invocation above).
- If you see the literal `{branch}` in status, the mount/label discovery failed
  (open an issue with `docker inspect` output of the container).
- To see the daemon's own logs: `~/.devenv/daemon/daemon.log`
- Check daemon state: `port-zero status`

## Running the daemon (general)

You start it once and leave it running:

```bash
# After `cargo install --path client/crates/cli`
port-zero start

# Foreground for development
cargo run -p port-zero-cli --bin port-zero -- start --foreground

# See what it found
port-zero status

# Stop it
port-zero stop
```

It writes state to `~/.devenv/daemon/`. On Linux it can discover native processes (via `/proc`) and containers without any special per-example setup.
