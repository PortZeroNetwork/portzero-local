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

## Autostart (running the daemon privileged at boot)

To have the overlay come up automatically, the daemon must autostart **with
privileges** — otherwise it would relaunch unprivileged and silently degrade to
cloud/local-only mode (see "Graceful degradation" below). Each platform uses the
native mechanism for a privileged service:

| Platform | Autostart mechanism                                                        |
|----------|----------------------------------------------------------------------------|
| macOS    | **Root LaunchDaemon** at `/Library/LaunchDaemons/tools.devenv.daemon.plist`. Runs as root, so utun + `/etc/resolver` + routes all succeed. |
| Linux    | systemd **user** unit (`~/.config/systemd/user/devenv-daemon.service`); the binary itself carries `CAP_NET_ADMIN`, so the user-level service is sufficient. |
| Windows  | Scheduled task at logon (admin for adapter setup). |

### macOS: why a LaunchDaemon (not a LaunchAgent)

macOS has no `setcap` equivalent, so the binary cannot be granted networking
capabilities the way it is on Linux. A user-level **LaunchAgent** runs as the
logged-in user and is unprivileged, so the overlay would never come up. The
autostart installer therefore writes a system-domain **LaunchDaemon**, which
launchd runs as **root**.

Installing or removing this system service is a **one-time privileged step** and
must be run with `sudo`. Logs are written to a root-writable location
(`/Library/Logs/devenv/daemon.log`), since root's home is not the installing
user's. The installer loads the daemon with the modern
`launchctl bootstrap system <plist>` and unloads it with
`launchctl bootout system/tools.devenv.daemon` (the deprecated `load -w` /
`unload -w` are kept only as a fallback). If you run the autostart install or
uninstall without root, it fails fast with a message telling you to re-run under
`sudo`.

The decision to use a root LaunchDaemon (rather than a Network Extension or a
privileged helper) is recorded in `work/task-29.task.md`.

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
