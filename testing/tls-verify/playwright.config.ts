import { defineConfig, devices } from "@playwright/test";

// The tunnel must already be up before `playwright test` runs: a plain HTTP
// backend behind PZ_TUNNEL=...portzero.local:443 (TLS terminated at the
// tunnel edge by the PortZero local CA). The CI workflow starts it and
// passes the resolved https:// URL in via PZ_TUNNEL_URL — see
// ../../.github/workflows/playwright-tls-verify.yml.
const url = process.env.PZ_TUNNEL_URL;
if (!url) {
  throw new Error(
    "PZ_TUNNEL_URL must be set to the https://*.portzero.local URL under test.",
  );
}
if (!url.startsWith("https://")) {
  throw new Error(`PZ_TUNNEL_URL must be an https:// URL, got: ${url}`);
}

export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  fullyParallel: true,
  retries: 0,
  reporter: [["list"], ["json", { outputFile: "results.json" }]],
  use: {
    baseURL: url,
  },
  projects: [
    // Chromium and WebKit are expected to trust the PortZero local CA out of
    // the box on a Linux runner:
    //  - Chromium reads the shared NSS store at ~/.pki/nssdb, which
    //    `portzero trust install` seeds proactively even before any browser
    //    has ever run (LinuxTrustEnv::provision_nss_db_dirs in
    //    client/crates/daemon/src/tls/trust.rs).
    //  - WebKitGTK (Playwright's Linux WebKit build) verifies certs against
    //    the system trust store via GnuTLS, which `update-ca-certificates`
    //    plus the p11-kit `trust extract-compat` refresh (also done by
    //    `portzero trust install`) populate.
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], ignoreHTTPSErrors: false },
    },
    {
      name: "webkit",
      use: { ...devices["Desktop Safari"], ignoreHTTPSErrors: false },
    },

    // Firefox is the known gap (see docs/known-limitations.md): NSS
    // databases are per-profile (a directory containing cert9.db), and
    // `portzero trust install` only certutil's *existing* profiles found
    // under ~/.mozilla/firefox/*. Playwright launches its bundled Firefox
    // against a fresh, ephemeral profile that does not exist at install
    // time and is never seeded, so the CA is not trusted there by default.
    // ignoreHTTPSErrors is the documented fallback.
    {
      name: "firefox",
      use: { ...devices["Desktop Firefox"], ignoreHTTPSErrors: true },
    },
  ],
});
