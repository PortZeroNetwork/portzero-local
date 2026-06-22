#!/usr/bin/env bash
#
# verify.sh — automated end-to-end check for the LOCAL `.devenv.local` overlay.
#
# This is the "fuller check" referenced in the README and in docs/troubleshooting.md.
# It is the manual counterpart to the root-gated `real_tun_overlay` integration test:
# it proves the *whole* path on a real machine, including the part the in-process
# test can't — that the system resolver actually routes `*.devenv.local` to the
# overlay (this is exactly what task-15 fixes on systemd-resolved-without-networkd
# / NetworkManager boxes).
#
# What it does:
#   1. starts the example service (server.py) bound to port 0 with DEVENV_TUNNEL set,
#   2. starts the devenv-tunnel daemon in the foreground (needs root for the TUN),
#   3. asserts `resolvectl query <name>` returns a 10.254.x.x overlay VIP,
#   4. asserts `curl http://<name>:<canonical-port>/` reaches the service THROUGH
#      the overlay (DNS -> VIP -> TUN -> smoltcp -> real ephemeral backend). The
#      canonical port comes from the `:<port>` declared in DEVENV_TUNNEL, NOT the
#      random ephemeral port the service actually bound,
#   5. tears everything down and asserts the scoped DNS config is gone.
#
# It must run as root because creating the TUN device + configuring scoped DNS
# requires it. Because cargo usually isn't on root's PATH, this script uses the
# PRE-BUILT binary and refuses to run if it's missing (build it first as your
# normal user). Run it like:
#
#   cargo build -p devenv-tunnel-cli         # as your normal user, once
#   sudo ./examples/local-overlay/verify.sh  # the check itself
#
# Optionally pass a custom name (with an optional canonical :port):
#   sudo ./verify.sh db.devenv.local:5432
#
# Exit status is 0 only if every check passes.

set -uo pipefail

# DEVENV_TUNNEL value: a full `.devenv.local` domain plus a CANONICAL `:port`.
# The overlay exposes the service on VIP:<canonical-port> (here 8080) and proxies
# to the real ephemeral backend, so clients use a clean, stable port.
NAME="${1:-hello.devenv.local:8080}"
SCAN_WAIT_SECS="${SCAN_WAIT_SECS:-6}"

# Split the optional trailing `:<port>` off the value. DOMAIN is what gets
# resolved via DNS; CANONICAL_PORT is what we curl. If no `:port` is given we
# fall back to the discovered ephemeral port for CHECK 2 (see below).
DOMAIN="${NAME%:*}"
if [[ "$NAME" == *:* ]]; then
  CANONICAL_PORT="${NAME##*:}"
else
  CANONICAL_PORT=""
fi

# --- locate repo + binary --------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR" && git rev-parse --show-toplevel 2>/dev/null || echo "$SCRIPT_DIR/../..")"
BIN="$REPO_ROOT/target/debug/devenv-tunnel"

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
info()  { printf '\033[36m%s\033[0m\n' "$*"; }

if [[ "$(id -u)" -ne 0 ]]; then
  red "This check must run as root (it creates a TUN + configures scoped DNS)."
  echo "  build first:  cargo build -p devenv-tunnel-cli"
  echo "  then run:     sudo $0 ${NAME}"
  exit 2
fi

if [[ ! -x "$BIN" ]]; then
  red "Binary not found: $BIN"
  echo "Build it first as your normal user:  cargo build -p devenv-tunnel-cli"
  exit 2
fi

if [[ "$DOMAIN" != *.devenv.local ]]; then
  red "Name must end in .devenv.local (the overlay path). Got: $DOMAIN"
  exit 2
fi

SVC_LOG="$(mktemp)"
DAEMON_LOG="$(mktemp)"
SVC_PID=""
DAEMON_PID=""
FAILURES=0

cleanup() {
  info "--- teardown ---"
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    # SIGTERM -> graceful shutdown: removes scoped DNS config + tears down the TUN.
    kill -TERM "$DAEMON_PID" 2>/dev/null
    for _ in $(seq 1 20); do kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.25; done
    kill -9 "$DAEMON_PID" 2>/dev/null || true
  fi
  [[ -n "$SVC_PID" ]] && kill "$SVC_PID" 2>/dev/null || true

  # After a clean shutdown the scoped entry should be gone. Flush the resolver
  # cache first: systemd-resolved caches the A record for its TTL, so a query
  # right after shutdown can return the old VIP "from cache" even though the
  # scoped config was reverted. A real leak resolves "from network" after a flush.
  sleep 0.5
  resolvectl flush-caches 2>/dev/null || true
  local after
  after="$(resolvectl query "$DOMAIN" 2>&1 || true)"
  if echo "$after" | grep -q '10\.254\.'; then
    red "CHECK (teardown): $DOMAIN STILL resolves to a VIP after cache flush — scoped config leaked:"
    echo "$after" | sed 's/^/    /'
    FAILURES=$((FAILURES + 1))
  else
    green "CHECK (teardown): scoped DNS for $NAME removed cleanly (no resolution after cache flush)."
  fi

  rm -f "$SVC_LOG" "$DAEMON_LOG"
  echo
  if [[ "$FAILURES" -eq 0 ]]; then
    green "ALL CHECKS PASSED ✔"
  else
    red "$FAILURES CHECK(S) FAILED x"
  fi
  exit "$FAILURES"
}
trap cleanup EXIT INT TERM

# --- 1. start the backend service (port 0) ---------------------------------
info "--- starting example service (DEVENV_TUNNEL=$NAME, port 0) ---"
DEVENV_TUNNEL="$NAME" python3 "$SCRIPT_DIR/server.py" >"$SVC_LOG" 2>&1 &
SVC_PID=$!
# server.py prints: "[server] bound to 127.0.0.1:<port> (ephemeral)"
PORT=""
for _ in $(seq 1 20); do
  PORT="$(grep -oE 'bound to 127\.0\.0\.1:[0-9]+' "$SVC_LOG" | grep -oE '[0-9]+$' | head -1)"
  [[ -n "$PORT" ]] && break
  kill -0 "$SVC_PID" 2>/dev/null || { red "service exited early:"; cat "$SVC_LOG"; exit 1; }
  sleep 0.25
done
if [[ -z "$PORT" ]]; then red "could not determine service port"; cat "$SVC_LOG"; exit 1; fi
green "service up: pid=$SVC_PID  real backend=127.0.0.1:$PORT"

# --- 2. start the daemon (foreground, root) --------------------------------
info "--- starting daemon (foreground) ---"
RUST_LOG="${RUST_LOG:-info}" "$BIN" start --foreground >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
info "daemon pid=$DAEMON_PID; waiting ${SCAN_WAIT_SECS}s for TUN bring-up + a scan..."
sleep "$SCAN_WAIT_SECS"

if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
  red "daemon exited early:"; cat "$DAEMON_LOG"; DAEMON_PID=""; exit 1
fi

# Surface the exact failure task-15 targets, if present.
if grep -qi 'network1.service not found' "$DAEMON_LOG"; then
  red "REGRESSION: saw the systemd-networkd error task-15 was supposed to fix:"
  grep -i 'network1' "$DAEMON_LOG" | sed 's/^/    /'
  FAILURES=$((FAILURES + 1))
fi

# --- 3. DNS resolution check (the core task-15 assertion) ------------------
info "--- check 1: resolvectl query $DOMAIN -> overlay VIP ---"
QUERY="$(resolvectl query "$DOMAIN" 2>&1 || true)"
echo "$QUERY" | sed 's/^/    /'
VIP="$(echo "$QUERY" | grep -oE '10\.254\.[0-9]+\.[0-9]+' | head -1)"
if [[ -n "$VIP" ]]; then
  green "CHECK 1 PASSED: $DOMAIN resolves to overlay VIP $VIP"
else
  red "CHECK 1 FAILED: $DOMAIN did not resolve to a 10.254.x.x VIP."
  echo "    (daemon log tail:)"; tail -8 "$DAEMON_LOG" | sed 's/^/    /'
  FAILURES=$((FAILURES + 1))
fi

# --- 4. end-to-end curl THROUGH the overlay --------------------------------
# Use the CANONICAL port from DEVENV_TUNNEL's `:<port>` (a clean, stable number),
# NOT the random ephemeral port the service actually bound. If no canonical port
# was declared, fall back to the discovered ephemeral port.
CURL_PORT="${CANONICAL_PORT:-$PORT}"
info "--- check 2: curl http://$DOMAIN:$CURL_PORT/ through the overlay ---"
BODY="$(curl -fsS --max-time 8 "http://$DOMAIN:$CURL_PORT/" 2>&1 || true)"
echo "$BODY" | sed 's/^/    /'
if echo "$BODY" | grep -q 'devenv-tunnel local overlay'; then
  green "CHECK 2 PASSED: reached the service through the overlay (DNS -> VIP -> TUN -> smoltcp -> backend)."
else
  red "CHECK 2 FAILED: did not get the expected response via http://$DOMAIN:$CURL_PORT/"
  FAILURES=$((FAILURES + 1))
fi

# teardown + final verdict happen in the EXIT trap.
