# Platform privilege requirements

The local `.portzero.local` overlay manipulates kernel networking state, which
requires elevated privileges. This document explains exactly what needs them and
what happens when they are missing.

## What requires privileges

The overlay performs four privileged operations when it starts
(`OverlayNetwork::start`):

1. **Create the TUN device** (`net/tun_device.rs`). Opening `/dev/net/tun` and
   bringing up an interface needs root or `CAP_NET_ADMIN`.
2. **Install the route** for `10.254.0.0/16` into the TUN interface. Modifying
   the routing table needs root / `CAP_NET_ADMIN`. (This step is best-effort and
   non-fatal — a missing route is logged, not aborted.)
3. **Bind the embedded DNS server** (`net/dns.rs`) to the TUN gateway on the
   privileged port `10.254.0.1:53`. Binding a port below 1024 needs root or
   `CAP_NET_BIND_SERVICE`. Without it the DNS server exits with `Permission
   denied` and **no `*.portzero.local` name resolves**, even though the rest of
   the overlay is up.
4. **Install the scoped resolver** (`net/resolver_config.rs`) so
   `*.portzero.local` queries go to the embedded DNS server. Writing OS resolver
   config (`/etc/resolver`, systemd-resolved, etc.) needs root.

### Per platform

| Platform | Requirement                                                                 |
|----------|-----------------------------------------------------------------------------|
| Linux    | root, or the daemon binary granted **both** `CAP_NET_ADMIN` and `CAP_NET_BIND_SERVICE` (e.g. `sudo setcap 'cap_net_admin,cap_net_bind_service+eip' $(which portzero)`). |
| macOS    | root, or the `com.apple.developer.networking.networkextension` entitlement.  |
| Windows  | `wintun.dll` present next to the binary / in `PATH`; admin for adapter setup. |

### Linux: the bare `portzero.local` dashboard name

The overlay DNS server answers `portzero.local` (the management dashboard, a
fixed VIP at `10.254.0.2`), but on most desktop Linux `/etc/nsswitch.conf` ships
an `mdns4_minimal [NOTFOUND=return]` entry that claims **2-label** `*.local`
names and halts the lookup before systemd-resolved is consulted — so the bare
dashboard name would never resolve through the overlay DNS. `just install`
therefore writes a static `10.254.0.2 portzero.local` line (tagged
`# portzero-local`) to `/etc/hosts`, which `files` resolves ahead of mdns.
Multi-label Local tunnel names (e.g. `app.portzero.local`) get `UNAVAIL` from mdns,
fall through to systemd-resolved, and resolve via the overlay DNS as normal.

The simplest path for local development is to start the daemon with `sudo`:

```bash
sudo -E port-zero start --foreground
```

`-E` preserves your environment.

## Autostart (running the daemon privileged at boot)

To have the overlay come up automatically, the daemon must autostart **with
privileges** — otherwise it would relaunch unprivileged and silently degrade to
cloud/local-only mode (see "Graceful degradation" below). Each platform uses the
native mechanism for a privileged daemon:

| Platform | Autostart mechanism                                                        |
|----------|----------------------------------------------------------------------------|
| macOS    | **Root LaunchDaemon** at `/Library/LaunchDaemons/tools.devenv.daemon.plist`. Runs as root, so utun + `/etc/resolver` + routes all succeed. |
| Linux    | systemd **user** unit (`~/.config/systemd/user/portzero-daemon.service`); the binary itself carries `CAP_NET_ADMIN` + `CAP_NET_BIND_SERVICE`, so the user-level unit is sufficient. |
| Windows  | Scheduled task at logon (admin for adapter setup). |

### Enabling / disabling autostart

Manage autostart with the `autostart` subcommands:

```bash
portzero autostart enable    # install the system unit
portzero autostart disable   # remove it
portzero autostart status    # show whether it's installed
```

On macOS, `enable`/`disable` write to `/Library/LaunchDaemons` and must be run
with `sudo` (see below); the command fails fast with that hint when run without
root:

```bash
sudo port-zero autostart enable
```

### macOS: why a LaunchDaemon (not a LaunchAgent)

macOS has no `setcap` equivalent, so the binary cannot be granted networking
capabilities the way it is on Linux. A user-level **LaunchAgent** runs as the
logged-in user and is unprivileged, so the overlay would never come up. The
autostart installer therefore writes a system-domain **LaunchDaemon**, which
launchd runs as **root**.

Installing or removing this system unit is a **one-time privileged step** and
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

Cloud tunnels (`*.<username>.tunnel.portzero.cloud`) and process/container **discovery** still
work without root; only the local overlay data path needs it.

## Consequence: `.portzero.local` visibility

Because the overlay (TUN + scoped resolver) is what makes `.portzero.local` names
resolvable and routable, **`.portzero.local` tunnels are only visible once the
overlay is running — i.e. when the daemon was started with root.** If you run the
daemon unprivileged, `curl http://hello.portzero.local/` will not resolve even
though the tunnel was discovered. See [troubleshooting.md](troubleshooting.md).

## Testing implications

- `cargo test` / `just test` run fully unprivileged and are side-effect safe on
  Linux, macOS, and Windows. The privileged real-TUN/Wintun test
  (`real_tun_overlay`) skips unless `PORTZERO_REQUIRE_REAL_TUN_E2E=1` is set.
- To exercise the real adapter path, run `just e2e`. It sets the opt-in
  variable and performs the current OS's privileged setup (`sudo` on Unix,
  Administrator + Wintun on Windows).
