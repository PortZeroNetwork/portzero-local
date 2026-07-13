# Prerelease ("edge") channel

Full installers can be cut off `staging` for testing **without** any stable
user detecting, being offered, or receiving them. This is the edge channel.
(Edge builds ship **unsigned** — see below.)

## The one guarantee

Everything that could pull a stable user toward an update resolves through
GitHub's **`/releases/latest`** endpoint, and that endpoint **ignores releases
marked `prerelease: true`**. In this repo that covers:

- the in-CLI update check — `client/crates/cli/src/update.rs` fetches
  `releases/latest/download/version.json`;
- the Linux installer's default path — `scripts/linux-install.sh` downloads
  `releases/latest/download/...`;
- the stable Homebrew formula (`portzero`), only bumped by the stable release;
- the repo homepage "Latest release" widget.

So the whole design rests on one fact: **edge releases are marked
`prerelease: true`** (and `make_latest: false`). Stable is untouched by them.

Edge builds are built from unmodified `staging` source, so they connect to the
**production** portzero.cloud, exactly like stable — the staging *cloud* is only
ever used inside the release workflow's interop *tests*, never compiled into a
binary.

## Cutting an edge build

Run the **Release** workflow via *Actions → Release → Run workflow* off
`staging` with **channel** set to `edge` (the default). An edge
`workflow_dispatch` run always produces a prerelease:

- version `X.Y.Z-rc.<run-number>` (the `X.Y.Z` is the next stable version; the
  `-rc.N` SemVer suffix sorts *below* it, so it can never look newer than
  stable);
- a GitHub Release tagged `vX.Y.Z-rc.N` with `prerelease: true`,
  `make_latest: false`;
- the same full matrix as stable — macOS/Linux binaries, `.deb`, `.rpm`,
  Windows `.exe`/`.msi`, tarballs. **Windows artifacts are unsigned** on edge:
  the Azure Artifact Signing federated-identity credential only trusts the
  stable release tags (`refs/tags/v*`), not the `staging` dispatch ref, so all
  signing steps are skipped for prereleases (see below);
- the Homebrew **edge** formula (`portzero-edge`) bumped in the tap.

Stable releases are cut by pushing a `vX.Y.Z` tag (the gated `promote` job) and
are unaffected by edge runs — prerelease tags are excluded from the stable
version calculation. See [sdlc.md](sdlc.md#cut-a-stable-release).

## Installing an edge build (opt-in, per platform)

None of these are advertised in the README, on the website, or in stable
release notes. They are deliberately opt-in.

### macOS — Homebrew

A separate `portzero-edge` formula lives in the same tap. It conflicts with the
stable `portzero` formula (both ship the `portzero` binary), so install one at a
time:

```sh
brew tap PortZeroNetwork/portzero
brew install portzeronetwork/portzero/portzero-edge
```

Return to stable:

```sh
brew uninstall portzero-edge
brew install portzero
```

### Linux — install script

The single install script honors a `PORTZERO_CHANNEL` switch. Default is
`stable`; `prerelease` resolves the newest prerelease tag via the GitHub API
(needs `jq` or `python3`, both common on dev machines):

```sh
curl -fsSL https://github.com/PortZeroNetwork/portzero-local/releases/latest/download/linux-install.sh | PORTZERO_CHANNEL=prerelease sh
```

Pin an exact build (no API/parser needed):

```sh
curl -fsSL .../linux-install.sh | PORTZERO_VERSION=0.2.0-rc.7 PORTZERO_CHANNEL=prerelease sh
```

Reinstalling with no channel set returns the machine to stable on the next run.

### Windows — MSI

Download the `.msi` from the prerelease's GitHub Release page and run it.
winget is intentionally **not** used for edge (winget has no prerelease lane, so
only stable is ever submitted there).

Edge Windows artifacts are **unsigned** (see above), so SmartScreen / Defender
will warn on first run — choose *More info → Run anyway*. Signing is authorized
by the Azure federated-identity credential, whose subject is pegged to the
stable release tags (`refs/tags/v*`). If an edge build genuinely needs to be
signed for testing, add the `staging` ref (`refs/heads/staging`) to that
credential's subject instead of relying on the edge channel — but prefer cutting
a real stable tag.

## Why these choices

- **`prerelease: true` is the linchpin** — it is what keeps `latest`, the CLI
  update check, `brew upgrade`, and `curl | sh` away from edge builds.
- **`-rc.N` SemVer suffix** — orders below the eventual stable version for both
  GitHub and the CLI's own comparator (`update.rs`), so an edge user is never
  falsely told a *stable* update is available, and stable users never see edge.
- **Separate `portzero-edge` formula** (not `portzero@edge`) — a plain formula
  links `portzero` onto `PATH` normally; a versioned `@`-formula would be
  keg-only and require manual linking.
- **Excluding `-` tags from stable versioning** — without it, a `v0.2.0-rc.1`
  tag would sort into the stable `git tag` query and corrupt the next stable
  bump.
