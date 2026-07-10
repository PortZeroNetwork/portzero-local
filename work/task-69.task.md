---
id: 3f96d0ff-6b88-445b-a388-aa17c0231809
slug: task-69
status: todo
title: Windows trust::install hangs indefinitely (root cause of v0.1.0 release blocker)
created_at: 2026-07-09T19:13:47.961666Z
updated_at: 2026-07-09T19:13:47.961666Z
---

## Context

The v0.1.0 Windows client interop failures were caused by daemon startup wedging inside `OverlayNetwork::start`. The mitigation (30s startup deadline + per-step logging, commit 781181b) made the daemon resilient and CI green, and the step logging from Release run 29042846603 pinned the exact wedge:

```
19:11:04.839 overlay startup step: LoadingLocalCa
19:11:04.842 overlay startup step: InstallingTrust
19:11:34.844 WARN Virtual overlay network startup did not complete within 30s
```

`trust::install` (client/crates/daemon/src/tls/trust.rs, Windows `install_impl` → `install_windows_root_store` / `install_nss_dbs_windows`) hangs indefinitely on GitHub's windows-latest runner (reproduced 3/3 before the deadline was added).

## Impact after mitigation

On any Windows machine where this hang reproduces, daemon startup is delayed 30s, the overlay never comes up (cloud tunnels still work), and the local CA is never installed into the trust store, so `.portzero.local` HTTPS shows cert warnings.

## Investigation leads

- certutil -addstore on the machine Root store may be waiting on something (elevation UI? CryptoAPI network retrieval?) even when run as Administrator in a non-interactive session.
- `install_nss_dbs_windows` scans Firefox profiles and runs NSS certutil; windows-latest ships Firefox, and NSS certutil can block on a locked cert9.db.
- Child-process calls in trust.rs have no timeouts; whatever the cause, each external command invocation there should get a hard timeout so one stuck tool can't consume the whole 30s overlay budget.
- CI's e2e job (real TUN overlay) passes on windows-latest — compare what it does differently (it skips trust install: see task-54 "skip trust install in real TUN e2e", which suggests this hang may have been encountered before).
