---
id: 4e6b3b97-ba9f-4f5c-91f5-91113b701b09
slug: task-25
status: todo
title: Verify macOS scoped DNS against .local/mDNS (Bonjour) collision
milestones:
- milestone-2
depends_on:
- 2a8cf42b-859e-449a-802b-6f8ffa24caf0
- 90c60514-3fb9-4866-b2e0-1932ad263477
- 2c4637f2-5092-460d-afbb-c8806ee3dd6f
created_at: 2026-06-23T12:51:59.270601Z
updated_at: 2026-06-23T12:51:59.270601Z
---

## Context

macOS reserves the `.local` TLD for **multicast DNS / Bonjour** (mDNSResponder).
Our overlay serves `*.devenv.local` and wires it in via
`/etc/resolver/devenv.local` (`net/resolver_config.rs`). In practice
mDNSResponder *does* consult `/etc/resolver/<subdomain>` files, so a sub-label of
`.local` usually routes to our nameserver — but it's fragile and sensitive to
the macOS / mDNSResponder version. This needs an explicit end-to-end check
rather than assuming the Linux behaviour carries over.

## Approach

After [[[task-22](../work/task-22.task.md)]] writes `/etc/resolver/devenv.local`, verify resolution
actually reaches our embedded DNS and isn't shadowed by mDNS:

- `dscacheutil -q host -a name <svc>.devenv.local` returns the VIP
- `scutil --dns` shows our resolver scoped to `devenv.local` (and the custom
  port, if non-53)
- A negative case: an unknown `*.devenv.local` name does **not** resolve to a
  bogus mDNS answer
- Re-check after `dscacheutil -flushcache; killall -HUP mDNSResponder`

If the non-standard DNS **port** (the `port` line in
`macos_resolver_file_content`) turns out not to be honoured by mDNSResponder,
consider binding the embedded DNS to 53 on the loopback/overlay address on macOS.

Done when:

- [ ] `<svc>.devenv.local` resolves to the VIP via the system resolver on macOS
- [ ] Confirmed it survives a DNS cache flush / mDNSResponder restart
- [ ] Unknown names under the zone don't resolve (no mDNS shadowing)
- [ ] Any port/mechanism caveat documented in `docs/` (or fixed)
