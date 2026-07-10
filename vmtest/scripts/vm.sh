#!/usr/bin/env bash
# Parallels VM control plane for the system-test harness. One place for all
# prlctl orchestration so the justfile recipes stay one-liners and the logic is
# testable. macOS host only.
#
#   vm.sh up <vm>                 stop other VMs, revert golden, boot, wait, snapshot "<vm>-ready"
#   vm.sh checkpoint <vm> <name>  take a RUNNING snapshot "<vm>-<name>" (per-session reset point)
#   vm.sh reset <vm> [name]       revert to running snapshot "<vm>-<name>" (default "ready"), wait ready
#   vm.sh run <vm> <script> [a..]  run a repo script inside the guest (.ps1 on Windows, else bash)
#   vm.sh exec <vm> <cmd...>      raw command in the guest
#   vm.sh stop <vm>               power off
#   vm.sh ensure-only <vm>        stop every OTHER running VM (one-at-a-time rule)
#   vm.sh list                    VMs + snapshots
set -euo pipefail

REPO_HOST="/Users/loumtech/Documents/src/PortZeroNetwork/portzero-local"

# --- per-VM facts -----------------------------------------------------------
# "Windows 11 Pro" is the canonical VM on the external drive; "Windows 11 Pro
# verify" is a throwaway copy on the internal SSD (fast) that gets deleted when
# space is tight and re-copied from external later. Both share OS/golden/built.
vm_os() { case "$1" in
    "Windows 11 Pro"|"Windows 11 Pro verify") echo windows ;;
    "Ubuntu Linux") echo linux ;;
    "macOS 15.7.7") echo macos ;;
    *) echo "unknown VM: $1" >&2; return 1 ;;
esac; }

vm_golden() { case "$1" in
    "Windows 11 Pro"|"Windows 11 Pro verify") echo "Windows 11 Pro" ;;
    "Ubuntu Linux") echo "Ubuntu 26.04 LTS" ;;
    "macOS 15.7.7") echo "MacOS 15.7.7" ;;
esac; }

# Resolve a logical checkpoint name (e.g. "built") to the ACTUAL snapshot name.
# The Windows working VM was renamed to "Windows 11 Pro" but its built snapshot
# kept its original name "Windows Pro-built", so map it rather than rename the
# snapshot (Parallels has no snapshot-rename, and the user refers to it by name).
resolve_snap() { # <vm> <logical>
    case "$1" in
        "Windows 11 Pro"|"Windows 11 Pro verify") echo "Windows Pro-$2" ;;
        *) echo "$1-$2" ;;
    esac
}

# Repo path AS SEEN FROM THE GUEST (via the Parallels share). Verified/adjusted
# per VM; the guest must be able to read the repo to run scripts & build.
guest_repo() { case "$(vm_os "$1")" in
    windows) printf '%s' '\\Mac\Home\Documents\src\PortZeroNetwork\portzero-local' ;;
    linux)   printf '%s' '/media/psf/Home/Documents/src/PortZeroNetwork/portzero-local' ;;
    macos)   printf '%s' '/Volumes/SharedFolders/Users/loumtech/Documents/src/PortZeroNetwork/portzero-local' ;;
esac; }

# --- snapshot helpers -------------------------------------------------------
snap_id_by_name() { # <vm> <snapshot-name>
    local vm="$1" name="$2" id
    for id in $(prlctl snapshot-list "$vm" 2>/dev/null | grep -oE '\{[0-9a-f-]+\}'); do
        if prlctl snapshot-list "$vm" -i "$id" 2>/dev/null | grep -qiE "^Name: ${name}$"; then
            echo "$id"; return 0
        fi
    done
    return 1
}

is_running() { prlctl status "$1" 2>/dev/null | grep -q running; }

wait_ready() { # <vm>  — block until the guest answers, per OS
    local vm="$1" i
    for i in $(seq 1 90); do
        if [ "$(vm_os "$vm")" = windows ]; then
            prlctl exec "$vm" cmd /c "echo READY" 2>/dev/null | grep -q READY && return 0
        else
            prlctl exec "$vm" echo READY 2>/dev/null | grep -q READY && return 0
        fi
        sleep 2
    done
    echo "guest '$vm' did not become ready" >&2; return 1
}

# --- one-VM-at-a-time -------------------------------------------------------
ensure_only() { # <vm> — stop every OTHER running VM
    local keep="$1" line name
    prlctl list -o name --no-header 2>/dev/null | while IFS= read -r name; do
        [ -z "$name" ] && continue
        if [ "$name" != "$keep" ]; then
            echo ">> stopping other running VM: $name"
            prlctl stop "$name" --fast >/dev/null 2>&1 || true
        fi
    done
}

# --- commands ---------------------------------------------------------------
cmd_up() {
    local vm="$1"; local rsnap; rsnap="$(resolve_snap "$vm" ready)"
    ensure_only "$vm"
    local ready; ready="$(snap_id_by_name "$vm" "$rsnap" || true)"
    if [ -n "$ready" ]; then
        echo ">> reverting to existing running snapshot '$rsnap'"
        prlctl snapshot-switch "$vm" -i "$ready" >/dev/null
        wait_ready "$vm"; echo "ready"; return 0
    fi
    local gid; gid="$(snap_id_by_name "$vm" "$(vm_golden "$vm")")" \
        || { echo "golden snapshot '$(vm_golden "$vm")' not found" >&2; return 1; }
    echo ">> reverting to golden '$(vm_golden "$vm")' and booting"
    prlctl snapshot-switch "$vm" -i "$gid" >/dev/null
    prlctl start "$vm" >/dev/null
    wait_ready "$vm"
    prlctl snapshot "$vm" -n "$rsnap" -d "Booted + guest tools. Per-test reset point." >/dev/null
    echo "up; captured '$rsnap'"
}

cmd_checkpoint() { # <vm> <name>
    local vm="$1" name="$2"; local snap; snap="$(resolve_snap "$vm" "$name")"
    is_running "$vm" || { echo "VM '$vm' is not running" >&2; return 1; }
    # Replace an existing checkpoint of the same name so re-provisioning is idempotent.
    local old; old="$(snap_id_by_name "$vm" "$snap" || true)"
    [ -n "$old" ] && prlctl snapshot-delete "$vm" -i "$old" >/dev/null 2>&1 || true
    prlctl snapshot "$vm" -n "$snap" -d "Session checkpoint: $name" >/dev/null
    echo "checkpoint '$snap' taken"
}

cmd_reset() { # <vm> [name=ready]
    local vm="$1" name="${2:-ready}" id; local snap; snap="$(resolve_snap "$vm" "$name")"
    id="$(snap_id_by_name "$vm" "$snap")" \
        || { echo "no snapshot '$snap' (run: vm.sh up / checkpoint)" >&2; return 1; }
    prlctl snapshot-switch "$vm" -i "$id" >/dev/null
    wait_ready "$vm"
    echo "reset to '$snap'"
}

# Kill lingering test/daemon processes inside the guest. Killing a host-side
# `prlctl exec` does NOT kill the process it launched inside the VM, so on
# timeout we must reach in and clean up, or a wedged daemon lingers (and holds
# the TUN/ports) into the next test.
guest_kill_stragglers() { # <vm>
    local vm="$1"
    if [ "$(vm_os "$vm")" = windows ]; then
        prlctl exec "$vm" cmd /c "taskkill /F /IM portzero.exe /T 2>nul & taskkill /F /IM powershell.exe /FI \"WINDOWTITLE ne *vm.sh*\" 2>nul & exit 0" >/dev/null 2>&1 || true
    else
        prlctl exec "$vm" bash -lc 'pkill -9 -f portzero >/dev/null 2>&1; pkill -9 -f http-echo >/dev/null 2>&1; true' >/dev/null 2>&1 || true
    fi
}

# Run a command with a hard host-side timeout (macOS has no `timeout`). On
# expiry, kill the host process AND clean up guest stragglers, then return 124
# (same convention as GNU timeout) so callers can distinguish a wedge from a
# normal failure. VM_RUN_TIMEOUT seconds; default 1800 (30m) covers a cold
# build; set it low (e.g. 300) for tests so a wedge surfaces in minutes.
run_guarded() { # <vm> <cmd...>
    local vm="$1"; shift
    local secs="${VM_RUN_TIMEOUT:-1800}"
    local marker; marker="$(mktemp)"
    "$@" &
    local pid=$!
    ( sleep "$secs"; echo 1 > "$marker"; kill -TERM "$pid" 2>/dev/null; sleep 3; kill -KILL "$pid" 2>/dev/null ) &
    local killer=$!
    wait "$pid" 2>/dev/null; local rc=$?
    if [ -s "$marker" ]; then                 # killer fired -> genuine timeout
        rm -f "$marker"; kill "$killer" 2>/dev/null; wait "$killer" 2>/dev/null
        echo "vm.sh: TIMEOUT after ${secs}s — killing guest stragglers" >&2
        guest_kill_stragglers "$vm"
        return 124
    fi
    kill "$killer" 2>/dev/null; wait "$killer" 2>/dev/null; rm -f "$marker"
    return "$rc"
}

cmd_run() { # <vm> <repo-relative-script> [args...]
    local vm="$1" script="$2"; shift 2
    local base; base="$(guest_repo "$vm")"
    if [ "$(vm_os "$vm")" = windows ]; then
        local win="${base}\\${script//\//\\}"
        run_guarded "$vm" prlctl exec "$vm" powershell -NoProfile -ExecutionPolicy Bypass -File "$win" "$@"
    else
        # Forward selected host env into the guest (macOS uses a host-built
        # binary + a different secrets path). `env` with no assignments is a
        # harmless passthrough.
        local envargs=()
        [ -n "${PORTZERO_EXE:-}" ] && envargs+=("PORTZERO_EXE=$PORTZERO_EXE")
        [ -n "${STAGING_SECRETS:-}" ] && envargs+=("STAGING_SECRETS=$STAGING_SECRETS")
        run_guarded "$vm" prlctl exec "$vm" env "${envargs[@]}" bash "${base}/${script}" "$@"
    fi
}

cmd_exec() { local vm="$1"; shift; prlctl exec "$vm" "$@"; }
cmd_stop() { prlctl stop "$1" --fast 2>&1 | tail -1; }
cmd_list() { prlctl list --all; for v in "Windows 11 Pro" "Windows 10 Pro" "Ubuntu Linux" "macOS 15.7.7"; do echo "--- $v"; prlctl snapshot-list "$v" 2>/dev/null | grep -oE '\{[0-9a-f-]+\}|Name:.*' || true; done; }

case "${1:-}" in
    up)         cmd_up "$2" ;;
    checkpoint) cmd_checkpoint "$2" "$3" ;;
    reset)      cmd_reset "$2" "${3:-}" ;;
    run)        shift; cmd_run "$@" ;;
    exec)       shift; cmd_exec "$@" ;;
    stop)       cmd_stop "$2" ;;
    ensure-only) ensure_only "$2" ;;
    list)       cmd_list ;;
    *) echo "usage: vm.sh {up|checkpoint|reset|run|exec|stop|ensure-only|list} ..." >&2; exit 2 ;;
esac
