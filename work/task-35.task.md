---
id: a7a05fac-4706-41d3-9680-c53af65fd97e
slug: task-35
status: todo
title: Legacy-port monitor floods warnings for system services on macOS (e.g. sshd:22)
milestones:
- milestone-2
created_at: 2026-06-23T19:08:14.581933Z
updated_at: 2026-06-23T19:08:14.581933Z
---

## Context

Observed during the [[[task-22](../work/task-22.task.md)]] macOS runs: the legacy-port monitor floods the
daemon log and `devenv tunnel status` with warnings about **system services**:

```
WARN ... Port 22 is served directly (not via devenv-tunnel) by pid 606 (unknown
dir) — Set DEVENV_TUNNEL on this process ... Until then this service bypasses
the tunnel.
```

Port 22 is `sshd` — a system daemon the user will never tunnel. The legacy
monitor (`legacy_monitor.rs` + `enumerate_system_listeners` in `discovery.rs`)
enumerates ALL listeners system-wide and flags any on a "managed/common" port.
On macOS this is far noisier than on Linux because the daemon runs as **root**
(per [[[task-23](../work/task-23.task.md)]]) and therefore sees every process's listeners — including
system services (`sshd`, `mDNSResponder`, Parallels, ollama, JetBrains, etc.).

This is cosmetic (does not affect the overlay) but it's a real UX problem:
actionable warnings are drowned out.

UPDATE: the primary cause is the lsof `-a` bug ([[[task-36](../work/task-36.task.md)]]) — without `-a`,
`enumerate_system_listeners` attributes the WHOLE system's listeners to every
pid, so every process looked like it served 22/8080/etc. The [[[task-36](../work/task-36.task.md)]] fix
should eliminate most of this flood; re-verify whether any genuine scoping work
remains here afterward (the heuristic may still want a system-service denylist).

## Approach

- Scope the legacy-port heuristic so it doesn't flag system/well-known services.
  Options (pick after a quick look at the current heuristic):
  - Only flag listeners whose owning process is in the user's project context
    (has a discoverable cwd under a dev dir, or runs as the invoking user — not
    root/system uids), mirroring how the monitor already uses `cwd` /
    `has_devenv_tunnel`.
  - Exclude a denylist of well-known system ports (22, 53, 5353, 631, …) and/or
    well-known system process names.
  - Only warn for ports that actually collide with a discovered `.devenv.local`
    service, not every direct listener.
- Make sure the change is cross-platform (don't just suppress on macOS) and keep
  the genuinely-useful "your dev server bypasses the tunnel" case.

Done when:

- [ ] System services (sshd:22, etc.) no longer generate legacy warnings
- [ ] A real user dev server bypassing the tunnel is still flagged
- [ ] `status` + daemon log are quiet on a normal macOS box
- [ ] Cross-platform (Linux behaviour still sensible)