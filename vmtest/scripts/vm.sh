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
#   vm.sh test <platform> [flavor] windows|linux|macos, flavor local (default, no
#                                  cloud/secrets) or staging (real cloud tunnel):
#                                  recover-if-missing, reset to "built", run the e2e script
#   vm.sh list                    VMs + snapshots
set -euo pipefail

# Derived, not hardcoded: the Parallels shared folders are configured at the
# VM level to share the host's $HOME, so this works from any checkout under
# $HOME — the interactive daily-driver clone, or a CI runner's own workspace.
REPO_HOST="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# The 4TB drive holding the archival copy of every VM. Never booted directly
# except when a user explicitly opts into it (see resolve_effective_vm).
EXTERNAL_DRIVE="/Volumes/MBP-Sidecar"

# Host-side location of the staging-tunnel test's rotated seed token, mirroring
# the guest-side defaults baked into e2e-staging-tunnel.sh/.ps1 (which reach
# the same file via their own \\Mac\MBP-Sidecar / /media/psf/MBP-Sidecar
# shares). Only used to auto-push it into macOS guests — see cmd_test.
STAGING_SECRETS_HOST_DEFAULT="$EXTERNAL_DRIVE/loumtech/vm-toolchain-cache/common/staging-e2e.env"

# --- per-VM facts -----------------------------------------------------------
# Orthogonal per platform: "<name>" is the internal working copy (~/Parallels)
# — the only thing vm-up/vm-test ever boots. "<name> archive" is the dormant
# 4TB copy, registered only so it's prlctl-clonable, never started on its own.
# If the internal copy is missing, resolve_effective_vm() recovers from the
# archive (clone to internal, or run this session straight off the archive).
vm_os() { case "$1" in
    "Windows 11 Pro"|"Windows 11 Pro archive") echo windows ;;
    "Ubuntu Linux"|"Ubuntu Linux archive")     echo linux ;;
    "macOS 15.7.7"|"macOS 15.7.7 archive")     echo macos ;;
    *) echo "unknown VM: $1" >&2; return 1 ;;
esac; }

# Internal working-copy name -> its 4TB archive name.
archive_vm() { case "$1" in
    "Windows 11 Pro") echo "Windows 11 Pro archive" ;;
    "Ubuntu Linux")   echo "Ubuntu Linux archive" ;;
    "macOS 15.7.7")   echo "macOS 15.7.7 archive" ;;
    *) echo "no archive mapping for '$1'" >&2; return 1 ;;
esac; }

# Platform shorthand ("windows"/"linux"/"macos", as typed by `just vm-test
# <platform>`) -> the internal working-copy name. Keep in sync with vm_os().
platform_vm() { case "$1" in
    windows) echo "Windows 11 Pro" ;;
    linux)   echo "Ubuntu Linux" ;;
    macos)   echo "macOS 15.7.7" ;;
    *) echo "unknown platform: '$1' (expected windows|linux|macos)" >&2; return 1 ;;
esac; }

# The smoke-test script for `vm.sh test`, per guest OS and flavor. "local"
# (default): local overlay E2E, no cloud, no secrets. "staging": real cloud
# tunnel against staging, needs the rotated seed token (see STAGING_SECRETS
# handling in cmd_test/push_secrets_macos). Both scripts are shared by
# Linux/macOS; Windows needs the .ps1 twin.
default_test_script() { # <vm> [flavor=local]
    local base="e2e-local-overlay"
    [ "${2:-local}" = staging ] && base="e2e-staging-tunnel"
    if [ "$(vm_os "$1")" = windows ]; then
        echo "vmtest/scripts/${base}.ps1"
    else
        echo "vmtest/scripts/${base}.sh"
    fi
}

# Every VM/alias uses one uniform "portzero-<logical>" snapshot naming
# convention (golden/ready/built), so this needs no per-VM special-casing.
# Cloning an archive to internal carries its snapshot tree along, so a fresh
# internal working copy inherits these names for free.
resolve_snap() { echo "portzero-$2"; } # <vm> <logical>

# Repo path AS SEEN FROM THE GUEST (via the Parallels share). Windows/Linux
# read the repo live off the shared folder, which maps the whole host $HOME
# — so this is REPO_HOST's path relative to $HOME, computed rather than
# hardcoded, so it's correct whether REPO_HOST is the interactive daily-driver
# clone or a CI runner's own workspace under $HOME. macOS does NOT use this:
# its Parallels shared folder is SMB-backed and requires an authenticated GUI
# login the guest never has after a snapshot revert, so cmd_run pushes the
# script over `prlctl exec` instead (see cmd_run).
guest_repo() {
    case "$REPO_HOST" in
        "$HOME"/*) ;;
        *) echo "REPO_HOST ($REPO_HOST) is not under \$HOME ($HOME) — the guest shared folder can't reach it" >&2; return 1 ;;
    esac
    local rel="${REPO_HOST#"$HOME"/}"
    case "$(vm_os "$1")" in
        windows) printf '\\\\Mac\\Home\\%s' "${rel//\//\\}" ;;
        linux)   printf '/media/psf/Home/%s' "$rel" ;;
    esac
}

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

wait_ready() { # <vm>  — block until the guest answers, per OS. Prints a
    # heartbeat every ~10s so a slow boot/revert never looks hung.
    local vm="$1" i elapsed
    echo ">> waiting for guest '$vm' to become ready..."
    for i in $(seq 1 90); do
        elapsed=$(( (i - 1) * 2 ))
        if [ "$(vm_os "$vm")" = windows ]; then
            prlctl exec "$vm" cmd /c "echo READY" 2>/dev/null | grep -q READY \
                && { echo ">> guest '$vm' ready (${elapsed}s)"; return 0; }
        else
            prlctl exec "$vm" echo READY 2>/dev/null | grep -q READY \
                && { echo ">> guest '$vm' ready (${elapsed}s)"; return 0; }
        fi
        if [ "$elapsed" -gt 0 ] && [ $(( elapsed % 10 )) -eq 0 ]; then
            echo ">> still waiting for guest '$vm'... (${elapsed}s/180s)"
        fi
        sleep 2
    done
    echo "guest '$vm' did not become ready after 180s" >&2; return 1
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

# --- recover-if-missing preflight -------------------------------------------
# Make sure SOME bootable registration of <working-vm-name> exists locally
# before vm-test tries to reset/run it. If the internal working copy was
# deleted (e.g. to reclaim disk space), walk back to the 4TB archive: wait for
# the drive to be mounted, then let the user choose to clone it to internal
# (slow, one-time) or run this session directly off the external drive.
# Prints the vm name to actually operate on to stdout; everything else (status,
# prompts) goes to stderr so the caller's command substitution stays clean.
resolve_effective_vm() { # <working-vm-name>
    local vm="$1"
    if prlctl list -i "$vm" >/dev/null 2>&1; then
        echo "$vm"; return 0
    fi

    echo "!! '$vm' not found on internal disk." >&2
    local archive; archive="$(archive_vm "$vm")" || return 1

    if [ ! -d "$EXTERNAL_DRIVE" ] && [ ! -t 0 ]; then
        echo "external drive not mounted at $EXTERNAL_DRIVE and no terminal to prompt — aborting '$vm'" >&2
        return 1
    fi
    while [ ! -d "$EXTERNAL_DRIVE" ]; do
        echo "!! external drive not mounted at $EXTERNAL_DRIVE." >&2
        read -r -p "   Plug it in, then press Enter to check again (Ctrl-C to abort)... " _ < /dev/tty
    done

    prlctl list -i "$archive" >/dev/null 2>&1 || {
        echo "archive '$archive' not found on the external drive either — nothing to recover '$vm' from" >&2
        return 1
    }

    if ! snap_id_by_name "$archive" "portzero-built" >/dev/null; then
        echo "!! WARNING: archive '$archive' has no 'portzero-built' snapshot (stale/never-provisioned mirror)." >&2
        echo "   Testing may fail until it's refreshed or re-provisioned." >&2
    fi

    local perf_note=""
    [ "$(vm_os "$vm")" = macos ] && perf_note=" (macOS runs poorly off external storage — expect flakiness/slowness)"

    if [ ! -t 0 ]; then
        # No one to ask — don't hang, degrade to the always-available option.
        echo ">> non-interactive session: running '$vm' directly off '$archive' this time.$perf_note" >&2
        echo "$archive"; return 0
    fi

    echo "   found archive '$archive' on the external drive.$perf_note" >&2
    echo "   [1] clone to internal now (one-time, slow: tens of minutes, ~100GB)" >&2
    echo "   [2] run directly off the external drive this session (no copy)" >&2
    echo "   [3] abort" >&2
    local choice
    read -r -p "   choice [1/2/3]: " choice < /dev/tty
    case "$choice" in
        1)
            echo ">> cloning '$archive' -> '$vm' on internal disk (this will take a while)..." >&2
            prlctl clone "$archive" --name "$vm" --dst "$HOME/Parallels" >&2
            echo ">> clone complete: '$vm' now available locally." >&2
            echo "$vm"
            ;;
        2)
            echo ">> running this session directly off the external archive '$archive'." >&2
            echo "$archive"
            ;;
        *)
            echo "aborted: '$vm' not available" >&2
            return 1
            ;;
    esac
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
    local gid; gid="$(snap_id_by_name "$vm" "portzero-golden")" \
        || { echo "golden snapshot 'portzero-golden' not found on '$vm'" >&2; return 1; }
    echo ">> reverting to golden 'portzero-golden' and booting"
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
    ensure_only "$vm"
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
    local start; start="$(date +%s)"
    "$@" &
    local pid=$!
    ( sleep "$secs"; echo 1 > "$marker"; kill -TERM "$pid" 2>/dev/null; sleep 3; kill -KILL "$pid" 2>/dev/null ) &
    local killer=$!
    # Heartbeat on the host side so a guest script that goes quiet (e.g. mid
    # MSI install, before it prints its next PHASE line) never looks hung.
    ( while kill -0 "$pid" 2>/dev/null; do
          sleep 30
          kill -0 "$pid" 2>/dev/null \
              && echo ">> still running on '$vm'... ($(( $(date +%s) - start ))s elapsed, ${secs}s budget)" >&2
      done ) &
    local heartbeat=$!
    wait "$pid" 2>/dev/null; local rc=$?
    kill "$heartbeat" 2>/dev/null; wait "$heartbeat" 2>/dev/null
    if [ -s "$marker" ]; then                 # killer fired -> genuine timeout
        rm -f "$marker"; kill "$killer" 2>/dev/null; wait "$killer" 2>/dev/null
        echo "vm.sh: TIMEOUT after ${secs}s — killing guest stragglers" >&2
        guest_kill_stragglers "$vm"
        return 124
    fi
    kill "$killer" 2>/dev/null; wait "$killer" 2>/dev/null; rm -f "$marker"
    return "$rc"
}

# Push <script>'s containing directory (so sibling files like lib/http-echo.pl
# resolve) into the macOS guest over `prlctl exec ... bash -s`, bypassing the
# broken SharedFolders SMB mount (see guest_repo comment). Extracts under
# /tmp/portzero-vmtest, mirroring the repo-relative path. Prints the guest-side
# absolute path to the script on stdout.
push_script_macos() { # <vm> <repo-relative-script>
    local vm="$1" script="$2" script_dir; script_dir="$(dirname "$script")"
    local guest_dir="/tmp/portzero-vmtest"
    {
        printf 'mkdir -p %q\n' "$guest_dir"
        printf 'base64 -d > %q/payload.tar <<'"'"'PZEOF'"'"'\n' "$guest_dir"
        ( cd "$REPO_HOST" && tar -cf - "$script_dir" ) | base64
        printf 'PZEOF\n'
        printf 'cd %q && tar -xf payload.tar && rm -f payload.tar\n' "$guest_dir"
    } | prlctl exec "$vm" bash -s >&2
    printf '%s/%s' "$guest_dir" "$script"
}

# macOS is host-built-artifact-only (no toolchain in the guest) and the
# SharedFolders mount that would normally hand the guest a binary path is
# broken (see guest_repo comment) — so push the host-built binary directly.
# Prints the guest-side absolute path to the pushed binary on stdout.
push_binary_macos() { # <vm> <host-path-to-binary>
    local vm="$1" host_bin="$2"
    local guest_bin="/tmp/portzero-vmtest/bin/portzero"
    {
        printf 'mkdir -p %q\n' "$(dirname "$guest_bin")"
        printf 'base64 -d > %q <<'"'"'PZEOF'"'"'\n' "$guest_bin"
        base64 -i "$host_bin"
        printf 'PZEOF\n'
        printf 'chmod +x %q\n' "$guest_bin"
    } | prlctl exec "$vm" bash -s >&2
    printf '%s' "$guest_bin"
}

# Same problem as push_binary_macos, for the staging-tunnel test's secrets
# file: the shared folder Windows/Linux read it through doesn't work on
# macOS. Prints the guest-side absolute path to the pushed file on stdout.
push_secrets_macos() { # <vm> <host-path-to-secrets-file>
    local vm="$1" host_file="$2"
    local guest_file="/tmp/portzero-vmtest/staging-e2e.env"
    {
        printf 'mkdir -p %q\n' "$(dirname "$guest_file")"
        printf 'base64 -d > %q <<'"'"'PZEOF'"'"'\n' "$guest_file"
        base64 -i "$host_file"
        printf 'PZEOF\n'
    } | prlctl exec "$vm" bash -s >&2
    printf '%s' "$guest_file"
}

# CI-downloaded (or manually dropped) pre-built binary for windows/linux,
# placed under the repo checkout so the existing (working) shared folder
# already exposes it to the guest — no push-over-exec needed there, unlike
# macOS. Without this, the guest's "built" snapshot binary — baked in
# whenever someone last ran the provisioning scripts by hand — is whatever it
# was, not necessarily the commit under test. Prints nothing if absent.
downloaded_artifact_host_path() { # <os: windows|linux>
    case "$1" in
        windows) echo "$REPO_HOST/vmtest/.downloaded-artifacts/windows/portzero.exe" ;;
        linux)   echo "$REPO_HOST/vmtest/.downloaded-artifacts/linux/portzero" ;;
    esac
}

# Guest-side path for anything under REPO_HOST (which windows/linux's shared
# folder exposes in full), given its position relative to REPO_HOST.
guest_path_under_repo() { # <vm> <host-path-under-REPO_HOST>
    local vm="$1" host_path="$2"
    local rel="${host_path#"$REPO_HOST"/}"
    if [ "$(vm_os "$vm")" = windows ]; then
        printf '%s\\%s' "$(guest_repo "$vm")" "${rel//\//\\}"
    else
        printf '%s/%s' "$(guest_repo "$vm")" "$rel"
    fi
}

cmd_run() { # <vm> <repo-relative-script> [args...]
    local vm="$1" script="$2"; shift 2
    local os; os="$(vm_os "$vm")"
    if [ "$os" = windows ]; then
        local base; base="$(guest_repo "$vm")"
        local win="${base}\\${script//\//\\}"
        local exe_override=""
        if [ -z "${PORTZERO_EXE:-}" ]; then
            local artifact; artifact="$(downloaded_artifact_host_path windows)"
            [ -f "$artifact" ] && exe_override="$(guest_path_under_repo "$vm" "$artifact")"
        fi
        if [ -n "$exe_override" ]; then
            echo ">> using downloaded artifact as PORTZERO_EXE: $exe_override" >&2
            run_guarded "$vm" prlctl exec "$vm" cmd /c "set \"PORTZERO_EXE=$exe_override\"&& powershell -NoProfile -ExecutionPolicy Bypass -File \"$win\""
        else
            run_guarded "$vm" prlctl exec "$vm" powershell -NoProfile -ExecutionPolicy Bypass -File "$win" "$@"
        fi
    else
        # Forward selected host env into the guest (macOS uses a host-built
        # binary + a different secrets path). `env` with no assignments is a
        # harmless passthrough.
        local envargs=()
        [ -n "${PORTZERO_EXE:-}" ] && envargs+=("PORTZERO_EXE=$PORTZERO_EXE")
        [ -n "${STAGING_SECRETS:-}" ] && envargs+=("STAGING_SECRETS=$STAGING_SECRETS")
        local target
        if [ "$os" = macos ]; then
            target="$(push_script_macos "$vm" "$script")"
            if [ -z "${PORTZERO_EXE:-}" ] && [ -x "$REPO_HOST/target/release/portzero" ]; then
                echo ">> pushing host-built macOS binary into the guest (no toolchain there, shared folder unavailable)..." >&2
                envargs+=("PORTZERO_EXE=$(push_binary_macos "$vm" "$REPO_HOST/target/release/portzero")")
            fi
            # Harmless for scripts that don't read STAGING_SECRETS (e.g. the
            # local-overlay flavor); only the staging-tunnel flavor uses it.
            if [ -z "${STAGING_SECRETS:-}" ] && [ -f "$STAGING_SECRETS_HOST_DEFAULT" ]; then
                envargs+=("STAGING_SECRETS=$(push_secrets_macos "$vm" "$STAGING_SECRETS_HOST_DEFAULT")")
            fi
        else
            target="$(guest_repo "$vm")/${script}"
            if [ -z "${PORTZERO_EXE:-}" ]; then
                local artifact; artifact="$(downloaded_artifact_host_path linux)"
                if [ -f "$artifact" ]; then
                    echo ">> using downloaded artifact as PORTZERO_EXE" >&2
                    envargs+=("PORTZERO_EXE=$(guest_path_under_repo "$vm" "$artifact")")
                fi
            fi
        fi
        run_guarded "$vm" prlctl exec "$vm" env "${envargs[@]}" bash "$target" "$@"
    fi
}

cmd_test() { # <platform: windows|linux|macos> [flavor=local|staging]
    local platform="$1" flavor="${2:-local}"
    local vm; vm="$(platform_vm "$platform")" || return 1
    local start; start="$(date +%s)"
    local rc=0
    echo "=== $platform ($flavor): starting ==="
    local effective_vm
    effective_vm="$(resolve_effective_vm "$vm")" || return 1
    echo ">> using '$effective_vm'"
    local script; script="$(default_test_script "$effective_vm" "$flavor")"
    cmd_reset "$effective_vm" built || rc=$?
    if [ "$rc" -eq 0 ]; then
        echo ">> running $script on '$effective_vm'..."
        VM_RUN_TIMEOUT="${VM_RUN_TIMEOUT:-360}" cmd_run "$effective_vm" "$script" || rc=$?
    fi
    local elapsed=$(( $(date +%s) - start ))
    if [ "$rc" -eq 0 ]; then
        echo "=== $platform ($flavor): PASS (${elapsed}s) ==="
    else
        echo "=== $platform ($flavor): FAIL (exit $rc, ${elapsed}s) — see output above ===" >&2
    fi
    return "$rc"
}

cmd_exec() { local vm="$1"; shift; prlctl exec "$vm" "$@"; }
cmd_stop() { prlctl stop "$1" --fast 2>&1 | tail -1; }
cmd_list() { prlctl list --all; for v in "Windows 11 Pro" "Windows 11 Pro archive" "Windows 10 Pro" "Ubuntu Linux" "Ubuntu Linux archive" "macOS 15.7.7" "macOS 15.7.7 archive"; do echo "--- $v"; prlctl snapshot-list "$v" 2>/dev/null | grep -oE '\{[0-9a-f-]+\}|Name:.*' || true; done; }

case "${1:-}" in
    up)         cmd_up "$2" ;;
    checkpoint) cmd_checkpoint "$2" "$3" ;;
    reset)      cmd_reset "$2" "${3:-}" ;;
    run)        shift; cmd_run "$@" ;;
    exec)       shift; cmd_exec "$@" ;;
    stop)       cmd_stop "$2" ;;
    ensure-only) ensure_only "$2" ;;
    test)       cmd_test "$2" "${3:-}" ;;
    list)       cmd_list ;;
    *) echo "usage: vm.sh {up|checkpoint|reset|run|exec|stop|ensure-only|test|list} ..." >&2; exit 2 ;;
esac
