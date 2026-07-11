# FAQ

## Why doesn't Port Zero use real mDNS (Bonjour/Avahi `.local` names) instead of its own `portzero.local` domain?

It's tempting: mDNS already gives you `.local` names, and in principle a single
machine could bind a separate `.local` IP alias per service, all on the same
port, so `foo.local:80` and `bar.local:80` reach different processes without
port conflicts. We looked at it and decided against it, for a few reasons:

- **Address ownership.** Port Zero's virtual IPs live in a `10.254.0.0/16`
  range inside its own virtual NIC (see [architecture.md](architecture.md)) —
  a space Port Zero fully owns, so collisions are impossible. Real mDNS
  reachable from the LAN needs addresses aliased onto the physical NIC, out of
  whatever subnet that network happens to be using. That's a different subnet
  every time you change networks, and a real risk of colliding with another
  device's address or the DHCP pool.
- **Security boundary.** Today, a service opened with `PZ_TUNNEL` is reachable
  only from the machine running it. Real mDNS + LAN-facing aliases would make
  local dev services — often unauthenticated — reachable by anyone else on
  the same Wi-Fi or office network. That's a meaningful expansion of attack
  surface for a feature most people only need on one machine.
- **Reliability.** Port Zero's scoped-domain DNS approach works identically
  offline, on a VPN, or on networks that filter multicast — which is common
  on corporate Wi-Fi and especially on hotel/coffee-shop networks with client
  isolation. Real mDNS is unreliable in exactly those situations.
- **Protocol complexity.** Port Zero's embedded DNS server is authoritative
  for its own domain, so it never has to worry about record conflicts. Real
  mDNS requires implementing probing, announcing, and conflict tie-breaking
  against any other Bonjour/Avahi responders already on the network (a
  printer, another dev's Port Zero instance, etc.).

If you actually need a service to be reachable beyond the local machine —
from another device, a teammate, or the internet — that's what
[Cloud tunnels](portzero.md) are for, and they're simpler than making mDNS do
this job.

## Doesn't `.local` belong to mDNS? Does Port Zero break Bonjour/AirPrint/Avahi?

No. Port Zero claims only the `portzero.local` subtree, not `.local` itself,
and it does so with a *scoped* resolver entry — on macOS,
`/etc/resolver/portzero.local` sends queries for `portzero.local` and its
subdomains (and nothing else) to Port Zero's embedded DNS server. Every other
`.local` name — `printer.local`, `my-mac.local`, your teammate's AirDrop —
still goes to mDNSResponder/Avahi exactly as before. Port Zero never answers,
probes, or announces on multicast at all.

RFC 6762 does reserve `.local` for mDNS, so the honest description is: Port
Zero carves one name (`portzero.local`) out of that space on your machine
only. The only theoretical conflict is a real mDNS device on your network that
advertises the hostname `portzero` — in that case your machine resolves the
name to Port Zero's dashboard instead of that device. Nothing else on the
network is affected, because the override is local resolver configuration, not
network traffic.

Two platform quirks are worth knowing (both handled by `portzero setup`):

- **macOS** intercepts all `.local` lookups with mDNSResponder before other
  resolvers are consulted, which is why the scoped `/etc/resolver` entry (and
  an `/etc/hosts` pin for the bare dashboard name) is part of setup.
- **Linux** desktops commonly ship `mdns4_minimal [NOTFOUND=return]` in
  `nsswitch.conf`, which claims two-label `.local` names; see
  [privileges.md](privileges.md) for how the bare `portzero.local` dashboard
  name is handled there. Subdomain names like `myapp.portzero.local` have
  three labels and are not claimed by `mdns4_minimal`.

## How do I set up Port Zero inside a Claude Code (claude.ai/code) cloud/browser session?

It works, with one extra step beyond a normal machine: add a hosts entry by
hand instead of relying on automatic OS-level DNS, because these sandboxes
commonly don't run the services Linux resolver integration depends on. (Why,
exactly, is sandbox-specific and documented in
[troubleshooting.md](troubleshooting.md#portzerolocal-names-dont-resolve-in-a-cloud-sandbox-or-container)
— this answer is just the steps.)

1. **Install Port Zero** the same way you would anywhere else — see the
   [README](../README.md) for the current install command per platform.
2. **Start the daemon as root**, since the overlay needs `CAP_NET_ADMIN`:
   ```bash
   sudo -E portzero start --foreground
   ```
   (or run `sudo portzero setup` once, then `portzero start` — see
   [privileges.md](privileges.md)). Leave it running in the background for
   the rest of the session (e.g. launch it from a
   [`SessionStart` hook](https://code.claude.com/docs/en/claude-code-on-the-web)
   so it's already up before you start working).
3. **Tag your dev process or container** with `PZ_TUNNEL` as usual, e.g.
   `PZ_TUNNEL=myapp.portzero.local:80 npm start` — see
   [portzero.md](portzero.md).
4. **Check it actually worked:**
   ```bash
   portzero doctor
   ```
   If the hosts pin check warns, add the entry it suggests. `portzero doctor`
   and `portzero setup` both check first whether `/etc/hosts` is actually
   safe to edit in that specific sandbox, and tell you plainly if it isn't
   (rather than silently failing or writing something that won't persist).
5. **Don't assume `foo.portzero.local` resolves in scripts/tests.** Once the
   hosts pin from step 4 is in place it will, but for CI-style steps that
   can't add hosts entries, skip name resolution entirely: use `portzero url`
   / `portzero env` / `portzero wait` to get the concrete tunnel URL instead
   — the same pattern the [`tunnel-action`](../tunnel-action/README.md)
   GitHub Action uses.

## How do I expose a public `*.tunnel.portzero.cloud` URL from a remote Claude Code session?

This is [Cloud tunnels](portzero.md), not the local overlay above — and it's
actually simpler in a sandbox: **no root, no `sudo`, no TUN device, no hosts
pin.** The cloud connector runs fully unprivileged and only needs outbound
HTTPS; it's the local overlay that needs `CAP_NET_ADMIN`.

1. **Check the environment's network policy allows outbound HTTPS** to
   `app.portzero.cloud` (the API) and `edge.portzero.cloud` (the tunnel
   connection). This is configured per Claude Code environment; see
   [the docs](https://code.claude.com/docs/en/claude-code-on-the-web). If
   either is blocked, cloud tunnels can't work from that environment —
   `portzero doctor` will show the cloud connection as failed rather than
   hanging.
2. **Log in with `portzero login --interactive`, not plain `portzero
   login`.** The default flow opens a local browser and waits for it to post
   back to a `127.0.0.1` port on the same machine — in a remote session
   there's no local browser to open, and even a browser on your own laptop
   can't reach that loopback port on the sandbox. `--interactive` instead
   emails you a one-time code you type back at the prompt; no browser or
   local port involved. (See
   [troubleshooting.md](troubleshooting.md#portzero-login-hangs-or-times-out-in-a-remote-claude-code-session)
   if you're curious why the default flow can't work here.)
3. **Persist the login across sessions**, since each session is a fresh
   container: after logging in, save `~/.portzero/auth.json` (an
   email/token/account id, `chmod 600`) as a secret on the Claude Code
   environment, then write it back out at the start of every session, e.g.
   from a `SessionStart` hook:
   ```bash
   mkdir -p ~/.portzero
   printf '%s' "$PORTZERO_AUTH_JSON" > ~/.portzero/auth.json
   chmod 600 ~/.portzero/auth.json
   ```
   The token doesn't expire on its own; revoke it from the dashboard or
   `portzero logout` if it's ever compromised.
4. **Start the daemon** (no `sudo` needed for cloud-only use):
   ```bash
   portzero start
   ```
5. **Tag your process with a cloud domain** instead of `.portzero.local`:
   ```bash
   PZ_TUNNEL=myapp.{cloud-username}.tunnel.portzero.cloud:80 npm start
   ```
   (bind to port 0 as usual — see [portzero.md](portzero.md)).
6. **Get the public URL**:
   ```bash
   portzero url myapp.<cloud-username>.tunnel.portzero.cloud
   ```
   or `portzero wait <domain> --healthy` to block until it's actually
   serving. `portzero doctor` reports cloud connection state either way.
