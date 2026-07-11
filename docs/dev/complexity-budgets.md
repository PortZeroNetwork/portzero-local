# File-size / complexity budgets

Nothing enforced a maximum file size or complexity in this repo until
task-50. `client/crates/daemon/src/discovery.rs` grew to ~3.2k lines before
anyone noticed and split it (task-49) into `discovery.rs` +
`discovery/process.rs` + `discovery_loop.rs`. This document explains what is
enforced now, the threshold chosen, and what is deliberately out of scope.

## What's enforced

**File-size budget only.** A Rust source file under `client/` may not exceed
**2500 lines**. This is checked by `scripts/check-file-size-budget.sh`
(invoked via `just complexity`) and runs:

- **Locally, pre-commit** (`lefthook.yml`): scoped to staged `client/*.rs`
  files only (`just complexity --changed`), so it stays fast (a handful of
  `wc -l` calls) and matches the pattern already used by the `fmt` pre-commit
  hook.
- **In CI** (`.github/workflows/ci.yml`, `file-size-budget` job): the full
  check over every `client/*.rs` file, on every PR into `release/*`. This is
  a required job, so the budget can't be bypassed with `git commit
  --no-verify` / `git push --no-verify` — those only skip the local
  lefthook hooks, not CI.
- **On demand**: `just complexity` (whole tree) or `just complexity
  --changed` (staged files).

## Why 2500 lines

The budget is derived from the actual max file size in `client/` *after*
the discovery.rs split, not picked arbitrarily. Measured with:

```sh
find client -name '*.rs' | xargs wc -l | sort -n | tail
```

At the time this check was added, the largest file was
`client/crates/daemon/src/discovery_loop.rs` at **2168 lines**. The next
largest were `client/crates/daemon/src/tls/trust.rs` (2037) and
`client/crates/daemon/src/discovery/process.rs` (1979) — all comfortably
under 2500.

2500 gives ~330 lines of headroom above the current max: enough that normal
incremental work (adding a handler, a test, a variant) doesn't immediately
trip the check on the file that's already largest, but low enough that a
file creeping toward the ceiling is a real signal to consider splitting it
before it becomes another 3k-line discovery.rs. If a legitimate change
needs to grow a file past 2500 lines, that's exactly the point where a
split (as was done for discovery.rs) should be considered instead of
bumping the threshold.

The threshold lives in `scripts/check-file-size-budget.sh`
(`PORTZERO_FILE_SIZE_BUDGET`, default `2500`) and can be overridden via that
env var for local experimentation, but the default is what CI enforces.

## What's out of scope (for now)

The ticket asked us to evaluate cyclomatic/cognitive complexity tooling
(`cargo-complexity`, clippy's `cognitive_complexity`-style lints, etc.) in
addition to file size. As of this writing:

- `cognitive_complexity`-style lints are a clippy nightly-only lint group,
  not available on stable Rust, which is what this repo's toolchain and CI
  pin to (`dtolnay/rust-toolchain@stable`).
- There is no maintained, lightweight, stable-Rust cyclomatic/cognitive
  complexity checker that's trivially available in this environment (no
  crates.io tool was verified to install cleanly and run fast enough for a
  pre-commit hook without adding real toolchain weight).

Given that, per-function complexity enforcement is **deferred**, not
shipped. The file-size budget above is the concrete enforcement floor the
ticket calls "at minimum." If/when a suitable stable-Rust complexity tool
becomes available (or clippy stabilizes a cognitive-complexity lint), it
should plug into the same `just complexity` recipe and the same pre-commit
(changed-files-only) / CI (full-tree) split used here.

## Files

- `scripts/check-file-size-budget.sh` — the check itself.
- `justfile` — `just complexity [--changed]` recipe.
- `lefthook.yml` — `pre-commit.complexity` (changed files only).
- `.github/workflows/ci.yml` — `file-size-budget` job (full tree, required).
