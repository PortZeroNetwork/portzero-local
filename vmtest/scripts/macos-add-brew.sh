#!/usr/bin/env bash
# One-time (re)provisioning: bake Homebrew into the macOS VM's "portzero-built"
# reset point — the snapshot `just vm-test macos` and the CI VM-E2E job revert
# to — so both exercise the real `brew install portzero` path.
#
# Runs entirely on the INTERNAL working copy (~/Parallels); macOS runs poorly
# off the 4 TB spinny drive, so the install happens on fast storage and the
# result is mirrored out afterwards with `just vm-sync-macos`.
#
# Snapshot topology it produces (nothing pristine is destroyed):
#   ... -> macOS 15.7.7-built (pristine)     [pre-existing]
#          -> portzero-pre-brew (pristine)   [preservation snapshot, taken once]
#             -> portzero-built (Homebrew)   [re-captured reset point — churns]
#                -> portzero-toolchain (Homebrew)  [permanent CLT+brew anchor]
# The golden powered-off "MacOS 15.7.7" snapshot is never touched, so a fully
# clean baseline also remains available.
#
# Why BOTH portzero-built and portzero-toolchain (identical CLT+brew state):
# `portzero-built` is the reset point vm-test/CI revert to, and a future
# re-provision (`vm-checkpoint built`) REPLACES it — so it's churn-prone.
# `portzero-toolchain` is a permanent anchor the routine flow never overwrites:
# it is the durable cache of the one-time CLT+Homebrew download (metered link —
# see vmtest/README.md "Cache-first"). If `built` is ever clobbered, revert to
# `portzero-toolchain` and re-checkpoint `built` — no re-download, ever.
#
# Idempotent-friendly: the guest install script no-ops if brew already exists,
# and the pristine preservation snapshot is only taken if it isn't there yet.
set -euo pipefail

REPO_HOST="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="macOS 15.7.7"
VMSH="$REPO_HOST/vmtest/scripts/vm.sh"

has_snap() { # <snapshot-name> — prlctl snapshot-list only shows names with -i <id>
    local name="$1" id
    for id in $(prlctl snapshot-list "$VM" 2>/dev/null | grep -oE '\{[0-9a-f-]+\}'); do
        prlctl snapshot-list "$VM" -i "$id" 2>/dev/null | grep -qiE "^Name: ${name}$" && return 0
    done
    return 1
}

echo ">> [1/4] reset '$VM' to its pristine 'built' reset point"
bash "$VMSH" reset "$VM" built

if has_snap "portzero-pre-brew"; then
    echo ">> [2/4] 'portzero-pre-brew' already exists — keeping the original pristine snapshot, not re-taking it"
else
    echo ">> [2/4] preserving pristine state as 'portzero-pre-brew'"
    bash "$VMSH" checkpoint "$VM" pre-brew
fi

echo ">> [3/4] installing Homebrew in the guest (CLT + brew; several minutes)"
VM_RUN_TIMEOUT="${VM_RUN_TIMEOUT:-2400}" \
    bash "$VMSH" run "$VM" vmtest/scripts/macos-install-homebrew.sh

echo ">> [4/5] re-capturing 'portzero-built' with Homebrew baked in"
bash "$VMSH" checkpoint "$VM" built

echo ">> [5/5] capturing the permanent 'portzero-toolchain' anchor (never auto-overwritten)"
bash "$VMSH" checkpoint "$VM" toolchain

cat <<EOF

Done.
  portzero-built      -> CLT + Homebrew (vm-test/CI reset point)
  portzero-toolchain  -> CLT + Homebrew (permanent anchor; the durable cache of
                         the one-time download — revert here if 'built' is lost)
  portzero-pre-brew   -> pristine, no CLT/brew (preserved)
Next, mirror the updated VM to the 4 TB drive (off-machine backup of the cache):
    prlctl stop "$VM" --fast
    just vm-sync-macos
EOF
