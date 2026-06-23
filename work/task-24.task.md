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

## Approach

- Confirm from [[[task-22](../work/task-22.task.md)]]'s run whether traffic flows. If it does, the `tun`
  crate already strips the header — close this and just add a regression note.
- If it hangs, determine `tun` 0.6.1's macOS behaviour (does `Reader`/`Writer`
  include the 4-byte prefix?). Then either enable the crate's
  packet-information handling, or strip-on-read / prepend-on-write in the macOS
  branch of `stack.rs` (`tun_reader.read` / `tun_writer.write`).
- Keep Linux behaviour byte-for-byte unchanged (no header there).

Done when:

- [ ] macOS `curl http://<svc>.devenv.local/` returns a real response body
- [ ] Header handling is macOS-scoped and leaves Linux untouched
- [ ] A test or documented manual check guards against regression
