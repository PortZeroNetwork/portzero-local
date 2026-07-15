# Security model

Port Zero runs a privileged daemon on your machine and, optionally, connects it
to a cloud edge. This page states what the daemon reads, what it sends, why it
needs root, and how the local certificate authority is constrained. Every claim
here is traceable to the source; file references are given so you can check.

## What the daemon reads

To map a running service to a `*.portzero.local` name, the daemon scans local
processes. For each process it reads two things.

### Process environment variables

The daemon looks for `PZ_TUNNEL` (and the optional companions
`PZ_TUNNEL_HTTP_PORT`, `PZ_TUNNEL_PORTS`, `PZ_TUNNEL_HEALTH_PATH`,
`PZ_TUNNEL_NO_PROBE`, plus `PWD` for the working directory). It reads the
process's environment block to find them:

- **Linux** — reads `/proc/<pid>/environ`, the kernel's frozen copy of the
  environment captured at `execve` time
  (`scan_process_env_linux`, `discovery/process.rs`).
- **macOS** — shells out to `ps -p <pid> -wwwE -o command=`, which reads the
  same frozen argv/environment block the kernel exposes
  (`scan_process_env_macos`, `discovery/process.rs`).
- **Windows** — reads the target process's PEB environment block via
  `ReadProcessMemory` (`read_windows_process_environment`).

The scan reads the whole environment block into memory transiently but keeps
**only** the value of the specific `PZ_TUNNEL*` variables it is looking for; the
rest is discarded, never stored, never logged, never transmitted. A process with
no `PZ_TUNNEL` variable is ignored.

### Listening TCP ports

To learn which port a discovered service listens on, the daemon reads the
process's listening TCP sockets:

- **Linux** — `/proc/<pid>/fd` socket inodes cross-referenced against
  `/proc/<pid>/net/tcp` and `/proc/<pid>/net/tcp6` (`discovery/process.rs`).
- **macOS** — `lsof` scoped to the pid.
- **Windows** — `Get-NetTCPConnection` / `netstat` scoped by pid.

Only the port numbers are used, to build the local reverse-proxy route. No
socket payload is read.

## What leaves the machine

### Logged out (no cloud account)

Nothing leaves the machine from the daemon. There is no analytics, no
telemetry, and no phone-home in the long-running daemon. The daemon links
`reqwest`, but every call site is local:

- `forwarder.rs` forwards tunnel requests to `http://127.0.0.1:<port>` — loopback
  only.
- `diagnostics.rs` probes the local `portzero.local` dashboard over the overlay —
  local only.
- `auth.rs` refreshes a JWT against the cloud API, but only when a token is
  already stored (see below); logged out, this never runs.

The cloud connection itself is gated on an auth token loaded from disk
(`discovery_loop.rs`: the connector is built only inside
`if let Some(ref token) = current_token`). With no token, no connector is
created and no bytes are sent to any Port Zero server.

One honest exception, and it is the CLI front-end, not the daemon: running the
`portzero` command spawns a once-daily version check
(`update::check_for_update`, `cli/src/update.rs`). It is a plain HTTP `GET` of a
public GitHub release asset
(`https://github.com/LoumTechnologies/port-zero/releases/latest/download/version.json`).
It sends no machine data beyond what any HTTP request reveals (your IP and a
User-Agent), runs at most once per 24 hours, and is disabled by setting
`PZ_TUNNEL_NO_UPDATE_CHECK`.

### Logged in with cloud tunnels

When you log in and expose a cloud tunnel, the daemon opens a WebSocket to
`wss://edge.portzero.cloud/tunnel` (`cloud.rs`) and transmits:

- **Authentication** — a `Hello` message with your JWT auth token, the client
  version, an `os` string, and a `machine_id` (your hostname plus a random
  8-character suffix; `generate_machine_id`, `cloud.rs`). The JWT is obtained at
  login and stored locally in `auth.json`; it is refreshed against
  `https://app.portzero.cloud/api/auth/refresh`.
- **Tunnel registration metadata** — for each exposed route, a `RegisterRoute`
  message with the tunnel domain, the local port, the protocol, and process
  metadata: working directory, executable path, process name, and full command
  line (`RouteMetadata`, `proto/src/lib.rs`). If you expose a cloud tunnel, this
  process metadata leaves your machine. Local-only tunnels never register with
  the edge.
- **The tunneled traffic itself** — requests arriving from the internet are
  relayed through the edge to your daemon, which forwards them to
  `127.0.0.1:<port>` and relays the response back. This is the point of a tunnel:
  the traffic transits Port Zero's edge.

## Why the daemon needs root

The overlay creates a TUN device, installs a route, binds a DNS server on a
privileged port, and writes OS resolver configuration — all of which require
elevated privileges. The exact operations, the per-platform requirements, and
what degrades when privileges are missing are documented in
[privileges.md](privileges.md).

## The local certificate authority

To serve `https://*.portzero.local` without browser warnings, Port Zero
generates a local CA and installs its certificate into the OS trust store (and
into Firefox/Chromium NSS databases). This is the part of the design that most
deserves scrutiny, because a CA in your trust store is a powerful thing.

### The CA private key is never written to disk

The CA private key is generated in memory, used once to self-sign the CA
certificate and to sign the `*.portzero.local` wildcard certificate, and then
dropped (`generate`, `tls/ca.rs`). It is never serialized to disk. The only
private key persisted is the **wildcard leaf key**, which can impersonate only
`*.portzero.local` names in the first place.

Persisted files live in the platform data directory
(`~/Library/Application Support/PortZero/` on macOS,
`~/.local/share/PortZero/` on Linux, `%APPDATA%\PortZero\` on Windows):

| File            | Contents                        | Permissions |
|-----------------|---------------------------------|-------------|
| `ca.crt`        | CA certificate (public)         | default     |
| `wildcard.crt`  | Wildcard certificate (public)   | default     |
| `wildcard.key`  | Wildcard **private** key        | `0600` on Unix (owner read/write only) |

### Name constraints

The CA certificate carries a critical X.509 Name Constraints extension
(RFC 5280 §4.2.1.10), so a conforming verifier will reject any certificate the
CA signs that steps outside its lane:

- **Permitted** DNS subtree `portzero.local`. Per RFC 5280 this matches the apex
  name and every subdomain (`portzero.local`, `app.portzero.local`,
  `a.b.portzero.local`), and nothing else. The CA cannot mint a trusted
  certificate for `example.com`.
- **Excluded** IP address space `0.0.0.0/0` and `::/0`. A DNS permitted subtree
  alone does not constrain a leaf that presents only an IP-address SAN, because
  RFC 5280 applies constraints per name-form. Excluding the entire IP space
  closes that bypass. Port Zero never issues IP certificates, so this costs
  nothing.

The extension is emitted as **critical** by the `rcgen` library
(`oid::NAME_CONSTRAINTS` is written with the critical flag set), which is what
RFC 5280 requires for name constraints. You can verify it yourself:

```
openssl x509 -in "$DATA_DIR/ca.crt" -noout -text | grep -A4 "Name Constraints"
```

Expect `X509v3 Name Constraints: critical`, a `Permitted` `DNS:portzero.local`,
and an `Excluded` `IP:0.0.0.0/0.0.0.0`.

### Honest caveats

- **Not every trust store enforces name constraints.** Modern browsers
  (Chrome/Chromium, Firefox, Safari) and the OpenSSL and macOS/Windows platform
  verifiers do enforce them. But enforcement lives in the verifier, not the
  certificate: a client that ignores the extension gets no protection from it.
  Name constraints are defense-in-depth, not a substitute for keeping the trust
  store honest.
- **Constraints bound DNS and IP identities only.** A verifier doing TLS
  server authentication matches a site against DNS and IP SANs, which is exactly
  what we constrain. Other SAN forms (email, URI) are not name-forms a browser
  uses for server identity, so they are not an HTTPS MITM vector.
- **The CA key would still matter if extracted at generation time.** Because the
  key exists in daemon memory only during generation, an attacker would need to
  read that memory in that window. If they did, the name constraints still limit
  what the stolen key can forge.

### Migration from earlier, unconstrained CAs

Port Zero originally shipped an unconstrained CA. If you installed one:

- `portzero trust generate` detects a CA that lacks name constraints,
  regenerates the whole bundle in place (written atomically via temp-file +
  rename), and tells you to re-run `sudo portzero trust install`. The old,
  unconstrained anchor stays trusted until you do.
- `portzero trust install` removes any previously installed "PortZero Local CA"
  anchor before adding the new one (on macOS via `security delete-certificate`
  looped over the system keychain; on Windows via `CertDeleteCertificateFromStore`
  by subject; on Linux the anchor is a fixed file path, so it is overwritten).
  You do not accumulate stale roots.
- The daemon never regenerates the CA on its own. If it loads an unconstrained
  CA it logs a one-line warning pointing at
  `portzero trust generate && sudo portzero trust install`, because silently
  swapping the CA would break trusted HTTPS until you reinstalled it.

## Reporting a problem

If you find a way for the daemon to send data it should not, or a way for the
constrained CA to sign something outside `*.portzero.local`, please report it.
The claims above are meant to be falsifiable against the source in this
repository.
