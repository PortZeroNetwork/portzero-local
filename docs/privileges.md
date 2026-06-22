# Platform privilege requirements

The local `.devenv.local` overlay manipulates kernel networking state, which
requires elevated privileges. This document explains exactly what needs them and
what happens when they are missing.

## What requires privileges

The overlay performs three privileged operations when it starts
(`OverlayNetwork::start`):

1. **Create the TUN device** (`net/tun_device.rs`). Opening `/dev/net/tun` and
   bringing up an interface needs root or `CAP_NET_ADMIN`.
2. **Install the route** for `10.254.0.0/16` into the TUN interface. Modifying
   the routing table needs root / `CAP_NET_ADMIN`. (This step is best-effort and
   non-fatal — a missing route is logged, not aborted.)
3. **Install the scoped resolver** (`net/resolver_config.rs`) so
   `*.devenv.local` queries go to the embedded DNS server. Writing OS resolver
   config (`/etc/resolver`, systemd-resolved, etc.) needs root.

### Per platform

| Platform | Requirement                                                                 |
|----------|-----------------------------------------------------------------------------|
| Linux    | root, or the daemon binary granted `CAP_NET_ADMIN` (e.g. via `setcap`).      |
| macOS    | root, or the `com.apple.developer.networking.networkextension` entitlement.  |
| Windows  | `wintun.dll` present next to the binary / in `PATH`; admin for adapter setup. |

The simplest path for local development is to start the daemon with `sudo`:

```bash
sudo -E devenv-tunnel start --foreground
```

`-E` preserves your environment.

## Graceful degradation without privileges

The daemon does **not** require root to run. Without sufficient privileges it
**degrades to cloud/local-only mode**: it logs a line such as

```
continuing in cloud/local-only mode
```

and keeps running. Concretely:

- TUN creation fails → the overlay does not carry traffic.
- Scoped resolver install fails → it logs a warning but startup continues; the
  embedded DNS server still runs, just isn't wired into the OS resolver.
- Route install fails → logged, non-fatal.

Cloud tunnels (`.tunnel.devenv.tools`) and process/container **discovery** still
work without root; only the local overlay data path needs it.

## Consequence: `.devenv.local` visibility

Because the overlay (TUN + scoped resolver) is what makes `.devenv.local` names
resolvable and routable, **`.devenv.local` services are only visible once the
overlay is running — i.e. when the daemon was started with root.** If you run the
daemon unprivileged, `curl http://hello.devenv.local/` will not resolve even
though the service was discovered. See [troubleshooting.md](troubleshooting.md).

## Testing implications

- `cargo test` / `just test` run fully unprivileged. The root-gated real-TUN
  test (`real_tun_overlay`) detects `geteuid() != 0` and **skips cleanly**.
- To exercise the real TUN path, run `just e2e`, which uses `sudo`.
