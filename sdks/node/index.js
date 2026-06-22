/**
 * devenv-tunnel Node.js helper
 *
 * Thin convenience wrapper — the real mechanism is:
 *   1. Set DEVENV_TUNNEL to a full domain BEFORE launching your process.
 *      (Use direnv, shell export, or docker -e — see sdks/direnv/README.md)
 *   2. Bind to port 0 (OS picks ephemeral port).
 *   3. The devenv-tunnel daemon discovers the process via /proc/<pid>/environ
 *      (Linux) or sysctl KERN_PROCARGS2 (macOS), reads DEVENV_TUNNEL, finds
 *      the real port, and routes it.
 *
 * IMPORTANT: The daemon reads the process environment from OUTSIDE the process.
 * That snapshot is frozen at execve() time. Setting process.env.DEVENV_TUNNEL
 * at runtime updates only the in-process libc copy — the daemon NEVER sees it.
 * You MUST set DEVENV_TUNNEL before starting the process.
 *
 * No agent or sidecar is installed inside the process. Just env var + port 0.
 */

"use strict";

const net = require("net");
const { execSync } = require("child_process");

/**
 * Attempt to resolve {branch} and {worktree} placeholders locally for
 * display/logging purposes only. The daemon resolves these independently
 * from cwd/git context — this local resolution is purely informational.
 *
 * @param {string} template - Domain template possibly containing {branch}/{worktree}
 * @returns {string} Template with placeholders substituted (best-effort)
 */
function resolveTemplateForDisplay(template) {
  if (!template.includes("{branch}") && !template.includes("{worktree}")) {
    return template;
  }

  let branch = null;
  let worktree = null;

  try {
    branch = execSync("git rev-parse --abbrev-ref HEAD", {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: 2000,
    }).trim();
  } catch (_) {
    // Non-fatal — git may not be available or cwd may not be a repo
  }

  try {
    const worktreeRoot = execSync("git rev-parse --show-toplevel", {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: 2000,
    }).trim();
    worktree = require("path").basename(worktreeRoot);
  } catch (_) {
    // Non-fatal
  }

  let resolved = template;
  if (branch) resolved = resolved.replace(/\{branch\}/g, branch);
  if (worktree) resolved = resolved.replace(/\{worktree\}/g, worktree);
  return resolved;
}

/**
 * Listen on port 0 (OS-assigned ephemeral port) and log the DEVENV_TUNNEL value.
 *
 * Works with any object that exposes a `listen(port, [host], callback)` method:
 * Node.js `http.Server`, Express app, Fastify instance, etc.
 *
 * DEVENV_TUNNEL must be set BEFORE starting the process (via direnv, shell
 * export, or docker -e). This library cannot set it for daemon discovery.
 *
 * @param {object} server - An http.Server (or compatible) instance
 * @param {object} [options]
 * @param {string} [options.host="0.0.0.0"] - Interface to bind on
 * @param {string} [options.serviceName] - Name shown in log lines (default: "app")
 * @returns {Promise<number>} Resolves with the actual port assigned by the OS
 */
function listenWithTunnel(server, options = {}) {
  const { host = "0.0.0.0", serviceName = "app" } = options;

  const tunnel = process.env.DEVENV_TUNNEL;

  return new Promise((resolve, reject) => {
    server.listen(0, host, (err) => {
      if (err) {
        reject(err);
        return;
      }

      const address = server.address();
      const port =
        typeof address === "object" && address !== null ? address.port : null;

      if (port === null) {
        reject(
          new Error("Could not determine bound port from server.address()")
        );
        return;
      }

      if (tunnel) {
        const displayDomain = resolveTemplateForDisplay(tunnel);
        const displayNote =
          displayDomain !== tunnel
            ? ` (informational local resolution — daemon resolves independently)`
            : "";
        console.log(
          `[devenv-tunnel] ${serviceName} bound to ${host}:${port} — tunnel domain: ${displayDomain}${displayNote}`
        );
      } else {
        console.warn(
          `[devenv-tunnel] WARNING: ${serviceName} bound to ${host}:${port} — DEVENV_TUNNEL is not set.\n` +
            "  The daemon reads the process environment at launch time and cannot see runtime changes.\n" +
            "  Set DEVENV_TUNNEL before starting your process:\n" +
            "    • direnv: add 'export DEVENV_TUNNEL=myapp-$(git rev-parse --abbrev-ref HEAD).devenv.local' to .envrc\n" +
            "    • shell:  export DEVENV_TUNNEL=myapp-{branch}.devenv.local\n" +
            "    • docker: docker run -e DEVENV_TUNNEL=myapp-{branch}.devenv.local ...\n" +
            "  See sdks/direnv/README.md for the recommended direnv setup."
        );
      }

      resolve(port);
    });
  });
}

/**
 * Create a bare TCP server bound to port 0, resolve with the port number.
 *
 * Useful when you want just the port without passing an http.Server.
 * DEVENV_TUNNEL must be set BEFORE starting the process.
 *
 * @param {object} [options]
 * @param {string} [options.host="0.0.0.0"]
 * @param {string} [options.serviceName]
 * @returns {Promise<{port: number, close: () => Promise<void>}>}
 */
function reservePort(options = {}) {
  const server = net.createServer();
  return listenWithTunnel(server, options).then((port) => ({
    port,
    close: () =>
      new Promise((resolve, reject) =>
        server.close((err) => (err ? reject(err) : resolve()))
      ),
  }));
}

module.exports = { listenWithTunnel, reservePort };
