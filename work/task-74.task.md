---
id: 17b96ab9-576c-421a-8c17-8567e8807db5
slug: task-74
status: todo
title: macOS 15.7+ hides process env from ps -E, breaking daemon service discovery
created_at: 2026-07-10T19:26:10.069650Z
updated_at: 2026-07-10T19:26:10.069650Z
---

## Symptom

On macOS 15.7.7 (Sequoia), the daemon discovers NO `PZ_TUNNEL`-tagged services, so neither local `.portzero.local` overlay tunnels nor cloud tunnels register. Reproduced in the Parallels macOS VM ([[parallels-vm-testing]]): both E2E flows (local-overlay, staging-cloud) fail — the daemon connects to the cloud edge fine but never registers a route, and the overlay comes up (DNS queries arrive) but never resolves a service.

## Root cause

macOS service discovery reads `PZ_TUNNEL` from process environments via `ps -wwwE`:
- `scan_process_env_macos` → `ps -p <pid> -wwwE -o command=`
- `scan_network_processes_sync_macos` → `ps -axo pid=,command= -wwwE`

On macOS 15.7.7, `ps -E` no longer exposes process environments — verified airtight: a process with `PZ_MARKER=zzq` in its ENV (not argv), queried as ROOT with `sudo ps -p <pid> -Eww -o command=`, returns just `sleep 30` with no env. So `ps -E` yields no `PZ_TUNNEL` and discovery finds nothing. Apple appears to have hardened `ps -E` in a 15.x update — GitHub's `macos-15` CI runner (where the interop test passes) must predate this.

## Impact

portzero's env-tag discovery is broken for users on macOS 15.7+. Windows (ReadProcessMemory) and Linux (`/proc/<pid>/environ`) are unaffected and both E2E flows pass there.

## Fix direction (needs design + verification)

Replace `ps -E` on macOS with a direct env read via `sysctl` `KERN_PROCARGS2` (same-user/root can still read argv+env this way — this is how `ps` used to source it). Verify it isn't also restricted on 15.7. If procargs2 is restricted too, env-based discovery may be unworkable on recent macOS and an alternative tagging mechanism is needed.
