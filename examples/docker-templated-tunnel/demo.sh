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
docker build -t portzero-templated-demo . >/dev/null

echo "Starting container with templated PZ_TUNNEL=web-{branch}.tunnel.portzero.cloud ..."
CONTAINER_ID=$(docker run -d \
  --name "portzero-demo-$$" \
  -e PZ_TUNNEL="web-{branch}.tunnel.portzero.cloud" \
  -p 0:8080 \
  portzero-templated-demo)

echo "Container started: $CONTAINER_ID"
echo

# Give it a moment
sleep 1

echo "Container env (inside):"
docker exec "$CONTAINER_ID" env | grep PZ_TUNNEL || true
echo

echo "To see it discovered by the daemon, run in another terminal:"
echo "    portzero status"
echo "    # or: cargo run -p portzero-cli --bin portzero -- status"
echo
echo "Expected to see a domain containing: web-$SANITIZED_BRANCH.tunnel.portzero.cloud"
echo "(The env var must contain the full domain with suffix.)"
echo

echo "Press ENTER to stop and remove the container, or Ctrl-C to leave it running."
read -r

docker rm -f "$CONTAINER_ID" >/dev/null 2>&1 || true
echo "Cleaned up."
