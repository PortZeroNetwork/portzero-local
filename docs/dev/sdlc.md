# Software delivery lifecycle

This repository uses a Heroku-style, tag-addressed release model (the same one
`portzero-cloud` uses):

- `staging` is the single long-lived integration branch and the default branch.
  Day-to-day work merges here via PR.
- There is **no** `release/*` branch. A stable **release is an immutable
  `vX.Y.Z` tag** — the release of record, browsable under Tags.
- Cutting a stable release is a deliberate, gated action: you dispatch the
  Release workflow and pick the version bump, then a required reviewer approves
  the `release` GitHub Environment. That stamps the tag, and the tag push builds
  and publishes the signed release.
- GitHub Actions CI runs on PRs into `staging` (see
  `.github/workflows/ci.yml`).

## Local validation before release

Run the local checks on `staging` before cutting a release:

```bash
git switch staging
git pull
just verify
just e2e
```

The pre-commit and pre-push hooks also run the fast local checks, but `just e2e`
is intentionally manual because it needs root or Administrator privileges.

## Cut a stable release

A stable release is cut by the gated **Release** workflow, which picks the bump
explicitly and pushes the `vX.Y.Z` tag from the `staging` tip:

1. Go to **Actions → Release → Run workflow**, with `staging` selected as the
   branch to run from.
2. Set **channel** to `release` and pick the **bump** (`patch` / `minor` /
   `major`).
3. Run it. The `promote` job pauses on the `release` environment gate until a
   **required reviewer** approves it — this is the release "button".
4. On approval, `promote` computes the next `vX.Y.Z` from the latest stable tag
   and pushes that tag from the `staging` tip.

The tag push triggers the same workflow's tag build (`push: tags: v*`), which
produces the **signed** stable artifacts and the GitHub Release. Inspect or
follow the runs from the **Actions** tab, or with the GitHub CLI:

```bash
gh run list --workflow Release
gh run watch
```

Because the version is decided at release time by the person cutting it, there
is no commit-message marker to remember and nothing to compute from the tip
commit — the bump you pick is the bump you get.

## Prereleases (edge channel)

A prerelease is cut off `staging` by dispatching the same workflow with
**channel** `edge` (the default). It is ungated and produces an **unsigned**
build tagged/marked `prerelease: true`, so it never touches the stable install
paths. See [prerelease-channel.md](prerelease-channel.md).
