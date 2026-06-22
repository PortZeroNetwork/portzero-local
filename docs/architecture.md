# Architecture: the virtual overlay data path

The local overlay implements **"Port 0 + Virtual Mesh"**: a service binds an
ephemeral port and is reachable by a stable name + virtual IP, with no manual
port wiring. This document traces the full data path.

## Components

All overlay code lives in `client/crates/daemon/src/net/`:

| Module               | Responsibility                                                       |
|----------------------|---------------------------------------------------------------------|
| `virtual_ip.rs`      | Allocates stable virtual IPs from `10.254.0.0/16` per name.          |
| `service_table.rs`   | Maps `name` ⇄ `VIP` ⇄ real ephemeral `SocketAddr` + service port.    |
| `dns.rs`             | Embedded authoritative DNS server for `*.devenv.local` → VIP.        |
| `resolver_config.rs` | Installs a *scoped* OS resolver so only `.devenv.local` is sent to it. |
| `tun_device.rs`      | Creates the TUN interface and routes `10.254.0.0/16` into it.         |
| `stack.rs`           | User-space TCP stack (smoltcp) that proxies VIP:port → real backend.  |
| `overlay.rs`         | `OverlayNetwork`: orchestrates TUN + stack + DNS + resolver.          |

## The data path, step by step

```
service (port 0)        daemon                              client (curl)
      |                   |                                     |
  [1] |  set DEVENV_TUNNEL=hello.devenv.local before exec       |
      |                   |                                     |
  [2] |<--- discovery reads /proc/<pid>/environ ----------------|
      |     suffix .devenv.local => local overlay               |
      |     real ephemeral port found                           |
      |                   |                                     |
  [3] |     ServiceTable.register("hello", real_addr, port)     |
      |     VirtualIpAllocator.assign("hello") => 10.254.0.N    |
      |                   |                                     |
  [4] |     DNS server now answers hello.devenv.local => VIP    |
      |     scoped resolver routes *.devenv.local to it         |
      |                   |                                     |
  [5] |                   |   curl http://hello.devenv.local/   |
      |                   |<--- resolves to 10.254.0.N ---------|
      |                   |<--- TCP SYN to 10.254.0.N:80 -------|
      |                   |     (kernel routes /16 into TUN)    |
      |                   |                                     |
  [6] |     smoltcp accepts the SYN in user space               |
      |     looks up VIP => real_addr in ServiceTable           |
  [7] |<--- tokio TcpStream to real ephemeral port -------------|
      |     bytes proxied bidirectionally                       |
```

### 1–2. Discovery

A service sets `DEVENV_TUNNEL` to a **full domain** and binds **port 0**. The
long-running daemon (`devenv-tunnel start [--foreground]`) reads each process's
environment from the outside: `/proc/<pid>/environ` on Linux,
`sysctl KERN_PROCARGS2` on macOS. Both are **frozen at `execve()` time**, which
is why the variable must be set before launch. The `.devenv.local` suffix routes
the service to the overlay; the daemon discovers the real ephemeral host port.

### 3. VIP allocation

`VirtualIpAllocator` (`net/virtual_ip.rs`) hands each name a stable IP inside
`10.254.0.0/16` (`.0` and `.1` reserved; `.1` is the gateway). The same name
always gets the same VIP for the daemon's lifetime, so DNS answers are stable
across service restarts. The `ServiceTable` keys both `name → service` and
`vip → name` for the fast packet-path reverse lookup.

### 4. Scoped DNS

`OverlayDnsServer` (`net/dns.rs`) is a small authoritative UDP DNS server that
answers **A records only**, for `<name>.devenv.local → VIP`, and returns
`NXDOMAIN` for unknown names inside its own zone. It defaults to listening on
`127.0.0.1:5300`. `resolver_config.rs` then installs a **scoped** OS resolver so
that only `*.devenv.local` queries are directed at it — the system resolver is
never hijacked.

### 5–7. TUN + smoltcp proxy

`TunDevice` (`net/tun_device.rs`) creates an L3 TUN interface, assigns the
gateway address (`10.254.0.1/16`), and ensures `10.254.0.0/16` routes into it.
When a client connects to a VIP, the kernel routes those packets into the TUN.

`VirtualStack` (`net/stack.rs`) runs a single dedicated task that owns the
smoltcp `Interface` + `SocketSet` and an `AnyIP` wildcard listener per service
port — so a SYN to **any** VIP on a registered port completes the TCP handshake
**entirely in user space** (no OS sockets on the client side). Once established,
the stack looks up which VIP the client targeted, opens a real
`tokio::net::TcpStream` to the discovered ephemeral backend, and proxies bytes
both ways via per-connection mpsc channels. smoltcp is synchronous and its
sockets aren't `Send` across `await`, so all of this lives in one task; backend
I/O runs in small async helper tasks so the loop never blocks.

`OverlayNetwork::start` (`net/overlay.rs`) wires these together and accepts
service-table updates from discovery via `update_services`.

## Testing the data path

- **Unprivileged** (`client/crates/daemon/tests/overlay_e2e.rs`,
  `unprivileged_overlay_round_trip`): asserts the wiring — VIP allocation,
  name → VIP via the real embedded DNS server, and a reachable real backend —
  with no root and no real TUN.
- **smoltcp byte proxy** (`net::stack::tests::vip_connect_proxies_to_backend`):
  drives a smoltcp client through an in-memory device pair against the real stack
  engine and a real tokio echo backend.
- **Root-gated** (`real_tun_overlay`, run via `just e2e`): stands up the real
  `OverlayNetwork` (TUN + stack + DNS) under `sudo` and resolves a service.
