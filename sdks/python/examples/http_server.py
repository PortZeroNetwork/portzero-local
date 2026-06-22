#!/usr/bin/env python3
"""
devenv-tunnel Python example — stdlib http.server (no pip install needed)

Usage:
    DEVENV_TUNNEL=myapp-mybranch.devenv.local python3 examples/http_server.py

DEVENV_TUNNEL must be set BEFORE starting this process. Setting it at
runtime (os.environ["DEVENV_TUNNEL"] = ...) does NOT work for daemon
discovery — the daemon reads /proc/<pid>/environ which is frozen at
execve() time.

Recommended: use direnv (see sdks/direnv/README.md) or shell export.

The daemon on the host resolves {branch}/{worktree} templates; this
process just needs to bind port 0 and inherit DEVENV_TUNNEL.
"""

from __future__ import annotations

import http.server
import logging
import os
import sys

# Adjust if running from a different directory
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from devenv_tunnel import find_free_port  # noqa: E402

logging.basicConfig(level=logging.INFO, format="%(message)s")


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        tunnel = os.environ.get("DEVENV_TUNNEL", "(not set)")
        body = (
            "devenv-tunnel Python example\n"
            f"DEVENV_TUNNEL: {tunnel}\n"
            f"Request: {self.command} {self.path}\n"
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        logging.info(f"[http] {fmt}", *args)


def main() -> None:
    # DEVENV_TUNNEL must already be set in the environment before running.
    sock, port = find_free_port(service_name="example-http")

    server = http.server.HTTPServer(("0.0.0.0", 0), Handler)
    # Replace the auto-bound socket with our pre-bound port-0 socket.
    server.socket.close()
    server.socket = sock
    server.server_address = sock.getsockname()

    print(f"[example] Listening on http://0.0.0.0:{port}")
    print("[example] The devenv-tunnel daemon routes DEVENV_TUNNEL -> this port.")

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n[example] Shutting down.")


if __name__ == "__main__":
    main()
