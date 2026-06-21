# Templated DEVENV_TUNNEL for Docker Services (with and without Compose)

This example shows how `DEVENV_TUNNEL` supports templating (e.g. `{branch}`,
`{worktree}`) when running services in Docker containers.

The daemon performs template resolution on the **host** by inspecting
container mounts and compose labels. This lets the same `docker-compose.yml`
or `docker run` command produce unique DNS names per git worktree/branch.

## Prerequisites

- Docker installed
- This repository checked out
- The CLI built: `cargo build -p devenv-tunnel-cli`

## Quick Demo (recommended)

```bash
# 1. Switch to a non-default branch so templating is visible
git checkout -b demo-$(date +%s | tail -c 6)

# 2. In one terminal, start the discovery daemon (local-only mode is fine)
cargo run -p devenv-tunnel-cli --bin devenv-tunnel -- start --foreground

# 3. In another terminal, start the example service
cd examples/docker-templated-tunnel
docker compose up -d

# 4. Check what the daemon discovered
cargo run -p devenv-tunnel-cli --bin devenv-tunnel -- status

# You should see a line containing your branch, e.g.:
#   web-demo-123456.tunnel.devenv.tools   49152   container web

# 5. Clean up
docker compose down
```

## Plain `docker run` (no compose)

```bash
git checkout -b another-demo

# Start daemon in another terminal if not already running
# cargo run -p devenv-tunnel-cli --bin devenv-tunnel -- start --foreground

cd examples/docker-templated-tunnel

docker build -t devenv-demo .

docker run -d \
  --name devenv-demo-web \
  -e DEVENV_TUNNEL=plain-{branch} \
  -p 0:8080 \
  devenv-demo

# Check status again - look for "plain-yourbranch..."
cargo run -p devenv-tunnel-cli --bin devenv-tunnel -- status

docker rm -f devenv-demo-web
```

## How it works

- The container is started with `DEVENV_TUNNEL=web-{branch}` (literal template).
- The daemon runs `docker inspect` (from the host) and extracts:
  - `com.docker.compose.project.working_dir` (when using compose)
  - Bind mount `Source` paths (for both compose and plain `docker run -v`)
- It walks those host paths to find the git root and resolves the template
  using the same `DomainContext` logic used for native processes.
- The resolved name (e.g. `web-demo-123456`) is what gets registered.

This approach works whether you use docker compose or raw `docker run`.

## As a System Test

You can treat the steps above as a manual system test. After running them
you have visually confirmed:

1. Templated `DEVENV_TUNNEL` env var was passed to the container.
2. The host-side daemon resolved `{branch}` using git context from mounts/labels.
3. The resolved domain appears in `devenv-tunnel status`.

A future automated test could spawn a temporary worktree + daemon with a custom
state directory and assert on `routes.json`.

## Troubleshooting

- Make sure you're on a real git branch (not detached HEAD).
- The daemon must be running while you start the container.
- If you see the literal `{branch}` in status, the mount/label discovery failed
  (open an issue with `docker inspect` output of the container).
