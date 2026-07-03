---
id: 30ed07e6-5172-45b0-841f-84fb2f503a1c
slug: task-54
status: todo
title: Bound Windows real-TUN E2E CI hangs
created_at: 2026-07-03T01:06:02.483133Z
updated_at: 2026-07-03T01:06:02.483133Z
---

## Context

The Windows `real_tun_overlay` CI matrix entry can hang after printing
`real_tun_overlay: bringing up real overlay`, even though `just e2e` passes in
an elevated Windows 11 PowerShell.

## Changes

- Add a wall-clock watchdog around the privileged real-TUN E2E worker so blocking Wintun setup fails quickly with the last recorded phase.
- Add manual CI dispatch inputs for job group and runner OS so the Windows E2E matrix entry can be run by itself.

## Verification

- `cargo fmt --check`
- `CARGO_TARGET_DIR=/private/tmp/portzero-target cargo test -p portzero-daemon --test overlay_e2e real_tun_overlay --no-run`
