---
id: 2a8cf42b-859e-449a-802b-6f8ffa24caf0
slug: task-22
status: todo
title: 'macOS bring-up: first privileged run + runtime triage'
milestones:
- milestone-2
created_at: 2026-06-23T12:50:48.796935Z
updated_at: 2026-06-23T12:50:48.796935Z
---

## Context

The workspace already compiles cleanly on macOS and most platform branches are
implemented (utun in `net/tun_device.rs`, `/etc/resolver` in
`net/resolver_config.rs`, `lsof`/`ps` discovery in `discovery.rs`, LaunchAgent
in `autostart.rs`). What has **never executed** is the privileged macOS data
path. This ticket is the entry point: do a real privileged run, observe what
actually works end-to-end, and triage findings into the dependent tickets
([[[task-24](../work/task-24.task.md)]] utun header, [[[task-25](../work/task-25.task.md)]] DNS/.local, [[[task-26](../work/task-26.task.md)]] discovery).

## Approach

Run the daemon privileged and watch the overlay come up:

```
cargo build
sudo -E ./target/debug/devenv-tunnel start --foreground
```

Then, from a second shell, start an example `.devenv.local` service (see
`examples/local-overlay/`) and exercise the full path:

- Does a `utunN` interface appear and get `10.254.0.1/16`?
- Does the `10.254.0.0/16` route install (`netstat -rn | grep 10.254`)?
- Does `/etc/resolver/devenv.local` get written, and does
  `dscacheutil -q host -a name <svc>.devenv.local` resolve to the VIP?
- Does `curl http://<svc>.devenv.local/` actually carry traffic, or does it
  resolve but hang? (A hang points straight at [[[task-24](../work/task-24.task.md)]].)
- Does discovery attribute the listener to the right process via `lsof`/`ps`?

Capture the daemon log and record which privileged operations succeed vs.
silently degrade ("continuing in cloud/local-only mode").

Done when:

- [ ] Privileged foreground run documented with the daemon log captured
- [ ] Each overlay step (utun create, route, scoped resolver) marked
      works / broken / degraded
- [ ] End-to-end `curl http://<svc>.devenv.local/` result recorded (carries
      traffic vs. resolves-but-hangs vs. no-resolve)
- [ ] Findings triaged into [[[task-24](../work/task-24.task.md)]], [[[task-25](../work/task-25.task.md)]], [[[task-26](../work/task-26.task.md)]] (close any
      that already pass; sharpen the rest with the observed behaviour)
