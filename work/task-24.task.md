---
id: 24ca8d9d-6eda-47d0-8efc-a10012e62739
slug: task-24
status: todo
title: macOS utun 4-byte protocol-family header in the smoltcp data path
milestones:
- milestone-2
depends_on:
- 2a8cf42b-859e-449a-802b-6f8ffa24caf0
- 90c60514-3fb9-4866-b2e0-1932ad263477
created_at: 2026-06-23T12:51:58.701977Z
updated_at: 2026-06-23T12:51:58.701977Z
---

## Context

macOS `utun` devices prepend a **4-byte address-family header** (`AF_INET` =
`0x00000002`, big-endian) to every L3 packet on read, and expect the same
header prepended on write. smoltcp expects a bare IP packet starting with the
version nibble.

`net/stack.rs` reads into a `STACK_MTU + 4` buffer (so someone anticipated the
header) but then forwards `buf[..n]` straight into the stack with no stripping,
and the writer path doesn't prepend the header. Whether this is a real bug
depends on how the `tun` crate (v0.6.1) handles the header on macOS — it may or
may not strip it for us. If unhandled, the overlay **compiles and resolves but
carries no traffic** on macOS (a resolve-but-hang in [[[task-22](../work/task-22.task.md)]] is the tell).

## CONFIRMED (2026-06-23) — this is the active data-path bug

The [[[task-22](../work/task-22.task.md)]] re-run (after [[[task-31](../work/task-31.task.md)]] + [[[task-33](../work/task-33.task.md)]] + [[[task-34](../work/task-34.task.md)]]) reaches
the resolve-but-hang: `hello.devenv.local` resolves to its VIP `10.254.0.2` and
`status` shows the route, but `curl http://hello.devenv.local:8080/` connects to
`10.254.0.2:8080` and **times out after 8s** — no traffic crosses the overlay.

Root cause verified by reading the crate source: `tun` 0.6.1's macOS backend
(`platform/posix/fd.rs` `Fd::read`/`Fd::write`) does raw `libc::read`/`write` on
the utun fd with **no header handling**. macOS utun prepends a 4-byte
address-family header (`00 00 00 02` = `AF_INET`, big-endian) on every read and
REQUIRES it on every write. So:

- **read:** `stack.rs` feeds `[00 00 00 02][IP…]` to smoltcp, which reads the
  first nibble `0x0` as IP version 0 → drops the SYN.
- **write:** smoltcp's bare IP packet is written with no AF header → the kernel
  rejects/misroutes it.

## Approach

- Add the macOS utun 4-byte header handling at the TUN boundary in
  `net/tun_device.rs` (preferred — keeps `stack.rs` platform-agnostic): on macOS,
  `TunReader::read` strips the leading 4 bytes before returning the IP packet,
  and `TunWriter::write` prepends the 4-byte AF header. Choose the family from
  the IP version nibble of the outgoing packet: IPv4 (`0x4…`) → `AF_INET`
  (`00 00 00 02`); IPv6 (`0x6…`) → `AF_INET6` (`00 00 00 1E`). The header is a
  big-endian `u32` protocol family.
- Keep **Linux byte-for-byte unchanged** (no header there — passthrough).
- Make the strip/prepend logic pure where possible and unit-test it (header
  bytes in/out) so it's verifiable without root.

Done when:

- [ ] macOS `curl http://hello.devenv.local:8080/` returns a real response body
- [ ] Header strip-on-read / prepend-on-write is macOS-scoped; Linux untouched
- [ ] Unit test covers the header add/strip (incl. IPv4 vs IPv6 family byte)
- [ ] Verified by a privileged [[[task-22](../work/task-22.task.md)]] re-run (curl carries traffic)
