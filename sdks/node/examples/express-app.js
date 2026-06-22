/**
 * devenv-tunnel Node.js example — plain http (no framework dependency)
 *
 * Usage:
 *   DEVENV_TUNNEL=myapp-mybranch.devenv.local node examples/express-app.js
 *
 * DEVENV_TUNNEL must be set BEFORE starting this process. Setting it at
 * runtime (process.env.DEVENV_TUNNEL = ...) does NOT work for daemon
 * discovery — the daemon reads /proc/<pid>/environ which is frozen at
 * execve() time.
 *
 * Recommended: use direnv (see sdks/direnv/README.md) or shell export.
 *
 * The daemon on the host resolves {branch}/{worktree} templates; this
 * process just needs to bind port 0 and inherit DEVENV_TUNNEL.
 */

"use strict";

const http = require("http");
const path = require("path");

// Adjust the path if you copy this example outside the sdks/node/ directory.
const { listenWithTunnel } = require(path.join(__dirname, ".."));

const server = http.createServer((req, res) => {
  const tunnel = process.env.DEVENV_TUNNEL || "(not set)";
  res.writeHead(200, { "Content-Type": "text/plain" });
  res.end(
    [
      "devenv-tunnel example",
      `DEVENV_TUNNEL: ${tunnel}`,
      `Request: ${req.method} ${req.url}`,
    ].join("\n") + "\n"
  );
});

// DEVENV_TUNNEL must already be set in the environment before this process started.
listenWithTunnel(server, {
  serviceName: "example-http",
}).then((port) => {
  console.log(`[example] Listening on http://0.0.0.0:${port}`);
  console.log("[example] The devenv-tunnel daemon routes DEVENV_TUNNEL -> this port.");
});
