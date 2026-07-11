#!/usr/bin/env bash
# check-file-size-budget.sh
#
# Enforces a maximum line-count budget on Rust source files under `client/`.
#
# Why: nothing currently stops a single file from growing without bound.
# `client/crates/daemon/src/discovery.rs` reached ~3.2k lines before being
# split (see task-49) purely because no check ever flagged it. This script
# is the "at minimum" floor called out in task-50: a cheap, dependency-free
# line-count budget that catches the same problem early, before a file
# becomes painful to split.
#
# Threshold rationale (see docs/dev/complexity-budgets.md for the full
# writeup): after the discovery.rs split, the largest file in `client/` is
# `client/crates/daemon/src/discovery_loop.rs` at 2168 lines. The budget is
# set to 2500 lines: enough headroom that ordinary incremental work doesn't
# immediately trip the check, but low enough that a file approaching it is a
# real signal to consider splitting.
#
# Usage:
#   scripts/check-file-size-budget.sh                 # check all client/*.rs files
#   scripts/check-file-size-budget.sh --changed        # check only staged/changed *.rs files under client/
#
# Exit status: 0 if every file is within budget, 1 otherwise (with a report
# of the offending files).

set -euo pipefail

# Max lines allowed in a single Rust source file under client/.
MAX_LINES="${PORTZERO_FILE_SIZE_BUDGET:-2500}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

mode="all"
if [[ "${1:-}" == "--changed" ]]; then
  mode="changed"
fi

if [[ "$mode" == "changed" ]]; then
  # Staged files (pre-commit) plus any changed-but-unstaged, restricted to
  # client/*.rs. Falls back to an empty set (nothing to check) if there is
  # no git repo (e.g. running from a tarball) — file-size check then no-ops.
  mapfile -t files < <(git diff --cached --name-only --diff-filter=ACMR -- 'client/*.rs' 2>/dev/null || true)
  if [[ ${#files[@]} -eq 0 ]]; then
    echo "check-file-size-budget: no changed client/*.rs files staged, nothing to check."
    exit 0
  fi
else
  mapfile -t files < <(find client -name '*.rs' -type f | sort)
fi

failed=0
for f in "${files[@]}"; do
  [[ -f "$f" ]] || continue
  lines=$(wc -l < "$f" | tr -d ' ')
  if (( lines > MAX_LINES )); then
    echo "FAIL: $f has $lines lines (budget: $MAX_LINES)"
    failed=1
  fi
done

if [[ "$failed" -ne 0 ]]; then
  echo ""
  echo "One or more files under client/ exceed the ${MAX_LINES}-line budget."
  echo "Consider splitting the file into smaller modules (see docs/dev/complexity-budgets.md)."
  exit 1
fi

echo "check-file-size-budget: all client/*.rs files within ${MAX_LINES}-line budget."
