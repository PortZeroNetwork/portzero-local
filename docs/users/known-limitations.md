# Known limitations: local CA trust

Port Zero installs a local CA (`portzero trust generate` + `portzero trust
install`, see `client/crates/daemon/src/tls/trust.rs`) into the OS trust store
and per-browser NSS databases so `*.portzero.local` works over HTTPS without
certificate warnings. That coverage is broad but not universal. This page
tracks the specific gaps found so far and the documented fallback for each.

## Snap-packaged Chromium-family browsers (Brave, Chromium) on Linux

**Symptom**: `net::ERR_CERT_AUTHORITY_INVALID` for `https://portzero.local` (or
any `*.portzero.local` name), specifically in the **Snap** package of Brave or
Chromium, while the native (non-Snap) package of the same browser works.

**Cause**: `portzero trust install` seeds the Snap revision's own
`~/snap/{brave,chromium}/<revision>/.pki/nssdb` NSS database (see
`find_nss_dbs` / `snap_chromium_nss_dbs` in `trust.rs`) and refreshes the
p11-kit compat bundles (`trust extract-compat`) that back the system trust
store. Some Snap Brave/Chromium builds still ignore both of those locally
installed trust anchors and report the PortZero CA as an unknown issuer
regardless — this is a Snap browser sandboxing/packaging quirk, not something
`portzero trust install` failed to do.

**Fallback**: use the native (non-Snap) package of the same browser, or a
different browser entirely. There is no `ignoreHTTPSErrors`-equivalent for
regular end-user browsing (that flag exists in a browser-automation context
like Playwright, not in Chrome/Brave's UI) — the workaround here is packaging
choice, not a flag.

See also [`troubleshooting.md`](troubleshooting.md#brave-shows-err_cert_authority_invalid-for-httpsportzerolocal).

## Playwright's bundled Firefox on Linux (task-66)

**Symptom**: a Playwright test navigating to `https://*.portzero.local` in the
`firefox` project fails with a certificate-trust error, while the same test in
the `chromium` and `webkit` projects succeeds, on a hosted `ubuntu-latest`
runner with `portzero trust install` already run.

**Cause**: NSS certificate databases are **per-profile** (a directory
containing `cert9.db`), not a single shared system database. `portzero trust
install`'s Linux NSS step (`install_nss_dbs` in `trust.rs`) certutil's:

- `~/.pki/nssdb` (the store shared by Chrome, Chromium, and native Brave —
  seeded proactively even before any such browser has run), and
- any **existing** Firefox profile directories under `~/.mozilla/firefox/*`
  (plus the Flatpak/Snap equivalents).

Playwright, however, launches its bundled Firefox against a **fresh, ephemeral
profile** created per test run (not a profile under `~/.mozilla/firefox`).
That profile does not exist at CA-install time, so it is never in the set of
directories `trust install` seeds, and the CA is untrusted there by
construction — this is not a bug in the CA-install logic, it is a mismatch
between "seed known profile directories" and "Playwright creates a new,
unknown one every run."

**Fallback (documented, in use)**: set `ignoreHTTPSErrors: true` for the
`firefox` project in Playwright config. This is what
[`testing/tls-verify/playwright.config.ts`](../../testing/tls-verify/playwright.config.ts)
does; see that project's README for the full per-engine expectation table and
the CI workflow (`.github/workflows/playwright-tls-verify.yml`) that exercises
it on a real runner.

**Coordination note for a real fix**: a genuine fix (seeding a *newly created*
Firefox profile automatically, e.g. via a policies.json enterprise root or a
`certutil`-seeded profile template `portzero trust install` could copy from)
would live in `client/crates/daemon/src/tls/trust.rs`, which is core CA-install
source owned by the daemon/discovery agent for this milestone — not changed
here. This document exists so that work has a concrete, reproduced starting
point instead of starting from a guess.

## Chromium and WebKit on GitHub's `ubuntu-latest` runner image

**Symptom**: on the first real run of `playwright-tls-verify.yml` (a
GitHub-hosted `ubuntu-latest` runner), Playwright's `chromium` and `webkit`
projects both failed to trust the PortZero local CA over `https://*.portzero.local`
— not just the already-documented `firefox` gap above. This contradicts the
expectation in `trust.rs`'s own comments (Chromium reads the shared
`~/.pki/nssdb`, which `portzero trust install` seeds proactively; WebKitGTK
verifies via GnuTLS against the system trust store).

**Cause (partially identified)**: the daemon's own log shows `trust
extract-compat` (p11-kit's compat-bundle refresh, which `portzero trust
install` runs) failing on this runner image:
`p11-kit: could not run /usr/libexec/p11-kit/trust-extract-compat command:
Unknown error 2`. That plausibly explains WebKit, whose trust path is
documented as depending on that refresh. It does not explain Chromium, whose
trust path is documented as depending only on `~/.pki/nssdb` (unaffected by
p11-kit) — Chromium's failure is unexplained and needs separate investigation
in `trust.rs`.

**Fallback (documented, in use)**: `ignoreHTTPSErrors: true` for the
`chromium` and `webkit` projects too, alongside `firefox`, in
[`testing/tls-verify/playwright.config.ts`](../../testing/tls-verify/playwright.config.ts) —
matching observed CI reality rather than the (currently incorrect) theoretical
expectation. `.github/workflows/playwright-tls-verify.yml` also installs
`p11-kit-modules` and runs a non-fatal diagnostic (`trust extract-compat`
directly, with output captured) to gather more signal on the
`trust-extract-compat` failure on future runs.

**Coordination note for a real fix**: same as the Firefox entry above — this
needs someone to actually reproduce `trust extract-compat`'s failure on a
`ubuntu-latest` runner (or an equivalent local container) and either find the
missing package/dependency or add a fallback path in
`client/crates/daemon/src/tls/trust.rs`. Not done here; this entry exists so
that work starts from reproduced evidence instead of a guess.

## What this page is not

This is not a general troubleshooting index — see
[`troubleshooting.md`](troubleshooting.md) for day-to-day failure modes
(`curl` can't resolve a tunnel name, `PZ_TUNNEL` set too late, etc.). This page
is specifically about browser/engine-level CA trust gaps that have an
identified cause and a documented workaround, kept here so they don't get
re-discovered from scratch.
