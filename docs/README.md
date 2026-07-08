# Port Zero Local Documentation

Developers have all seen port conflict errors like this one:

```
Error: listen EADDRINUSE: address already in use :::3000
```

Port Zero Local solves this problem for traffic on a single computer. It is free & open source.

You use Port Zero Local with your programs the same, whether they are a process or a Docker container. Configure all ports to 0 for any program you want Port Zero to manage; this tells the operating system to pick an available port at random. Then you start your programs with the `PZ_TUNNEL` environment variable.

- If you specify `PZ_TUNNEL={branch}.mytodoapp.portzero.local:80`, that is a Local tunnel.

The `PZ_TUNNEL` setting tells PortZero the domain name and port that clients should use.

With your program running, you can open `http://master.mytodoapp.portzero.local:80` in your browser. You might also be running a different version of your program in a separate git worktree. Port Zero supports this; `http://some-other-branch.mytodoapp.portzero.local:80` can be available at the same time without port conflicts. This doesn't just work for http; it works for *any* TCP protocol.

## How does this work?

Port Zero runs a background process on your local dev machine that scans for processes and Docker containers with the special `PZ_TUNNEL` environment variable. If the `PZ_TUNNEL` contains `portzero.local`, Port Zero opens a Local tunnel and does four things:

1. Create a virtual network interface card (NIC) on your local machine if Port Zero has not already done so
2. Create a virtual IP address in this virtual NIC for that process
3. Create a virtual DNS record for that virtual IP address, based on the template specified in `PZ_TUNNEL`
4. Forward the port specified in `PZ_TUNNEL` on the virtual IP address to the randomly-assigned port on the actual process or Docker container

Cloud tunnels (using `*.tunnel.portzero.cloud`) are also supported via the same `PZ_TUNNEL` mechanism (requires login and a subscription).

## Documentation

All documents in this directory are written for **developers using Port Zero Local**.

### Core concepts

- [PZ_TUNNEL semantics](portzero.md) — the full-domain rule, how the suffix selects cloud vs. local, `{branch}` / `{worktree}` templates, and when the variable must be set.
- [Examples](examples.md) — checked-in instructions generated from the latest adjacent `portzero-examples` checkout.

### Patterns

- [Review apps (per-PR preview environments)](review-apps.md) — an orchestrator-neutral
  pattern for per-PR preview URLs using only tunnel naming, with a plain `docker compose`
  + GitHub Actions example. Documents that a deploy agent is explicitly out of scope for
  the product.

### How it works

- [Architecture overview](architecture.md) — the overlay data path: discovery → VIP allocation → scoped DNS → TUN → smoltcp user-space proxy.

### Setup and behavior

- [Platform privileges](privileges.md) — what needs root / `CAP_NET_ADMIN`, what degrades gracefully without it, and autostart behavior.

### Using with the cloud

- [Cloud tunnels: local UI vs dashboard](cloud-tunnels-local-vs-dashboard.md) — why counts and status differ between http://portzero.local and https://app.portzero.cloud.

### Troubleshooting

- [Troubleshooting](troubleshooting.md) — common failure modes and fixes for day-to-day usage.
- [Known limitations: local CA trust](known-limitations.md) — specific browser/engine CA-trust
  gaps with an identified cause and documented workaround (Snap browsers, Playwright's bundled
  Firefox).
- [FAQ](FAQ.md) — answers to recurring questions.

## For contributors

Documentation intended for people contributing to (hacking on, releasing, maintaining) portzero-local lives under [dev/](dev/):

- [dev/README.md](dev/README.md) — entry point for contributors
- [Development](dev/development.md) — running tests (`just test` / `just e2e`), local CI checks, lefthook git hooks, Ticketry, and other just recipes
- [Software delivery lifecycle](dev/sdlc.md) — branch flow, release validation, and manual GitHub release publishing
- [Windows signing runbook](dev/windows-signing.md) — Azure Artifact Signing, release signing order, and Defender false positives

See also:

- [`../installer/README.md`](../installer/README.md) — the checked-in manifest that feeds the "Getting Started" section in the local UI.
- The `justfile` at the repository root (run `just --list` from anywhere).
