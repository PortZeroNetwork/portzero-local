# Parallels VM system-test harness

Host-driven system tests for portzero against real Windows / Linux / macOS
guests, run from the macOS host that owns the Parallels VMs. The point is
coverage the unit/E2E suites can't give: real installers, real trust stores,
real TUN/wintun adapters, real DNS, driven end to end on a genuine OS.

## Why this exists (and its one hard limit)

CI already builds every platform and runs client-interop E2E against staging.
What it *can't* cheaply do is exercise privileged, OS-specific install/runtime
paths interactively and iterate on them in seconds. A local VM can.

**The limit, learned the hard way:** a VM only reproduces a bug to the extent
its image matches the target. The v0.1.0 Windows CI hang (daemon wedged in the
trust-store install) does **not** reproduce on a clean Windows 11 snapshot — it
depends on what GitHub's `windows-latest` image has on `PATH`. So use the VM for
*behaviour you can make faithful* (install the real MSI, create a real wintun
adapter, register a real tunnel), and treat "repro an env-specific CI hang" as a
separate exercise that needs the CI image replicated, or a defensive fix instead.

## The reset model (fast, pristine, no boot wait)

Each VM has a hand-made **golden** powered-off snapshot: a clean, configured
baseline (for Windows: the fresh Win11 Pro install). We never test on it.

`just vm-up` reverts to golden, boots once, waits for Parallels Tools, then takes
a **running** snapshot `"<vm>-ready"`. Reverting to a *running* snapshot resumes
from RAM — a fully booted, logged-in OS in ~25s with no boot sequence. That is
the per-test reset point:

```
just vm-up "Windows Pro"      # one-time per session: golden -> boot -> snapshot running state
just vm-reset "Windows Pro"   # between tests: back to clean booted state (~25s)
```

## Running things in the guest

`prlctl exec` runs as `NT AUTHORITY\SYSTEM` on Windows — the same account the
daemon runs as under CI, which is exactly the privilege context most install
bugs live in. Scripts live in `vmtest/scripts/` and run **by UNC path** over the
automatic `\\Mac\Home` share, so nothing is copied into the guest and edits on
the Mac are picked up immediately:

```
just vm-run "Windows Pro" vmtest/scripts/inventory.ps1
just vm-inventory "Windows Pro"      # what dev tooling the guest already has
just vm-repro-trust "Windows Pro"    # the trust-install hang probe
```

## Offline provisioning (metered-connection friendly)

Installers are downloaded **once** to a cross-repo cache on the 4 TB drive,
`/Volumes/MBP-Sidecar/loumtech/vm-toolchain-cache/` (shared into every guest; see
that dir's `README.md` for the full layout and guest mount paths). The cache
survives snapshot reverts, so guests install from it offline and metered sessions
never re-download. macOS's Rust toolchain lives on the **internal** disk
(`~/Parallels/vm-toolchain-cache/macos/`, macOS wants fast storage) and is mirrored
to the 4 TB by `just vm-sync-macos`.

```
TIER=toolchain just vm-fetch    # rustup + git + VS bootstrapper + Rust toolchains
# then the multi-GB parts that need a guest, once:
just vm-run "Windows Pro" vmtest/scripts/build-msvc-layout.ps1   # MSVC offline layout
```

Tiers:
- `artifact` — nothing pre-downloaded; test the CI-built `portzero.exe`/MSI,
  pulled per-run with `gh run download`. Minimal data.
- `toolchain` — Rust + git + MSVC layout / Ubuntu debs, so the daemon builds
  **inside the VM** fully offline (no CI round-trip for platform-only iteration).
  macOS is intentionally artifact-only — the host builds macOS natively and the
  macOS VM stays a pristine end-user machine for install/uninstall testing.

## Adding a system test

1. Write `vmtest/scripts/<name>.ps1` (or `.sh` for Linux/macOS guests). Make it
   print `KEY=value` / `PHASE=… ok=…` lines — greppable assertions, not prose.
2. Time-box anything that can hang (see `repro-trust-install.ps1`: each phase
   runs in a child job with `Wait-Job -Timeout`, so a real wedge is *proven and
   named*, never an indefinite stall).
3. Reset first, then run: `just vm-reset "<vm>" && just vm-run "<vm>" vmtest/scripts/<name>.ps1`.
4. Keep guest state out of the repo; keep the script and its expected output in.

## VMs

| VM            | Guest        | Use                                        |
|---------------|--------------|--------------------------------------------|
| `Windows Pro` | Windows 11   | MSI install, trust store, wintun, tunnels  |
| `Ubuntu Linux`| Linux        | .deb/install.sh, CAP_NET_ADMIN, /dev/net/tun |
| `macOS 15.7.7`| macOS        | pkg/brew, LaunchDaemon, utun               |

Snapshot convention is the same for all three: a golden powered-off baseline
plus a `"<vm>-ready"` running snapshot as the reset point.
