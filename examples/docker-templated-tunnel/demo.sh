#!/usr/bin/env bash
#
# Convenience script for the templated Docker example.
# It demonstrates the container side and computes what the resolved
# name *should* look like.
#
# For the full experience (daemon discovery) follow the README steps.
#
set -euo pipefail

cd "$(dirname "$0")"

BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "detached")
SANITIZED_BRANCH=$(echo "$BRANCH" | tr -c '[:alnum:]-' '-' | tr -s '-' | sed 's/^-*//;s/-*$//' | cut -c1-63)

echo "Current git branch: $BRANCH"
echo "Sanitized for DNS:  $SANITIZED_BRANCH"
echo

echo "Building image..."
docker build -t devenv-templated-demo . >/dev/null

echo "Starting container with templated DEVENV_TUNNEL=web-{branch} ..."
CONTAINER_ID=$(docker run -d \
  --name "devenv-demo-$$" \
  -e DEVENV_TUNNEL="web-{branch}" \
  -p 0:8080 \
  devenv-templated-demo)

echo "Container started: $CONTAINER_ID"
echo

# Give it a moment
sleep 1

echo "Container env (inside):"
docker exec "$CONTAINER_ID" env | grep DEVENV_TUNNEL || true
echo

echo "To see it discovered by the daemon, run in another terminal:"
echo "    cargo run -p devenv-tunnel-cli --bin devenv-tunnel -- status"
echo
echo "Expected to see a domain containing: web-$SANITIZED_BRANCH.tunnel.devenv.tools"
echo

echo "Press ENTER to stop and remove the container, or Ctrl-C to leave it running."
read -r

docker rm -f "$CONTAINER_ID" >/dev/null 2>&1 || true
echo "Cleaned up."
