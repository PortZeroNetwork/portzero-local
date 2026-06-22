#!/usr/bin/env python3
"""Minimal stdlib-only HTTP service for the local .devenv.local overlay demo.

It binds to **port 0** (the OS picks an ephemeral port) and serves a tiny
response on every path. The devenv-tunnel daemon discovers this process,
reads DEVENV_TUNNEL from /proc/<pid>/environ, finds the real ephemeral port,
assigns a virtual IP, and makes the service reachable at
http://<name>.devenv.local/.

DEVENV_TUNNEL must be set BEFORE this process starts (see README / .envrc).
This script never sets it at runtime — a runtime change is invisible to the
daemon, which reads the frozen execve() environment snapshot.
"""

from __future__ import annotations

import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (stdlib naming)
        tunnel = os.environ.get("DEVENV_TUNNEL", "<unset>")
        body = (
            "Hello from the devenv-tunnel local overlay!\n"
            f"DEVENV_TUNNEL={tunnel}\n"
            f"served on real port {self.server.server_address[1]}\n"
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args) -> None:  # quieter logs
        sys.stderr.write("[server] " + (fmt % args) + "\n")


def main() -> None:
    tunnel = os.environ.get("DEVENV_TUNNEL")
    if not tunnel:
        sys.stderr.write(
            "WARNING: DEVENV_TUNNEL is not set. The daemon reads the process\n"
            "environment at launch time and cannot see a value set after start.\n"
            "Set it before launching, e.g.:\n"
            "  export DEVENV_TUNNEL=hello.devenv.local\n"
            "  direnv allow   (if using the bundled .envrc)\n\n"
        )

    # Bind to port 0 -> the OS assigns an ephemeral port.
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = httpd.server_address[1]
    sys.stderr.write(
        f"[server] bound to 127.0.0.1:{port} (ephemeral)\n"
        f"[server] DEVENV_TUNNEL={tunnel or '<unset>'}\n"
        f"[server] once the daemon (run with sudo) is up, try:\n"
        f"[server]   curl http://{tunnel or 'hello.devenv.local'}/\n"
    )
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        sys.stderr.write("\n[server] shutting down\n")
        httpd.shutdown()


if __name__ == "__main__":
    main()
