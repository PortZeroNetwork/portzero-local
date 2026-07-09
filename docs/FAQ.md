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
