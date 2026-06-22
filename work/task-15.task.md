---
id: ee0488f7-2163-465d-805a-0f9b1d17959e
slug: task-15
status: todo
title: Linux scoped DNS resolver fallback for non-systemd-resolved hosts
milestones:
- milestone-1
depends_on:
- af741f08-3311-48c3-a459-8cde3f518a90
created_at: 2026-06-22T10:41:40.321030133Z
updated_at: 2026-06-22T10:41:40.321030133Z
---

## Problem

task-2's Linux scoped resolver (`net/resolver_config.rs`) configures `*.devenv.local`
resolution exclusively through `resolvectl` (systemd-resolved):

- `resolvectl dns <link> <ip>:<port>`
- `resolvectl domain <link> ~devenv.local`  (routing domain on the `lo` link)

This hard-requires an active `systemd-resolved`. On hosts that don't run it, the
calls fail and scoped DNS is never wired up. Observed 2026-06-22 running the
`real_tun_overlay` e2e test as root:

```
Failed to set DNS configuration: Unit dbus-org.freedesktop.network1.service not found.
Failed to revert interface configuration: Unit dbus-org.freedesktop.network1.service not found.
```

The overlay TUN + smoltcp byte-proxy come up fine (the test passes, because resolver
setup is best-effort/non-fatal), but `*.devenv.local` does not resolve, so
`curl http://db.devenv.local/` cannot reach the overlay on such a machine. The full
end-to-end flow only works on systemd-resolved hosts today.

## Goal

Make scoped `*.devenv.local` resolution work on Linux hosts WITHOUT systemd-resolved,
while preserving the existing guarantees: no whole-resolver hijack, reversible/clean
teardown, best-effort/non-fatal.

## Approach (finalize in design)

Detect the active resolver mechanism at install time and pick a strategy:

1. **systemd-resolved active** -> current `resolvectl` path (unchanged).
2. **dnsmasq / NetworkManager+dnsmasq** -> drop a scoped snippet
   (e.g. `/etc/NetworkManager/dnsmasq.d/devenv.conf` or `/etc/dnsmasq.d/devenv.conf`)
   with `server=/devenv.local/127.0.0.1#5300`, then reload the resolver.
3. **Plain `/etc/resolv.conf`, no local stub** -> fall back to per-name `/etc/hosts`
   entries (`<vip> <name>`). The overlay already assigns a stable VIP per name, so this
   is scoped + reversible; tradeoff is it bypasses the embedded DNS server for those
   names and must be kept in sync as services come/go.

Detection ideas: `systemctl is-active systemd-resolved`, presence of
`/run/systemd/resolve/`, `resolvectl status` success, or which resolver owns
`/etc/resolv.conf`.

## Acceptance Criteria

- [ ] Detects whether systemd-resolved is active; keeps using `resolvectl` when it is.
- [ ] At least one working fallback on a non-systemd-resolved host (dnsmasq snippet
      and/or `/etc/hosts` per-name mapping).
- [ ] `*.devenv.local` resolves to the overlay VIP on such a host, so the
      `real_tun_overlay` e2e (or a documented manual `curl`) succeeds end-to-end.
- [ ] Reversible/clean teardown for whichever mechanism is chosen; no leftover config
      after daemon shutdown.
- [ ] Only affects `devenv.local` (no whole-resolver hijack).
- [ ] Clear, actionable warning + guidance when no mechanism can be configured.
- [ ] Pure helpers (mechanism detection over injected inputs, config-snippet/hosts-line
      generation) unit-tested; no real system mutation in tests.

## Notes

Extends task-2 (`depends_on: task-2`). The `/etc/hosts` fallback also relates to how
overlay services are surfaced — keep entries in sync with the ServiceTable lifecycle.
