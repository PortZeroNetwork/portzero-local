# Software delivery lifecycle

This repository uses a lightweight Git Flow model:

- `develop` is the default branch for day-to-day integration.
- `release/current` is the release gate branch.
- GitHub Actions CI runs when `release/current` is updated.
- Publishing a GitHub Release is manual and runs from `release/current`.

## Local validation before release

Run the local checks before updating the release branch:

```bash
git switch develop
git pull
just verify
just e2e
```

The pre-commit and pre-push hooks also run the fast local checks, but `just e2e`
is intentionally manual because it needs root or Administrator privileges.

## Start release validation

Fast-forward `release/current` to the prepared `develop` tip and push it:

```bash
git switch release/current
git pull
git merge --ff-only develop
git push origin release/current
```

That push starts the CI workflow for `release/current`.

Inspect or follow the workflow runs with:

```bash
gh run list --branch release/current
gh run watch
```

## Publish a release manually

After CI passes on `release/current`, trigger the release workflow manually:

```bash
gh workflow run release.yml --ref release/current
```

Then inspect or follow the release workflow:

```bash
gh run list --workflow Release
gh run watch
```

The release workflow computes the next version from the tip commit message on
`release/current`:

- `BREAKING CHANGE` or `[major]` creates a major version bump.
- `feat(...):` or `[minor]` creates a minor version bump.
- Anything else creates a patch version bump.
