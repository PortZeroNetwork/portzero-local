---
id: ee0488f7-2163-465d-805a-0f9b1d17959e
slug: task-15
status: done
title: Make Linux scoped DNS robust (systemd-resolved without networkd, plus fallbacks)
milestones:
- milestone-1
depends_on:
- af741f08-3311-48c3-a459-8cde3f518a90
created_at: 2026-06-22T10:41:40.321030133Z
updated_at: 2026-06-22T10:41:40.321030133Z
---

## Problem

task-2's Linux scoped resolver (`net/resolver_config.rs`) configures `*.devenv.local`
resolution through `resolvectl`, attaching the DNS server + routing domain to the
**`lo` link**:

- `resolvectl dns lo <ip>:<port>`
- `resolvectl domain lo ~devenv.local`
- (teardown) `resolvectl revert lo`

**Corrected diagnosis (2026-06-22).** This is NOT only a "missing systemd-resolved"
problem. Running the `real_tun_overlay` e2e as root on a box where systemd-resolved
*is* active but systemd-**networkd** is NOT (NetworkManager-managed — a very common
setup) produced:

```
Failed to set DNS configuration: Unit dbus-org.freedesktop.network1.service not found.
Failed to revert interface configuration: Unit dbus-org.freedesktop.network1.service not found.
```

These are `resolvectl`'s OWN messages (our `run_resolvectl` calls `.status()`, so
resolvectl's stderr prints raw). `network1` is systemd-networkd. So the current
`resolvectl … lo` approach fails whenever networkd isn't managing the link, even with
systemd-resolved fully active. The overlay TUN + smoltcp byte-proxy come up fine (the
test still passes — resolver setup is best-effort/non-fatal), but `*.devenv.local`
does not resolve, so `curl http://db.devenv.local/` cannot reach the overlay.

There are therefore (at least) THREE Linux environments to handle:
- (A) systemd-resolved **with** networkd — current path works.
- (B) systemd-resolved **without** networkd (NetworkManager) — current path FAILS. ← this machine.
- (C) no systemd-resolved at all — current path FAILS.

## Goal

Make scoped `*.devenv.local` resolution work across the common Linux resolver
environments (A/B/C above) — especially systemd-resolved-without-networkd, which is
where this was found — while preserving the existing guarantees: no whole-resolver
hijack, reversible/clean teardown, best-effort/non-fatal.

## Approach (finalize in design; sub-agent should root-cause on this box first)

Detect the active resolver mechanism at install time and pick a strategy:

1. **systemd-resolved active** (A and B): attach the scoped DNS + `~devenv.local`
   routing domain to the overlay's **own TUN link (`deven0`)** instead of `lo`. The TUN
   interface is created by the daemon and is a real link resolved can manage directly
   via its D-Bus `SetLinkDNS`/`SetLinkDomains` API, which should not require networkd.
   (Primary hypothesis for fixing environment B — must be verified by re-running the
   root `real_tun_overlay` e2e on a NetworkManager box.) Consider calling resolved's
   D-Bus API directly, or `resolvectl dns deven0 …`, and verify the networkd error is
   gone. Note ordering: the resolver must be configured AFTER the TUN link exists.
2. **dnsmasq / NetworkManager+dnsmasq** (fallback): drop a scoped snippet
   (e.g. `/etc/NetworkManager/dnsmasq.d/devenv.conf` or `/etc/dnsmasq.d/devenv.conf`)
   with `server=/devenv.local/127.0.0.1#5300`, then reload the resolver.
3. **No local stub at all** (C): fall back to per-name `/etc/hosts` entries
   (`<vip> <name>`). The overlay assigns a stable VIP per name, so this is scoped +
   reversible; tradeoff is it bypasses the embedded DNS server for those names and must
   be kept in sync as services come/go (would require feeding ServiceTable updates into
   the resolver layer — larger change; flag if pursued).

Detection ideas: `systemctl is-active systemd-resolved`, presence of
`/run/systemd/resolve/`, `systemctl is-active systemd-networkd`, `resolvectl status`,
or which resolver owns `/etc/resolv.conf`.

## Acceptance Criteria

- [ ] **Environment B (this machine): systemd-resolved active, networkd absent** — scoped
      DNS configures WITHOUT the `network1` error, by attaching to the TUN link
      (`deven0`) or another networkd-independent method. Verified by `real_tun_overlay`
      passing with NO `Unit dbus-org.freedesktop.network1.service not found` output and
      `*.devenv.local` actually resolving to the overlay VIP.
- [ ] Detects the resolver environment; keeps the working `resolvectl` path for (A).
- [ ] At least one working fallback for environment C (no systemd-resolved): dnsmasq
      snippet and/or `/etc/hosts` per-name mapping.
- [ ] `*.devenv.local` resolves to the overlay VIP, so the `real_tun_overlay` e2e
      (or a documented manual `curl`) succeeds end-to-end on a box like this one.
- [ ] Reversible/clean teardown for whichever mechanism is chosen; no leftover config
      after daemon shutdown.
- [ ] Only affects `devenv.local` (no whole-resolver hijack).
- [ ] Clear, actionable warning + guidance when no mechanism can be configured.
- [ ] Pure helpers (mechanism detection over injected inputs, config-snippet/hosts-line
      generation) unit-tested; no real system mutation in tests.

## Notes

Extends task-2 (`depends_on: task-2`). The `/etc/hosts` fallback also relates to how
overlay services are surfaced — keep entries in sync with the ServiceTable lifecycle.
