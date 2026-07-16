# Port Zero Local — user documentation

Audience: [target-audience.md](target-audience.md).

## Problem

Developers have all seen port conflict errors like this one:

```
Error: listen EADDRINUSE: address already in use :::3000
```

Port Zero Local solves this problem for traffic on a single computer. It is free & open source.

You use Port Zero Local with your programs the same, whether they are a process or a Docker container. Configure all ports to 0 for any program you want Port Zero to manage; this tells the operating system to pick an available port at random. Then you start your programs with the `PZ_TUNNEL` environment variable.

- If you specify `PZ_TUNNEL={branch}.mytodoapp.portzero.local:80`, that is a Local tunnel.

With your program running, open `http://master.mytodoapp.portzero.local:80` in your browser. Different worktrees can use different branch labels at the same time without port conflicts. This works for any TCP protocol, not only HTTP.

Cloud tunnels (`*.tunnel.portzero.cloud`) use the same `PZ_TUNNEL` mechanism (login + subscription).

## Getting started

1. Install Port Zero (see the [repo README](../../README.md#install)).
2. Start the daemon:

   ```bash
   portzero start
   ```

   This opens the **PortZero app** — a desktop app showing daemon and tunnel
   health, the HTTPS toggle, daemon controls, and the bundled examples. You
   can reopen it anytime from the system tray icon's **Open PortZero** item.
   Run `portzero start --no-browser` to start the daemon without opening it.
3. Tag a process or Docker container with `PZ_TUNNEL` as shown above, then
   check it in the app's tunnel list (or run `portzero status`).

The daemon still serves a browser dashboard at `http://portzero.local` for
debugging, but it's no longer linked from the app, the tray, or the CLI — the
desktop app is the recommended way to use Port Zero day to day.

## Index

### Core concepts

- [PZ_TUNNEL semantics](portzero.md)
- [Examples](examples.md)
- [Dev-to-production flow](dev-to-production.md)
- [MCP server & `portzero inspect`](mcp.md)
- [Review records](review-records.md)

### Patterns

- [Running portzero in CI](../../tunnel-action/README.md)
- [Review apps](review-apps.md)

### How it works

- [Architecture](architecture.md)

### Setup and behavior

- [Platform privileges](privileges.md)

### Cloud

- [Cloud tunnels: local UI vs dashboard](cloud-tunnels-local-vs-dashboard.md)

### Troubleshooting

- [Troubleshooting](troubleshooting.md)
- [Known limitations](known-limitations.md)
- [FAQ](FAQ.md)
- [Security](security.md)

## For contributors

See [../developers/](../developers/).
