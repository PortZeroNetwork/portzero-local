"""
port_zero — thin Python helper for port-zero.

The universal mechanism (no real SDK needed):
  1. Set PORT_ZERO to a full domain name BEFORE starting your process.
     The suffix decides the route:
       - myapp-{branch}.devenv.local       → local virtual overlay
       - myapp-{branch}.tunnel.portzero.cloud → cloud tunnel
     See sdks/direnv/README.md for the recommended direnv setup.
  2. Bind your server to port 0 — the OS assigns an ephemeral port.
  3. The port-zero daemon discovers the process, reads PORT_ZERO,
     finds the real port, and routes traffic to it.

IMPORTANT — environment visibility:
  The daemon reads each process's environment from OUTSIDE the process
  (/proc/<pid>/environ on Linux, sysctl KERN_PROCARGS2 on macOS). Both
  sources are frozen snapshots from execve() time. Setting
  os.environ["PORT_ZERO"] at runtime updates only the in-process
  libc copy and is NEVER visible to the daemon.

  This module does NOT and CANNOT set PORT_ZERO for daemon discovery.
  Set it before starting your process (direnv / shell export / docker -e).

This module is stdlib-only (no pip install needed).
"""

from __future__ import annotations

import logging
import os
import socket
import subprocess
from typing import Optional

logger = logging.getLogger(__name__)


def _resolve_template_for_display(template: str) -> str:
    """Resolve {branch}/{worktree} placeholders locally for logging only.

    The daemon resolves these independently from cwd/git context — this
    local resolution is purely informational. Failure is non-fatal.
    """
    if "{branch}" not in template and "{worktree}" not in template:
        return template

    branch: Optional[str] = None
    worktree: Optional[str] = None

    try:
        branch = subprocess.check_output(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"],
            stderr=subprocess.DEVNULL,
            timeout=2,
        ).decode().strip()
    except Exception:
        pass  # Non-fatal

    try:
        worktree_root = subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"],
            stderr=subprocess.DEVNULL,
            timeout=2,
        ).decode().strip()
        worktree = os.path.basename(worktree_root)
    except Exception:
        pass  # Non-fatal

    resolved = template
    if branch:
        resolved = resolved.replace("{branch}", branch)
    if worktree:
        resolved = resolved.replace("{worktree}", worktree)
    return resolved


def read_tunnel() -> Optional[str]:
    """Read PORT_ZERO from the environment.

    Returns the value, or None if unset.  Does NOT set the variable —
    the daemon reads /proc/<pid>/environ at launch time and cannot see
    runtime os.environ changes.
    """
    return os.environ.get("PORT_ZERO")


def find_free_port(
    host: str = "0.0.0.0",
    *,
    service_name: str = "app",
    log: bool = True,
) -> tuple[socket.socket, int]:
    """Bind a TCP socket to port 0 and return ``(sock, port)``.

    The caller is responsible for closing the socket (or passing it to a
    framework that takes ownership of it).

    PORT_ZERO must already be set in the environment before this
    process was started.  If it is unset, a WARNING is logged with setup
    instructions.

    Parameters
    ----------
    host:
        Interface to bind on. Defaults to all interfaces.
    service_name:
        Label used in log output.
    log:
        Emit an INFO log line (or WARNING when PORT_ZERO is unset).
        Set to False to suppress output.

    Returns
    -------
    (sock, port)
        ``sock`` is already bound; pass it to your server framework or close
        it after recording the port.
    """
    tunnel = read_tunnel()

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((host, 0))
    _, port = sock.getsockname()

    if log:
        if tunnel:
            display = _resolve_template_for_display(tunnel)
            note = (
                " (informational local resolution — daemon resolves independently)"
                if display != tunnel
                else ""
            )
            logger.info(
                "[port-zero] %s bound to %s:%d — tunnel domain: %s%s",
                service_name,
                host,
                port,
                display,
                note,
            )
        else:
            logger.warning(
                "[port-zero] WARNING: %s bound to %s:%d — PORT_ZERO is not set.\n"
                "  The daemon reads the process environment at launch time and cannot see "
                "runtime os.environ changes.\n"
                "  Set PORT_ZERO before starting your process:\n"
                "    direnv:  add export PORT_ZERO=myapp-$(git rev-parse --abbrev-ref HEAD)"
                ".devenv.local  to .envrc\n"
                "    shell:   export PORT_ZERO=myapp-{branch}.devenv.local\n"
                "    docker:  docker run -e PORT_ZERO=myapp-{branch}.devenv.local ...\n"
                "  See sdks/direnv/README.md for the recommended setup.",
                service_name,
                host,
                port,
            )

    return sock, port


def get_free_port(
    host: str = "0.0.0.0",
    *,
    service_name: str = "app",
    log: bool = True,
) -> int:
    """Return an ephemeral port number (the bound socket is closed immediately).

    Use this when you need the port *number* before constructing your server
    and the framework accepts a port integer rather than a pre-bound socket.

    Note: there is a brief TOCTOU window between closing the socket and the
    server rebinding; ``find_free_port`` (which keeps the socket open) is
    safer when the framework supports it.
    """
    sock, port = find_free_port(host, service_name=service_name, log=log)
    sock.close()
    return port
