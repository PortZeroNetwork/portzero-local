---
id: 90c60514-3fb9-4866-b2e0-1932ad263477
slug: task-31
status: todo
title: 'macOS TUN never created: pass None for kernel-assigned utun (bare "utun" name panics tun crate)'
milestones:
- milestone-2
created_at: 2026-06-23T14:41:22.788764Z
updated_at: 2026-06-23T14:41:22.788764Z
---

## Context

Found by the [[[task-22](../work/task-22.task.md)]] privileged run on macOS. The overlay never starts —
even as root — because **TUN device creation fails** at step 1:

```
failed to create TUN device (are you root?): cannot parse integer from empty string
```

Root cause confirmed in `tun-0.6.1/src/platform/macos/device.rs`:

```rust
let id = if let Some(name) = config.name.as_ref() {
    if !name.starts_with("utun") { /* err */ }
    name[4..].parse::<u32>()? + 1u32   // "utun"[4..] == "" -> parse error
} else {
    0u32                                // None -> kernel auto-assigns next free utunN
};
```

`net/tun_device.rs::create` always sets the name via
`config.name.clone().unwrap_or_else(|| default_device_name())`, and
`default_device_name()` returns the bare `"utun"` on macOS. So we pass
`Some("utun")` → `"utun"[4..]` is empty → parse error. The crate's `None` branch
is exactly the "let the kernel pick the next free utunN" behaviour the existing
(wrong) code comment claims the bare prefix gives.

Because the overlay aborts here, the route, `/etc/resolver/devenv.local`, and
the whole data path never run — which is why [[[task-24](../work/task-24.task.md)]] and [[[task-25](../work/task-25.task.md)]] could
not be assessed.

## Approach

- On macOS, when no explicit device name is requested, **do not call
  `tuncfg.name(...)`** (leave it `None`) so the crate uses `id = 0` and the
  kernel assigns the next free `utunN`. Keep Linux (`deven0`) unchanged.
- After creation, `actual_name` already comes from `dev.get_ref().name()` (the
  crate queries the real `utunN` via getsockopt), so route install/teardown keep
  working with the kernel-assigned name.
- Do NOT bump the `tun` crate (0.7.x changes the packet-header handling and would
  entangle [[[task-24](../work/task-24.task.md)]]). Keep this a minimal, macOS-scoped fix.
- Fix the misleading `default_device_name()` / `TunConfig` doc comment that says
  the bare `utun` prefix lets the kernel choose.

Done when:

- [ ] macOS `OverlayNetwork::start` creates a `utunN` device as root (no parse
      error) — verified by a re-run of the [[[task-22](../work/task-22.task.md)]] runbook
- [ ] `10.254.0.0/16` route + `/etc/resolver/devenv.local` then install
      (unblocks assessment of [[[task-24](../work/task-24.task.md)]] / [[[task-25](../work/task-25.task.md)]])
- [ ] Linux device creation (`deven0`) unchanged; builds clean with
      `clippy -D warnings`
- [ ] Misleading comments corrected