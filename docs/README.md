# devenv-tunnel documentation

These documents describe how the local virtual overlay network works and how to
operate it.

- [Architecture overview](architecture.md) — the overlay data path: discovery →
  VIP allocation → scoped DNS → TUN → smoltcp user-space proxy.
- [`DEVENV_TUNNEL` semantics](devenv-tunnel.md) — the full-domain rule, how the
  suffix selects cloud vs. local, and the `{branch}` / `{worktree}` templates.
- [Platform privileges](privileges.md) — what needs root / `CAP_NET_ADMIN`, and
  what degrades gracefully without it.
- [Troubleshooting](troubleshooting.md) — common failure modes and fixes.

See also:

- [`../examples/local-overlay/`](../examples/local-overlay/) — a runnable
  `.devenv.local` end-to-end example.
- [`../examples/docker-templated-tunnel/`](../examples/docker-templated-tunnel/) —
  templated names + Docker discovery.
- [`../sdks/`](../sdks/) — thin language helpers (direnv, Node, Python, Go).
