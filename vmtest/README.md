# Parallels VM system-test harness

Host-driven system tests for portzero against real Windows / Linux / macOS
guests, run from the macOS host that owns the Parallels VMs. The point is
coverage the unit/E2E suites can't give: real trust stores, real TUN/wintun
adapters, real DNS, and — via `vm-test-lifecycle` — the real installer
(install → use → **uninstall** → assert nothing is left behind), driven end to
end on a genuine OS.

> **Two different things, don't conflate them.** `vm-test` runs the overlay
> *smoke* (TUN + DNS + proxy) against the **raw binary** pushed into the guest —
> it does NOT install a package and says nothing about install/uninstall.
> `vm-test-lifecycle` installs the **real package** (Linux `.deb` via `dpkg`,
> Windows signed MSI via `msiexec`, macOS an offline `brew install` of a
> locally-generated formula **plus** the privileged `setup` flow the Homebrew
> formula's `post_install` + caveats run), verifies the CA / DNS / autostart /
> TUN artifacts landed, then runs the real uninstaller and asserts every one is
> GONE. `vm-test-upgrade` layers on prior-version→new-version upgrade dup-checks.
> If you want installer/uninstaller coverage, those are the recipes — see below.

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

## Running the e2e smoke test

`just vm-test <platform>` resets that VM to its `built` checkpoint and runs the
default local-overlay E2E (`vmtest/scripts/e2e-local-overlay.ps1`/`.sh`).
`just vm-test` with no argument runs windows, then linux, then macos in
series — `vm.sh` stops any other running VM before each reset, so only one
guest is ever up at a time.

```
just vm-test            # all three, in series
just vm-test windows
just vm-test linux
just vm-test macos
```

For anything other than the default script/checkpoint, use
`just vm-test-script "<vm>" vmtest/scripts/<name>.ps1 [checkpoint] [args...]`.

## Lifecycle test: install → use → UNINSTALL → assert-clean

`just vm-test-lifecycle [platform]` is the coverage a launch audit flagged as
missing: the whole download→install→use→**uninstall**→(upgrade) path against the
**real installer**, with every uninstall step *asserted*. Leftover trusted-CA or
DNS-resolver residue after uninstall is the loud-complaint bug class, so it is
proven gone, not assumed.

Per platform, `lifecycle-<os>.{sh,ps1}` install the real artifact, verify it
landed, run the real uninstaller, then assert the artifacts are removed:

| Platform | Install (real)                | Asserts landed → **then GONE**                                              |
|----------|-------------------------------|-----------------------------------------------------------------------------|
| Linux    | `dpkg -i` the built `.deb`    | binary, `setcap` caps, `/etc/hosts` pin, systemd unit; CA in system trust store + `/etc/ssl` + `ca-certificates.conf`; `~/.config` autostart unit |
| Windows  | `msiexec /i` the signed MSI   | binary, scheduled task, CA in `LocalMachine\Root`, `.portzero.local` NRPT rule, Wintun adapter |
| macOS    | **offline `brew install`** of a generated local formula, **then** privileged `portzero setup` | brew phase: binary linked onto PATH under the brew prefix + `post_install` CA generated + caveats surface `sudo portzero setup`, then `brew uninstall` removes it; setup phase: CA in System keychain, root LaunchDaemon, `/etc/resolver/portzero.local`, `/etc/hosts` pin — **then GONE** |

Each prints greppable `PHASE=<name> ok=<true|false>` lines and a final
`RESULT=PASS|FAIL`; a leftover artifact yields `ok=false` and fails the run. The
scripts read the artifact from where `vm-e2e.yml` stages it
(`vmtest/.downloaded-artifacts/<os>/`), or from `PORTZERO_DEB` / `PORTZERO_MSI`.

```
just vm-test-lifecycle            # all three, in series
just vm-test-lifecycle linux
```

**macOS delivery — what's real, what's not (honest scope).** macOS ships via
Homebrew (a release `.tar.gz` + tap formula), *not* a `.pkg`. `lifecycle-macos.sh`
now exercises the real install **mechanism** fully offline: it tars the host-built
binary into a `portzero-darwin-<arch>.tar.gz`, generates a formula whose `url` is
a `file://` path to it and whose `sha256` is computed from it — mirroring the
shipped formula's `install` / `post_install` / `caveats` blocks — then runs
`brew install --formula <generated.rb>` with all Homebrew network access disabled,
asserting the binary lands on PATH, `post_install` generated the CA, and the
caveats surface `sudo portzero setup`, then `brew uninstall` removes it. After
that it runs the privileged `sudo portzero setup` residue lifecycle (CA-trust /
LaunchDaemon / resolver / hosts) and asserts every artifact is removed.

Two honest caveats remain:
- **Homebrew-in-snapshot prerequisite.** The brew phase needs `brew` present in
  the golden macOS snapshot. If it is absent the phase SKIPs cleanly
  (`PHASE=brew-install ok=SKIP reason="Homebrew not installed in golden
  snapshot"`) and the privileged residue lifecycle still runs — the run never
  falsely fails.
- **Published tarball, not exercised here.** We install from a *local* file, so
  the *real published* GitHub URL + its formula `sha256` are **not** verified in
  the VM. That check lives in `release.yml` as a post-publish smoke (see below) —
  the one thing a network-pristine guest genuinely can't cover. There is no macOS
  `.pkg` build to test because none is shipped.

**Where each piece actually runs.** The Linux `.deb` package lifecycle
(postinst/prerm, hosts pin, `setcap`, unit) and the Linux upgrade dup-checks are
validated on hosted CI too (`ci.yml`: `lifecycle-upgrade-linux`, and the
`real_tun_overlay_trust_lifecycle` Rust e2e that install/verify/uninstalls the CA
on Linux). Windows MSI + macOS `brew`/setup lifecycles and the Windows/macOS
**upgrade** dup-checks run only on this Parallels runner (`vm-e2e.yml`), the only
place with a real MSI-capable Windows guest and a real macOS keychain/LaunchDaemon.
The **published** Homebrew formula's URL + sha256 are verified post-publish in
`release.yml` (`verify-homebrew-artifacts`) — see the upgrade/release notes below.

## Upgrade test: prior version → new version (all three platforms)

`upgrade-{linux,windows,macos}.{sh,ps1}` install a **prior** released version,
then the new one over the top, and assert the in-place upgrade DUPLICATED nothing:

| Platform | Prior install → new install | Asserts (exactly one of each; no side-by-side) |
|----------|-----------------------------|------------------------------------------------|
| Linux    | prior `.deb` → new `.deb` via `dpkg -i` | one `/etc/hosts` pin, one systemd unit (a postinst that appends instead of replacing is how you get two) |
| Windows  | prior MSI → new MSI via `msiexec /i`    | WiX `<MajorUpgrade>` replaced in place: one installed product with the `UpgradeCode`, one scheduled task, one `.portzero.local` NRPT rule, one Wintun adapter; installed binary is the new `ProductVersion` |
| macOS    | prior `setup` → new `setup`             | one `cloud.portzero.*` LaunchDaemon, one `/etc/resolver/portzero.local`, one System-keychain CA, one `/etc/hosts` pin; LaunchDaemon references the new binary |

The prior artifact is fetched from the **latest published release** via `gh`, or
pinned with `PORTZERO_DEB_OLD` / `PORTZERO_MSI_OLD` / `PORTZERO_EXE_OLD`. Each
script `SKIP`s cleanly (never a false pass) when no prior artifact is available —
before the first release, or a network-pristine guest without `gh`. The Windows
upgrade additionally SKIPs when the prior and new MSI carry the **same**
`ProductVersion` (`<MajorUpgrade>` needs a version bump). Run all three with
`just vm-test-upgrade` (wired into `vm-e2e.yml`); Linux also runs on hosted CI
(`ci.yml`: `lifecycle-upgrade-linux`).

## Release-time smoke: published Homebrew formula (url + sha256)

The one check a VM can't do offline lives in `release.yml`
(`verify-homebrew-artifacts`, post-publish): it checks out the pushed tap formula
(`PortZeroNetwork/homebrew-portzero`), and for each arch confirms the formula's
**real published** GitHub `url` actually resolves *and* its declared `sha256`
matches the downloaded tarball — catching the `0000…` placeholder `sha256` never
getting filled in, a formula whose version didn't get bumped, or a broken release
asset URL.

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
| `Windows Pro` | Windows 11   | real MSI install/uninstall, trust store, wintun, tunnels |
| `Ubuntu Linux`| Linux        | real `.deb` install/uninstall, CAP_NET_ADMIN, /dev/net/tun |
| `macOS 15.7.7`| macOS        | offline `brew install` of a generated local formula + privileged `setup`/teardown (CA keychain, LaunchDaemon, resolver), utun, upgrade dup-checks — no `.pkg` is shipped, so none is tested; see the macOS delivery scope above |

Snapshot convention is the same for all three: a golden powered-off baseline
plus a `"<vm>-ready"` running snapshot as the reset point.
