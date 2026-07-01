# port-zero documentation

These documents describe how the local virtual overlay network works and how to
operate it.

- [Architecture overview](architecture.md) — the overlay data path: discovery →
  VIP allocation → scoped DNS → TUN → smoltcp user-space proxy.
- [`PZ_TUNNEL` semantics](portzero.md) — the full-domain rule, how the
  suffix selects cloud vs. local, and the `{branch}` / `{worktree}` templates.
- [Platform privileges](privileges.md) — what needs root / `CAP_NET_ADMIN`, and
  what degrades gracefully without it.
- [Troubleshooting](troubleshooting.md) — common failure modes and fixes.

- [Windows signing runbook](windows-signing.md) - Azure Artifact Signing setup,
  release signing order, and Defender false-positive follow-up.

See also:

- [`../examples/local-overlay/`](../examples/local-overlay/) — a runnable
  `.portzero.local` end-to-end example.
- [`../examples/docker-templated-tunnel/`](../examples/docker-templated-tunnel/) —
  templated names + Docker discovery.
- [`../sdks/`](../sdks/) — thin language helpers (direnv, Node, Python, Go).

## For contributors

See [troubleshooting.md](troubleshooting.md) for:

- How to run the test suite (`just test` vs `just e2e`)
- Setting up automatic local checks with git hooks (`just install-hooks`)
- Reproducing the CI checks locally to avoid burning GitHub Actions minutes

The project uses `just` as the task runner everywhere. Run `just --list` to see
available commands.
