# Local `.devenv.local` overlay — end-to-end example

> **This example demonstrates the local virtual overlay path** of
> `DEVENV_TUNNEL` — the `.devenv.local` suffix.
>
> A tiny service binds to **port 0**, the daemon discovers it, assigns a
> virtual IP from `10.254.0.0/16`, serves scoped DNS for
> `<name>.devenv.local`, and proxies traffic to the real ephemeral port
> through a TUN device + user-space TCP stack.
>
> By appending a **canonical port** to the value
> (`DEVENV_TUNNEL=hello.devenv.local:8080`), the overlay exposes the service on
> a clean, stable `VIP:8080` instead of the random ephemeral port. Because each
> service gets its own VIP, identical ports never collide
> (`db.devenv.local:5432` and `cache.devenv.local:6379` coexist fine). The end
> result:
>
> ```bash
> curl http://hello.devenv.local:8080/
> ```
>
> works even though the service is on a random localhost port.

For the cloud-tunnel counterpart (`.tunnel.devenv.tools`) and templated names,
see [`../docker-templated-tunnel/`](../docker-templated-tunnel/). For the
language helpers referenced below, see [`../../sdks/`](../../sdks/).

## How the overlay path works (one paragraph)

You set `DEVENV_TUNNEL=hello.devenv.local:8080` **before** launching your service
and bind it to port 0. The long-running daemon scans process environments
(`/proc/<pid>/environ` on Linux, `sysctl KERN_PROCARGS2` on macOS — the env is
frozen at `execve()` time, so it must be set before launch), splits off the
canonical `:8080`, sees the `.devenv.local` suffix, finds the real ephemeral
port, assigns a stable virtual IP, answers DNS for `hello.devenv.local`, and
proxies `VIP:8080` → real ephemeral backend. (Omit the `:8080` and the overlay
falls back to exposing the discovered ephemeral port.) See
[`../../docs/architecture.md`](../../docs/architecture.md) for the full data
path.

## Privilege requirement (important)

Creating the TUN device, installing the `10.254.0.0/16` route, and configuring
the scoped resolver all require **root / `CAP_NET_ADMIN`**. So the daemon must
be run with `sudo` for the `.devenv.local` overlay to actually carry traffic.

Without root the daemon does **not** crash — it logs
`continuing in cloud/local-only mode` and the overlay is simply inactive, so
`curl http://hello.devenv.local/` will not resolve. `.devenv.local` services are
only visible once the overlay is running (i.e. with root). See
[`../../docs/privileges.md`](../../docs/privileges.md).

## Prerequisites

- This repository checked out.
- Python 3 (the example service is stdlib-only — no `pip install`).
- The CLI built or installed: `cargo install --path client/crates/cli`
  (gives you `devenv-tunnel`). Or run it straight from source with `cargo run`.
- Optionally [direnv](https://direnv.net/) for the bundled `.envrc`.

## Walkthrough

### 1. Set `DEVENV_TUNNEL` (before launching the service)

Pick **one**:

```bash
cd examples/local-overlay

# Option A: direnv (recommended) — uses the bundled .envrc
direnv allow

# Option B: plain shell export
export DEVENV_TUNNEL=hello.devenv.local:8080

# Option C: the devenv-tunnel-exec launcher from the SDKs
#   ../../sdks/direnv/devenv-tunnel-exec hello.devenv.local:8080 python3 server.py
```

The `.devenv.local` suffix is what selects the local overlay. Nothing is
appended implicitly — the value must be a full domain. The trailing `:8080` is
the canonical port the overlay exposes the service on; it is optional (without
it the overlay uses the discovered ephemeral port).

### 2. Start the example service (binds port 0)

```bash
python3 server.py
# [server] bound to 127.0.0.1:54321 (ephemeral)
# [server] DEVENV_TUNNEL=hello.devenv.local:8080
# [server]   curl http://hello.devenv.local:8080/
```

Leave it running. It binds an ephemeral port — you do not pick the number; the
daemon discovers it.

### 3. Start the daemon WITH ROOT (in another terminal)

```bash
# From the repo root. Root is required to create the TUN + routes + resolver.
sudo -E devenv-tunnel start --foreground
# or straight from source:
sudo -E cargo run -p devenv-tunnel-cli --bin devenv-tunnel -- start --foreground
```

`-E` preserves your environment so the daemon can see the same context. The
daemon logs the TUN device it created and the VIP it assigned to
`hello.devenv.local`.

### 4. Reach the service by name

```bash
curl http://hello.devenv.local:8080/
# Hello from the devenv-tunnel local overlay!
# DEVENV_TUNNEL=hello.devenv.local:8080
# served on real port 54321
```

The name resolved to a `10.254.x.y` VIP, and the user-space stack accepted the
connection on the canonical `VIP:8080` and proxied your request to the real
ephemeral port (54321 here).

### 5. Inspect and clean up

```bash
devenv-tunnel status    # shows the discovered .devenv.local service + VIP
# Stop the daemon (Ctrl-C if foreground, or `devenv-tunnel stop`)
# Ctrl-C the python server
```

## Automated verification (`verify.sh`)

[`verify.sh`](verify.sh) runs the whole walkthrough above as a single scripted
check — the "fuller check" for the overlay. It starts the service, starts the
daemon, asserts resolution + an end-to-end request, and tears everything down,
printing PASS/FAIL per step.

```bash
# 1. build the CLI once, as your normal user (root usually has no cargo on PATH):
cargo build -p devenv-tunnel-cli
# 2. run the check as root (it creates a TUN + configures scoped DNS):
sudo ./examples/local-overlay/verify.sh
# optional custom name + canonical port:
#   sudo ./examples/local-overlay/verify.sh db.devenv.local:5432
```

What it asserts:

1. **`resolvectl query <name>` returns a `10.254.x.x` VIP** — the scoped resolver
   actually routes `*.devenv.local` to the embedded DNS server. On Linux this is
   the exact behavior fixed by **task-15** (it works on systemd-resolved hosts
   even when `systemd-networkd` is absent / NetworkManager-managed); the script
   explicitly **fails** if it sees the old
   `Unit dbus-org.freedesktop.network1.service not found` error.
2. **`curl http://<name>:<canonical-port>/` succeeds through the overlay** — DNS
   → VIP → TUN → smoltcp → real ephemeral backend. The canonical port is the
   `:<port>` declared in `DEVENV_TUNNEL` (a clean, stable number), not the random
   ephemeral port the service actually bound.
3. **Teardown is clean** — after the daemon stops, the name no longer resolves to
   a VIP (no leftover scoped DNS config).

Exit status is `0` only if every check passes. This is the manual counterpart to
the root-gated `real_tun_overlay` test (`just e2e`); use whichever is convenient.

## Using the SDK helpers instead of raw `server.py`

The [`sdks/`](../../sdks/) directory has thin, stdlib-only helpers that bind
port 0 and log the configured `DEVENV_TUNNEL`. For example, in Python:

```python
from devenv_tunnel import find_free_port  # sdks/python/devenv_tunnel.py
sock, port = find_free_port(service_name="hello")
```

They are convenience wrappers only — they cannot set `DEVENV_TUNNEL` for daemon
discovery (it must be set before launch). See
[`../../sdks/README.md`](../../sdks/README.md).

## As a system test

Running steps 1–4 is a manual system test of the overlay data path. After it you
have confirmed: a full `.devenv.local` domain was set before launch, the daemon
discovered the port-0 service, a VIP was assigned, scoped DNS resolved the name,
and the user-space stack proxied real bytes to the ephemeral backend.

The unprivileged half of this path is also covered automatically by
`cargo test` (`client/crates/daemon/tests/overlay_e2e.rs`); the root-gated
real-TUN half runs via `just e2e` (uses `sudo`).

## Troubleshooting

- `curl: (6) Could not resolve host` — the overlay is not running. The daemon
  must be started **with root** for `.devenv.local` names to resolve. Without
  root it runs in cloud/local-only mode and the overlay is inactive.
- Service not discovered — make sure `DEVENV_TUNNEL` was set **before** you
  started `server.py` (re-check with `echo $DEVENV_TUNNEL`). A value set after
  the process started is invisible to the daemon.
- See [`../../docs/troubleshooting.md`](../../docs/troubleshooting.md) for more.
