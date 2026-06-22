#!/usr/bin/env python3
"""Simple HTTP server that reports the DEVENV_TUNNEL value it received.

The value is expected to be a full domain name (with suffix).
This makes it easy to visually confirm that templated full-domain
configuration reached the container and was discovered by the daemon.
"""
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        tunnel_var = os.environ.get("DEVENV_TUNNEL", "<not set>")
        branch = os.environ.get("GIT_BRANCH", "<unknown>")
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        body = (
            "DEVENV_TUNNEL=" + tunnel_var + "\n"
            "This container was started with a full templated domain in DEVENV_TUNNEL.\n"
            "If the daemon is running and discovered us, you should see\n"
            "the resolved full domain in:\n"
            "    devenv-tunnel status\n"
            "\n"
            "The value must include the suffix (.tunnel.devenv.tools or .devenv.local).\n"
        )
        self.wfile.write(body.encode("utf-8"))

    def log_message(self, format, *args):
        # quieter
        pass

if __name__ == "__main__":
    port = 8080
    print(f"Starting demo server on port {port} ...")
    print(f"DEVENV_TUNNEL={os.environ.get('DEVENV_TUNNEL', '<not set>')}")
    server = HTTPServer(("", port), Handler)
    server.serve_forever()
